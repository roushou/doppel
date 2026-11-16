// MachineSpec → VZVirtualMachineConfiguration.
//
// Deterministic by construction: the same spec always yields the same
// configuration, MAC address included. VZ refuses to restore a saved state into
// a configuration that differs from the one it was saved with.

import Foundation
import Virtualization
import DoppelWire

public enum ConfigurationError: Error, Equatable, CustomStringConvertible {
    case notInstalled
    case invalidHardwareModel
    case invalidMachineIdentifier
    case unsupportedHardwareModel
    case invalidMACAddress(String)
    case missingNetwork
    case unsupportedAccess(name: String)

    public var description: String {
        switch self {
        case .notInstalled: "macOS is not installed in this machine yet"
        case .invalidHardwareModel: "the spec's hardware model blob is not valid"
        case .invalidMachineIdentifier: "the spec's machine identifier blob is not valid"
        case .unsupportedHardwareModel: "this Mac cannot run the machine's hardware model"
        case .invalidMACAddress(let mac): "invalid MAC address \(mac)"
        case .missingNetwork: "the spec has no network setting"
        case .unsupportedAccess(let name): "shared directory \(name) has an unknown access mode"
        }
    }
}

/// Explicit file locations. The VMM never derives paths from a bundle layout.
public struct MachineFiles: Sendable {
    public let spec: URL
    public let disk: URL
    public let aux: URL

    public init(spec: URL, disk: URL, aux: URL) {
        self.spec = spec; self.disk = disk; self.aux = aux
    }

    public func readSpec() throws -> Doppel_Bundle_V1_MachineSpec {
        try Doppel_Bundle_V1_MachineSpec(serializedBytes: Data(contentsOf: spec))
    }

    /// Atomic: a crash leaves the old spec or the new one.
    public func writeSpec(_ machineSpec: Doppel_Bundle_V1_MachineSpec) throws {
        let data: Data = try machineSpec.serializedBytes()
        try data.write(to: spec, options: .atomic)
    }
}

extension Doppel_Bundle_V1_Display {
    /// Points per pixel the guest uses: 2 for Retina densities.
    var scale: Int { pixelsPerInch >= 200 ? 2 : 1 }
    var points: CGSize { CGSize(width: Int(widthPixels) / scale, height: Int(heightPixels) / scale) }
}

extension VZVirtualMachineConfiguration {
    /// The one place a VM's devices are decided.
    @MainActor
    public static func doppel(spec: Doppel_Bundle_V1_MachineSpec, files: MachineFiles) throws -> VZVirtualMachineConfiguration {
        guard !spec.hardwareModel.isEmpty, !spec.machineIdentifier.isEmpty else { throw ConfigurationError.notInstalled }
        guard let hardware = VZMacHardwareModel(dataRepresentation: spec.hardwareModel) else { throw ConfigurationError.invalidHardwareModel }
        guard hardware.isSupported else { throw ConfigurationError.unsupportedHardwareModel }
        guard let identifier = VZMacMachineIdentifier(dataRepresentation: spec.machineIdentifier) else { throw ConfigurationError.invalidMachineIdentifier }

        let config = VZVirtualMachineConfiguration()
        config.bootLoader = VZMacOSBootLoader()
        config.cpuCount = Int(spec.cpuCount)
        config.memorySize = spec.memoryBytes

        let platform = VZMacPlatformConfiguration()
        platform.hardwareModel = hardware
        platform.machineIdentifier = identifier
        platform.auxiliaryStorage = VZMacAuxiliaryStorage(url: files.aux)
        config.platform = platform

        config.storageDevices = [VZVirtioBlockDeviceConfiguration(
            attachment: try VZDiskImageStorageDeviceAttachment(url: files.disk, readOnly: false))]

        let graphics = VZMacGraphicsDeviceConfiguration()
        graphics.displays = [VZMacGraphicsDisplayConfiguration(
            widthInPixels: Int(spec.display.widthPixels),
            heightInPixels: Int(spec.display.heightPixels),
            pixelsPerInch: Int(spec.display.pixelsPerInch))]
        config.graphicsDevices = [graphics]

        // Host input goes through these via VZVirtualMachineView.
        config.keyboards = [VZMacKeyboardConfiguration()]
        config.pointingDevices = [VZUSBScreenCoordinatePointingDeviceConfiguration(), VZMacTrackpadConfiguration()]
        config.entropyDevices = [VZVirtioEntropyDeviceConfiguration()]
        config.socketDevices = [VZVirtioSocketDeviceConfiguration()]

        switch spec.network.attachment {
        case .nat(let nat):
            guard let mac = VZMACAddress(string: nat.macAddress) else { throw ConfigurationError.invalidMACAddress(nat.macAddress) }
            let network = VZVirtioNetworkDeviceConfiguration()
            network.attachment = VZNATNetworkDeviceAttachment()
            network.macAddress = mac
            config.networkDevices = [network]
        case .disconnected:
            config.networkDevices = []
        case nil:
            throw ConfigurationError.missingNetwork
        }

        if !spec.sharedDirectories.isEmpty {
            var shares: [String: VZSharedDirectory] = [:]
            for share in spec.sharedDirectories {
                let readOnly: Bool
                switch share.access {
                case .readOnly: readOnly = true
                case .readWrite: readOnly = false
                case .unspecified, .UNRECOGNIZED: throw ConfigurationError.unsupportedAccess(name: share.name)
                }
                shares[share.name] = VZSharedDirectory(url: URL(fileURLWithPath: share.hostPath), readOnly: readOnly)
            }
            let device = VZVirtioFileSystemDeviceConfiguration(tag: VZVirtioFileSystemDeviceConfiguration.macOSGuestAutomountTag)
            device.share = VZMultipleDirectoryShare(directories: shares)
            config.directorySharingDevices = [device]
        }

        try config.validate()
        try config.validateSaveRestoreSupport()
        return config
    }
}
