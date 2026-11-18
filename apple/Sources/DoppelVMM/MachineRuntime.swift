// One running VM and the requests that drive it.

import AppKit
import Foundation
import Virtualization
import DoppelWire

public enum Boot: Sendable {
    case coldBoot
    /// The first boot after an install: macOS creates this account itself
    /// (macOS 27 guests and hosts; earlier guests ignore it).
    case provision(Provisioning)
    /// Restore, then delete the file before the guest runs: a memory state
    /// must never be paired with a disk that has moved on.
    case consumeState(URL)
}

public enum WindowMode: Sendable {
    case hidden
    case shown

    var wire: Doppel_Vmm_V1_WindowMode {
        switch self {
        case .hidden: .hidden
        case .shown: .shown
        }
    }
}

public enum RuntimeError: Error, CustomStringConvertible {
    case consumeFailed(URL, Int32)

    public var description: String {
        switch self {
        case .consumeFailed(let url, let code):
            "restored \(url.path) but could not delete it (\(String(cString: strerror(code)))); stopping so it can't be restored twice"
        }
    }
}

@MainActor
public final class MachineRuntime: NSObject, VZVirtualMachineDelegate {
    let vm: VZVirtualMachine
    let view: VZVirtualMachineView
    let window: NSWindow
    /// Guest display size in points: the coordinate space of Input.
    let displayPoints: CGSize
    let displaySize: (width: Int, height: Int)
    /// Created when the first client opens the display.
    private var display: DisplayShare?
    private let server: ControlServer
    private var stateObservation: NSKeyValueObservation?
    /// How `run` shows the VM; reported with its state, so a restart can
    /// show it the same way.
    private var windowMode = WindowMode.hidden
    /// Handed-over guest connections. VZ closes a connection when its object
    /// is released, so they're kept alive for the VM's lifetime.

    public init(files: MachineFiles, socketPath: String) throws {
        let spec = try files.readSpec()
        vm = VZVirtualMachine(configuration: try .doppel(spec: spec, files: files))
        displayPoints = spec.display.points
        displaySize = (Int(spec.display.widthPixels), Int(spec.display.heightPixels))
        server = try ControlServer(path: socketPath)
        // The view exists even when hidden: host input and capture go through it.
        view = VZVirtualMachineView(frame: NSRect(origin: .zero, size: displayPoints))
        view.capturesSystemKeys = true
        window = NSWindow(contentRect: view.frame, styleMask: [.titled, .miniaturizable, .resizable], backing: .buffered, defer: false)
        super.init()
        view.virtualMachine = vm
        window.contentView = view
        window.title = "Doppel · \(files.spec.deletingLastPathComponent().lastPathComponent)"
        window.isReleasedWhenClosed = false
        vm.delegate = self
    }

    public func run(boot: Boot, window mode: WindowMode) async throws {
        windowMode = mode
        switch mode {
        case .hidden: break
        case .shown:
            NSApp.setActivationPolicy(.regular)
            window.center(); window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
        }
        stateObservation = vm.observe(\.state, options: [.new]) { [weak self] vm, _ in
            let state = vm.state
            MainActor.assumeIsolated {
                var event = Doppel_Vmm_V1_VmmEvent()
                event.stateChanged = .with { $0.runState = state.wire }
                self?.server.broadcast(event)
            }
        }
        var hello = Doppel_Common_V1_Hello()
        hello.protocolVersion = WireVersion.vmm
        hello.softwareVersion = "doppel-vmm 0.1.0"
        hello.capabilities = [.checkpoint, .hostInput, .screenshot, .displayStream, .recording]
        // Serve first, so a failing start is observable by clients.
        server.serve(hello: hello) { [weak self] operation in
            guard let self else { return Reply(.error(.invalidState("VMM shutting down"))) }
            return await self.handle(operation)
        }
        switch boot {
        case .coldBoot:
            try await vm.start()
        case .provision(let provisioning):
            try await vm.start(options: try provisioning.startOptions())
        case .consumeState(let url):
            try await vm.restoreMachineStateFrom(url: url)
            guard unlink(url.path) == 0 else {
                let code = errno
                try? await vm.stop()
                throw RuntimeError.consumeFailed(url, code)
            }
            try await vm.resume()
        }
        log("VM running (\(vm.state.wire))")
    }

    // MARK: Requests

