// doppel-agent: runs inside the guest's GUI session and serves the agent
// channel on vsock. Installed as `Doppel Agent.app` with a LaunchAgent
// (`doppel agent install`), or run as a bare binary from Terminal.
//
//   doppel-agent [--port 7400]

import AppKit
import DoppelAgent
import DoppelWire

var port = AgentServer.defaultPort
var arguments = CommandLine.arguments.dropFirst()
while let argument = arguments.popFirst() {
    switch argument {
    case "--port":
        guard let raw = arguments.popFirst(), let parsed = UInt32(raw) else {
            FileHandle.standardError.write(Data("doppel-agent: --port expects a number\n".utf8)); exit(64)
        }
        port = parsed
    default:
        FileHandle.standardError.write(Data("doppel-agent: unknown argument \(argument)\nusage: doppel-agent [--port N]\n".utf8)); exit(64)
    }
}

// A write to a closed socket or pipe must fail with EPIPE, never kill the agent:
// a SIGPIPE takes every tool down until launchd relaunches it (10 s or more).
// Sockets and exec's stdin also opt out one by one; this covers any other write.
signal(SIGPIPE, SIG_IGN)

let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
Agent.requestPermissions()

let server: AgentServer
do { server = try AgentServer(port: port) } catch {
    FileHandle.standardError.write(Data("doppel-agent: \(error)\n".utf8)); exit(1)
}
let agent = MainActor.assumeIsolated { Agent() }
let events = EventHub()
let watcher = MainActor.assumeIsolated { UiWatcher(hub: events) }
server.serve(hello: .with {
    $0.protocolVersion = WireVersion.agent
    $0.softwareVersion = "doppel-agent 0.1.0"
    $0.buildID = Agent.buildID
    $0.capabilities = Agent.capabilities
}, events: events) { operation in await agent.handle(operation) }

let granted = MainActor.assumeIsolated { Agent.grantedPermissions }
print("doppel-agent \(Agent.buildID.isEmpty ? "(bare binary)" : "build \(Agent.buildID)") listening on vsock port \(port); granted: \(granted.isEmpty ? "none" : granted.map { "\($0)" }.joined(separator: ", "))")
fflush(stdout)
app.run()
