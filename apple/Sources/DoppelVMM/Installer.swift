// `doppel-vmm install`: put macOS into a new machine.
//
// Progress and the outcome go to stdout as framed VmmFrame events, so the
// caller parses them with the same codec as the control socket.

import Foundation
import Virtualization
import DoppelWire

public enum InstallError: Error, CustomStringConvertible {
    case alreadyInstalled
    case unsupportedImage
    case diskExists(URL)

    public var description: String {
        switch self {
        case .alreadyInstalled: "the spec already has a hardware model; an installed machine is never reinstalled"
        case .unsupportedImage: "this restore image is not supported on this Mac"
        case .diskExists(let url): "\(url.path) already exists and will not be overwritten"
        }
    }
}

@MainActor
public struct Installer {
    let files: MachineFiles
    let restoreImage: URL

    public init(files: MachineFiles, restoreImage: URL) {
        self.files = files; self.restoreImage = restoreImage
    }

    public func run() async {
        do {
            try await install()
            emit(.with { $0.installFinished.done = .init() })
        } catch {
            emit(.with { $0.installFinished.error = .internal(String(describing: error)) })
        }
    }

    private func install() async throws {
        var spec = try files.readSpec()
        guard spec.hardwareModel.isEmpty else { throw InstallError.alreadyInstalled }
        let image = try await VZMacOSRestoreImage.image(from: restoreImage)
        guard let requirements = image.mostFeaturefulSupportedConfiguration, requirements.hardwareModel.isSupported else {
            throw InstallError.unsupportedImage
        }
        // O_EXCL: never truncate an existing disk. The new one is sparse.
        let fd = open(files.disk.path, O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC, 0o600)
        guard fd >= 0 else { throw InstallError.diskExists(files.disk) }
        let disk = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
        try disk.truncate(atOffset: spec.diskBytes)
        try disk.close()
        _ = try VZMacAuxiliaryStorage(creatingStorageAt: files.aux, hardwareModel: requirements.hardwareModel, options: [])

        spec.hardwareModel = requirements.hardwareModel.dataRepresentation
        spec.machineIdentifier = VZMacMachineIdentifier().dataRepresentation
        let vm = VZVirtualMachine(configuration: try .doppel(spec: spec, files: files))
        let installer = VZMacOSInstaller(virtualMachine: vm, restoringFromImageAt: restoreImage)
        let observation = installer.progress.observe(\.fractionCompleted, options: [.new]) { progress, _ in
            let fraction = progress.fractionCompleted
            MainActor.assumeIsolated { emit(.with { $0.installProgress.fraction = fraction }) }
        }
        defer { observation.invalidate() }
        try await installer.install()
        // Only now is the machine's identity real.
        try files.writeSpec(spec)
    }

    private func emit(_ event: Doppel_Vmm_V1_VmmEvent) {
        var frame = Doppel_Vmm_V1_VmmFrame()
        frame.event = event
        if let data = try? FrameEncoder().encode(frame) { FileHandle.standardOutput.write(data) }
    }
}
