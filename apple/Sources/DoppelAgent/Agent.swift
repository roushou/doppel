// The agent's request handling: status, screen capture and input, from inside
// the guest's GUI session. Each operation checks the TCC permission it needs
// and reports a typed PermissionMissing instead of failing obscurely.

import AppKit
import ApplicationServices
import CoreGraphics
import DoppelWire

typealias Outcome = Doppel_Agent_V1_AgentResponse.OneOf_Outcome

@MainActor
public final class Agent {
    private let ax = AxSnapshotter()

    public init() {}

    public static let capabilities: [Doppel_Common_V1_Capability] = [.screenshot, .input, .axSnapshot, .axObserve, .axAction, .exec, .files, .events]

    public func handle(_ operation: Doppel_Agent_V1_AgentRequest.OneOf_Operation?) async -> Doppel_Agent_V1_AgentResponse.OneOf_Outcome {
        do {
            switch operation {
            case .getStatus:
                return .status(status())
            case .captureScreen(let request):
                return .image(try await ScreenCapture.capture(request))
            case .input(let input):
                try GuestInput.send(input)
                return .done(.init())
            case .takeAxSnapshot(let request):
                return .axSnapshot(try ax.snapshot(request))
            case .performAxAction(let request):
                try ax.perform(request)
                return .done(.init())
            case .exec(let request):
                return .execResult(try await GuestExec.run(request))
            case .readFile(let request):
                return .fileContents(try GuestFiles.read(request))
            case .writeFile(let request):
                try GuestFiles.write(request)
                return .done(.init())
            case .subscribe:
                // A connection's own business: AgentConnection answers it.
                return .error(.with { $0.invalidState.reason = "subscribe belongs to a connection" })
            case nil:
                return .error(.with { $0.invalidArgument = .with { $0.field = "operation"; $0.reason = "missing or unknown to this agent" } })
            }
        } catch let error as AgentError {
            return .error(error.wire)
        } catch {
            return .error(.with { $0.internal.message = String(describing: error) })
        }
    }

    /// Asks macOS for the agent's permissions. Shows the system prompts the first time.
    /// The app bundle's build id (Contents/Resources/build-id); empty for a bare binary.
    public static let buildID: String = {
        guard Bundle.main.bundleIdentifier == "dev.doppel.agent",
              let url = Bundle.main.url(forResource: "build-id", withExtension: nil),
              let text = try? String(contentsOf: url, encoding: .utf8) else { return "" }
        return text.trimmingCharacters(in: .whitespacesAndNewlines)
    }()

    public static func requestPermissions() {
        _ = AXIsProcessTrustedWithOptions(["AXTrustedCheckOptionPrompt": true] as CFDictionary)
        _ = CGRequestScreenCaptureAccess()
    }

    public static var grantedPermissions: [Doppel_Common_V1_Permission] {
        var granted: [Doppel_Common_V1_Permission] = []
        if CGPreflightScreenCaptureAccess() { granted.append(.screenRecording) }
        if AXIsProcessTrusted() { granted.append(.accessibility) }
        return granted
    }

    func status() -> Doppel_Agent_V1_AgentStatus {
        .with {
            $0.granted = Self.grantedPermissions
            $0.session = Self.session
            let bounds = CGDisplayBounds(CGMainDisplayID())
            $0.displayPoints = .with { $0.width = bounds.width; $0.height = bounds.height }
            if let app = NSWorkspace.shared.frontmostApplication {
                $0.frontmostApp = .with {
                    $0.pid = app.processIdentifier
                    $0.bundleID = app.bundleIdentifier ?? ""
                    $0.name = app.localizedName ?? ""
                }
            }
        }
    }

    private static var session: Doppel_Agent_V1_SessionState {
        guard let session = CGSessionCopyCurrentDictionary() as? [String: Any] else { return .unspecified }
        // "CGSSessionScreenIsLocked" has no public constant; it's absent when unlocked.
        let locked = session["CGSSessionScreenIsLocked"] as? Bool ?? false
        let onConsole = session[kCGSessionOnConsoleKey as String] as? Bool ?? false
        return onConsole && !locked ? .active : .locked
    }
}

enum AgentError: Error {
    case permissionMissing(Doppel_Common_V1_Permission)
    case invalidArgument(field: String, reason: String)
    case unsupported(String)
    case staleReference(snapshotID: UInt64, nodeIndex: UInt32)
    case invalidState(String)
    case failed(String)

    var wire: Doppel_Common_V1_Error {
        switch self {
        case .permissionMissing(let permission): .with { $0.permissionMissing.permission = permission }
        case .invalidArgument(let field, let reason): .with { $0.invalidArgument = .with { $0.field = field; $0.reason = reason } }
        case .unsupported(let what): .with { $0.unsupported.what = what }
        case .staleReference(let snapshotID, let nodeIndex):
            .with { $0.staleReference = .with { $0.snapshotID = snapshotID; $0.nodeIndex = nodeIndex } }
        case .invalidState(let reason): .with { $0.invalidState.reason = reason }
        case .failed(let message): .with { $0.internal.message = message }
        }
    }
}