    func handle(_ operation: Doppel_Vmm_V1_VmmRequest.OneOf_Operation?) async -> Reply {
        do {
            switch operation {
            case .getState:
                return Reply(.state(.with {
                    $0.runState = vm.state.wire
                    $0.window = windowMode.wire
                }))
            case .pause:
                try await vm.pause()
                return Reply(.done(.init()))
            case .resume:
                try await vm.resume()
                return Reply(.done(.init()))
            case .stop(let stop):
                switch stop.mode {
                case .graceful:
                    try vm.requestStop()
                case .force:
                    try await vm.stop()
                    finish(after: .forced)
                case .unspecified, .UNRECOGNIZED:
                    return Reply(.error(.invalidArgument("mode", "unknown stop mode")))
                }
                return Reply(.done(.init()))
            case .saveState(let save):
                let url = URL(fileURLWithPath: save.path)
                guard vm.state == .paused else { return Reply(.error(.invalidState("the VM must be paused to save its state"))) }
                guard !FileManager.default.fileExists(atPath: url.path) else {
                    return Reply(.error(.invalidArgument("path", "\(save.path) already exists")))
                }
                try await vm.saveMachineStateTo(url: url)
                let size = (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? UInt64) ?? 0
                return Reply(.stateSaved(.with { $0.bytes = size }))
            case .hostInput(let input):
                try send(input)
                return Reply(.done(.init()))
            case .captureDisplay(let capture):
                return Reply(.image(try captureDisplay(maxDimension: Int(capture.maxDimensionPixels))))
            case .sampleDisplay(let sample):
                let color = try sampleDisplay(at: sample.at)
                return Reply(.displaySample(.with { $0.color = color }))
            case .recognizeText:
                guard let image = try displayPixels().cgImage else { throw HostInputError.captureFailed }
                let points = displayPoints
                // Seconds of work: off the main actor, which serves the view and the channel.
                let text = try await Task.detached { try TextRecognizer.recognize(image, displayPoints: points) }.value
                return Reply(.recognizedText(text))
            case .openAgentChannel(let open):
                guard let device = vm.socketDevices.first as? VZVirtioSocketDevice else {
                    return Reply(.error(.unsupported("this VM has no vsock device")))
                }
                let connection: VZVirtioSocketConnection
                do {
                    connection = try await device.connect(toPort: open.vsockPort)
                } catch {
                    return Reply(.error(.invalidState("nothing accepted vsock port \(open.vsockPort) in the guest (\(error.localizedDescription))")))
                }
                // The reply owns a duplicate; `connection` closes its own copy when released.
                let passed = dup(connection.fileDescriptor)
                guard passed >= 0 else { return Reply(.error(.internal("dup: \(String(cString: strerror(errno)))"))) }
                return Reply(.agentChannelOpened(.with { $0.vsockPort = open.vsockPort }), passing: passed)
            case .openDisplay:
                let share = try displayShare()
                let fd = try share.descriptor()
                share.start()
                return Reply(.displayOpened(.with { $0.layout = share.layout }), passing: fd, then: .streamDisplay)
            case .openEncodedDisplay:
                let share = try displayShare()
                share.start()
                let layout = share.layout
                return Reply(.encodedDisplayOpened(.with {
                    $0.widthPixels = layout.widthPixels
                    $0.heightPixels = layout.heightPixels
                    $0.displayPoints = layout.displayPoints
                }), then: .streamEncoded)
            case .startRecording(let start):
                return Reply(.recordingStarted(try displayShare().startRecording(path: start.path)))
            case .stopRecording(let stop):
                guard let display else { throw RecordingError.notRecording(stop.hasPath ? stop.path : nil) }
                return Reply(.recordingStopped(try await display.stopRecording(path: stop.hasPath ? stop.path : nil)))
            case nil:
                return Reply(.error(.unsupported("operation unknown to this VMM")))
            }
        } catch let error as RecordingError {
            return Reply(.error(error.wire))
        } catch let error as HostInputError {
            return Reply(.error(.invalidArgument("input", error.description)))
        } catch {
            return Reply(.error(.internal(String(describing: error))))
        }
    }

    /// The display's frames: shared with consoles and recorded. Made once.
    private func displayShare() throws -> DisplayShare {
        if let display { return display }
        let share = try DisplayShare(view: view, server: server, pixels: displaySize, points: displayPoints)
        display = share
        return share
    }

    // MARK: Lifecycle

    private func finish(after reason: Doppel_Vmm_V1_StopReason, error: String = "") {
        var event = Doppel_Vmm_V1_VmmEvent()
        event.guestStopped = .with { $0.reason = reason; $0.errorDescription = error }
        server.broadcast(event)
        server.removeSocket()
        log("VM stopped (\(reason))\(error.isEmpty ? "" : ": \(error)")")
        let display = display
        Task { @MainActor in
            // A recording ends with the VM: complete its file first.
            if let stopped = try? await display?.stopRecording(path: nil) {
                log("recorded \(stopped.path): \(stopped.frames) frames, \(Int(stopped.seconds)) s")
            }
            // Let the reply to a forced stop flush before the process goes.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { exit(reason == .error ? 1 : 0) }
        }
    }

    nonisolated public func guestDidStop(_ virtualMachine: VZVirtualMachine) {
        MainActor.assumeIsolated { finish(after: .guestShutdown) }
    }

    nonisolated public func virtualMachine(_ virtualMachine: VZVirtualMachine, didStopWithError error: Error) {
        let description = String(describing: error)
        MainActor.assumeIsolated { finish(after: .error, error: description) }
    }
}

func log(_ message: String) {
    FileHandle.standardError.write(Data("\(Date().ISO8601Format()) \(message)\n".utf8))
}

extension VZVirtualMachine.State {
    var wire: Doppel_Vmm_V1_VmRunState {
        switch self {
        case .stopped: .stopped
        case .running: .running
        case .paused: .paused
        case .error: .error
        case .starting: .starting
        case .pausing: .pausing
        case .resuming: .resuming
        case .stopping: .stopping
        case .saving: .saving
        case .restoring: .restoring
        @unknown default: .unspecified
        }
    }
}

extension Doppel_Common_V1_Error {
    static func invalidState(_ reason: String) -> Self { .with { $0.invalidState.reason = reason } }
    static func invalidArgument(_ field: String, _ reason: String) -> Self {
        .with { $0.invalidArgument.field = field; $0.invalidArgument.reason = reason }
    }
    static func unsupported(_ what: String) -> Self { .with { $0.unsupported.what = what } }
    static func `internal`(_ message: String) -> Self { .with { $0.internal.message = message } }
}
