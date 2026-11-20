import Testing
import Darwin
import Foundation
import DoppelWire
@testable import DoppelAgent

@Test func theCommandLineNamesOneListener() throws {
    #expect(try Listener.parse(arguments: []) == .vsock(port: 7400))
    #expect(try Listener.parse(arguments: ["--port", "9"]) == .vsock(port: 9))
    #expect(try Listener.parse(arguments: ["--socket", "/tmp/a.sock"]) == .unix(try UnixSocketPath("/tmp/a.sock")))

}

@Test(arguments: [
    ["--port", "9", "--socket", "/tmp/a.sock"],  // two listeners
    ["--port", "x"],
    ["--port", "-1"],
    ["--socket"],
    ["--socket", "a.sock"],                      // relative
    ["--sockets", "/tmp/a"],
])
func theCommandLineRefusesAnythingElse(arguments: [String]) {
    #expect(throws: (any Error).self) { try Listener.parse(arguments: arguments) }
}

@Test func aSocketPathMustFitItsAddress() throws {
    let longest = "/" + String(repeating: "a", count: UnixSocketPath.maxBytes - 1)
    #expect(try UnixSocketPath(longest).path == longest)
    #expect(throws: UnixSocketPathError.tooLong(longest + "a", max: UnixSocketPath.maxBytes)) { try UnixSocketPath(longest + "a") }
}

@Test func aUnixSocketIsOwnerOnlyAndSpeaksFirst() throws {
    try withSocketPath { path in
        let server = try AgentServer(listening: .unix(path))
        server.serve(hello: .with { $0.protocolVersion = WireVersion.agent }, events: EventHub()) { _ in .done(.init()) }

        var info = stat()
        #expect(lstat(path.path, &info) == 0)
        #expect(info.st_mode & S_IFMT == S_IFSOCK)
        #expect(info.st_mode & 0o777 == 0o600)

        let peer = try Peer(connectingTo: path)
        defer { close(peer.fd) }
        #expect(try peer.next()?.hello.protocolVersion == WireVersion.agent)
        try peer.send(.with { $0.hello.protocolVersion = WireVersion.agent })
        try peer.send(.with { $0.request.id = 7; $0.request.getStatus = .init() })
        let reply = try peer.next()
        #expect(reply?.response.id == 7)
        #expect(reply?.response.done != nil)
    }
}

@Test func aSocketLeftByAnAgentThatExitedIsReplaced() throws {
    try withSocketPath { path in
        // An agent that exited leaves its socket file behind, with no listener.
        // Made here by binding without listening: a listening socket closed
        // in this process can stay alive a moment while another test's
        // posix_spawn holds a copy of it.
        let left = socket(AF_UNIX, SOCK_STREAM, 0)
        var address = path.address
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(left, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        #expect(bound == 0)
        close(left)
        var info = stat()
        #expect(lstat(path.path, &info) == 0)

        let server = try AgentServer(listening: .unix(path))
        server.serve(hello: .with { $0.protocolVersion = WireVersion.agent }, events: EventHub()) { _ in .done(.init()) }
        let peer = try Peer(connectingTo: path)
        defer { close(peer.fd) }
        #expect(try peer.next()?.hello.protocolVersion == WireVersion.agent)
    }
}

@Test func aLiveAgentsSocketIsNotTakenOver() throws {
    try withSocketPath { path in
        let first = try AgentServer(listening: .unix(path))
        #expect(throws: AgentServerError.alreadyServed(path)) { try AgentServer(listening: .unix(path)) }
        _ = first
    }
}

@Test func aFileThatIsntASocketIsLeftAlone() throws {
    try withSocketPath { path in
        #expect(FileManager.default.createFile(atPath: path.path, contents: Data("keep".utf8)))
        #expect(throws: AgentServerError.notASocket(path)) { try AgentServer(listening: .unix(path)) }
        #expect(FileManager.default.contents(atPath: path.path) == Data("keep".utf8))
    }
}

@Test func bindingLeavesOnlyTheSocketBeside() throws {
    try withSocketPath { path in
        _ = try AgentServer(listening: .unix(path))
        let parent = (path.path as NSString).deletingLastPathComponent
        #expect(try FileManager.default.contentsOfDirectory(atPath: parent) == [(path.path as NSString).lastPathComponent])
    }
}

@Test func bindingLeavesOtherThreadsFileModesAlone() throws {
    // Binding once changed the process's umask for a moment, so a directory
    // made on another thread then could come out unusable.
    try withSocketPath { path in
        let parent = (path.path as NSString).deletingLastPathComponent
        let expected = Int(0o777 & ~currentUmask())
        let made = DispatchGroup()
        let modes = Modes()
        DispatchQueue.global().async(group: made) {
            for n in 0..<400 {
                let directory = "\(parent)/d\(n)"
                mkdir(directory, 0o777)
                var info = stat()
                if stat(directory, &info) == 0 { modes.add(Int(info.st_mode & 0o777)) }
                rmdir(directory)
            }
        }
        for n in 0..<200 {
            let socket = try UnixSocketPath("\(parent)/s\(n)")
            _ = try AgentServer(listening: .unix(socket))
            unlink(socket.path)
        }
        made.wait()
        #expect(modes.all.allSatisfy { $0 == expected }, "modes seen: \(Set(modes.all))")
    }
}

/// The process's umask, read without changing it for long: only this
/// test's thread reads it, before anything else runs in the test.
private func currentUmask() -> mode_t {
    let mask = umask(0o022)
    umask(mask)
    return mask
}

private final class Modes: @unchecked Sendable {
    private let lock = NSLock()
    private var seen: [Int] = []
    func add(_ mode: Int) { lock.withLock { seen.append(mode) } }
    var all: [Int] { lock.withLock { seen } }
}

/// A fresh socket path in a directory of its own, removed afterwards. The
/// directory is under /tmp: the user's temporary directory can make a path
/// too long for a socket.
private func withSocketPath(_ body: (UnixSocketPath) throws -> Void) throws {
    let directory = "/tmp/doppel-agent-test-\(getpid())-\(UUID().uuidString.prefix(8))"
    try FileManager.default.createDirectory(atPath: directory, withIntermediateDirectories: false)
    defer { try? FileManager.default.removeItem(atPath: directory) }
    try body(try UnixSocketPath(directory + "/agent.sock"))
}

/// doppeld's side of a unix socket connection: frames in, frames out.
private final class Peer {
    let fd: Int32
    private var decoder = FrameDecoder<Doppel_Agent_V1_AgentFrame>()

    struct ConnectFailed: Error { let code: Int32 }

    init(connectingTo path: UnixSocketPath) throws {
        fd = socket(AF_UNIX, SOCK_STREAM, 0)
        var address = path.address
        let connected = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard connected == 0 else { let code = errno; close(fd); throw ConnectFailed(code: code) }
        var timeout = timeval(tv_sec: 2, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
    }

    func send(_ frame: Doppel_Agent_V1_AgentFrame) throws {
        let data = try FrameEncoder().encode(frame)
        _ = data.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
    }

    /// The next frame, or nil after 2 s without one.
    func next() throws -> Doppel_Agent_V1_AgentFrame? {
        var buffer = [UInt8](repeating: 0, count: 4096)
        while true {
            if case .recognized(let frame, _) = try decoder.nextFrame() { return frame }
            let n = read(fd, &buffer, buffer.count)
            guard n > 0 else { return nil }
            decoder.push(buffer[0..<n])
        }
    }
}
