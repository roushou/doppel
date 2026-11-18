// doppel-agent: runs in a GUI session and serves the agent channel: on
// vsock inside a VM's guest, or on a unix socket on this Mac. Installed in a
// guest as `Doppel Agent.app` with a LaunchAgent (`doppel agent install`),
// or run as a bare binary from Terminal.
//
// `doppel-agent --help` lists its options; Listener.swift parses them.

import AppKit
import DoppelAgent
import DoppelWire

let listener: Listener
do { listener = try Listener.parse(arguments: Array(CommandLine.arguments.dropFirst())) } catch {
    Listener.exit(withError: error)
}

// A write to a closed socket or pipe must fail with EPIPE, never kill the agent:
// a SIGPIPE takes every tool down until launchd relaunches it (10 s or more).
// Sockets and exec's stdin also opt out one by one; this covers any other write.
signal(SIGPIPE, SIG_IGN)

let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
Agent.requestPermissions()

let server: AgentServer
do { server = try AgentServer(listening: listener) } catch {
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
print("doppel-agent \(Agent.buildID.isEmpty ? "(bare binary)" : "build \(Agent.buildID)") listening on \(listener); granted: \(granted.isEmpty ? "none" : granted.map { "\($0)" }.joined(separator: ", "))")
fflush(stdout)
app.run()
