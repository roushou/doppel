import Darwin
import Foundation
import Testing
import DoppelWire
@testable import DoppelVMM

/// A client on the server's socket that greets it and sends requests.
private struct Peer {
    let fd: Int32

    init(path: String) throws {
        fd = socket(AF_UNIX, SOCK_STREAM, 0)
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        withUnsafeMutableBytes(of: &address.sun_path) { buffer in
            for (i, byte) in path.utf8.enumerated() { buffer[i] = byte }
        }
        let connected = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard connected == 0 else { throw ControlServerError.system("connect", errno) }
        var hello = Doppel_Vmm_V1_VmmFrame()
        hello.hello = .with { $0.protocolVersion = 1 }
        try send(hello)
    }

    func request(id: UInt64) throws {
        var frame = Doppel_Vmm_V1_VmmFrame()
        frame.request = .with { $0.id = id }
        try send(frame)
    }

    private func send(_ frame: Doppel_Vmm_V1_VmmFrame) throws {
        let bytes = [UInt8](try FrameEncoder().encode(frame))
        let written = bytes.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
        guard written == bytes.count else { throw ControlServerError.system("write", errno) }
    }

    func close() { Darwin.close(fd) }
}

/// Polls `condition` for up to two seconds.
private func eventually(_ condition: () -> Bool) -> Bool {
    for _ in 0..<200 {
        if condition() { return true }
        usleep(10_000)
    }
    return false
}

@Suite struct ControlServerFrames {
    @Test func aDisplayConnectionThatJoinsAsksForTheScreenAsItIs() throws {
        let path = NSTemporaryDirectory() + "doppel-cs-\(getpid()).sock"
        let server = try ControlServer(path: path)
        defer { server.removeSocket() }
        // Request 1 is a plain one; request 2 opens the display.
        server.serve(hello: .with { $0.protocolVersion = 1 }) { _ in
            Reply(.done(.init()), then: Self.opening.take() ? .streamDisplay : .nothing)
        }
        let peer = try Peer(path: path)
        defer { peer.close() }

        try peer.request(id: 1)
        usleep(100_000)
        #expect(!server.takeFrameRequest(), "only a display connection asks")

        Self.opening.arm()
        try peer.request(id: 2)
        #expect(eventually { server.displayClients == 1 })
        #expect(server.takeFrameRequest(), "the new display connection asks for a frame")
        #expect(!server.takeFrameRequest(), "asking clears it")
    }

    /// Whether the next reply opens the display.
    private static let opening = Flag()

    private final class Flag: @unchecked Sendable {
        private let lock = NSLock()
        private var set = false
        func arm() { lock.withLock { set = true } }
        func take() -> Bool { lock.withLock { defer { set = false }; return set } }
    }
}
