// The vmm channel's transport: a unix socket server speaking framed VmmFrames.
//
// Blocking I/O on background threads. Requests are handed to a handler and
// answered on the same connection. Events are broadcast to every connection,
// except display frames: those go only to connections that opened the
// display, latest first, and encoded frames, which go in order to
// connections that opened the encoded display. A response may carry one
// file descriptor as SCM_RIGHTS ancillary data.

import Darwin
import Foundation
import DoppelWire

public enum ControlServerError: Error, CustomStringConvertible {
    case socketPathTooLong(String)
    case system(String, Int32)

    public var description: String {
        switch self {
        case .socketPathTooLong(let path): "socket path too long for sockaddr_un: \(path)"
        case .system(let call, let code): "\(call): \(String(cString: strerror(code)))"
        }
    }
}

/// What a request produced: a response, optionally with a descriptor to pass.
/// The reply owns `passing`: the server closes it once sent (or not sendable).
public struct Reply: Sendable {
    public var outcome: Doppel_Vmm_V1_VmmResponse.OneOf_Outcome
    public var passing: Int32?
    public var then: AfterReply

    public init(_ outcome: Doppel_Vmm_V1_VmmResponse.OneOf_Outcome, passing: Int32? = nil, then: AfterReply = .nothing) {
        self.outcome = outcome; self.passing = passing; self.then = then
    }
}

/// What the connection does once the reply is sent.
public enum AfterReply: Sendable {
    case nothing
    /// Receive display frames from now on.
    case streamDisplay
    /// Receive encoded frames from now on, starting at a key frame.
    case streamEncoded
}

public final class ControlServer: @unchecked Sendable {
    public typealias Handler = @Sendable (Doppel_Vmm_V1_VmmRequest.OneOf_Operation?) async -> Reply

    private let path: String
    private let listener: Int32
    private let lock = NSLock()
    private var connections: [Connection] = []
    private var feeds: [DisplayFeed] = []
    private var encodedFeeds: [EncodedFeed] = []
    /// A connection needs a key frame: one joined, or one fell behind.
    private var keyFrameWanted = false
    /// A display connection joined: it needs the screen as it is now, even
    /// if it hasn't changed since the last frame others were sent.
    private var frameWanted = false

    public init(path: String) throws {
        self.path = path
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        guard bytes.count < MemoryLayout.size(ofValue: address.sun_path) else { throw ControlServerError.socketPathTooLong(path) }
        withUnsafeMutableBytes(of: &address.sun_path) { buffer in
            for (i, byte) in bytes.enumerated() { buffer[i] = byte }
        }
        unlink(path) // A stale socket from a VMM that died.
        listener = socket(AF_UNIX, SOCK_STREAM, 0)
        guard listener >= 0 else { throw ControlServerError.system("socket", errno) }
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(listener, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard bound == 0 else { throw ControlServerError.system("bind", errno) }
        chmod(path, 0o600)
        guard listen(listener, 8) == 0 else { throw ControlServerError.system("listen", errno) }
    }

    deinit { close(listener) }

    public func removeSocket() { unlink(path) }

    public func serve(hello: Doppel_Common_V1_Hello, handler: @escaping Handler) {
        Thread { [self] in
            while true {
                let fd = accept(listener, nil, nil)
                if fd < 0 { if errno == EINTR { continue }; return }
                let connection = Connection(fd: fd)
                Thread { [self] in
                    var frame = Doppel_Vmm_V1_VmmFrame()
                    frame.hello = hello
                    // Hello must be the first frame: register for broadcasts only
                    // once it's sent, under the lock broadcasts take.
                    let greeted = lock.withLock {
                        guard connection.send(frame) else { return false }
                        connections.append(connection)
                        return true
                    }
                    var feed: DisplayFeed?
                    var encoded: EncodedFeed?
                    if greeted {
                        connection.run(handler: handler) { [self] stream in
                            switch stream {
                            case .nothing:
                                break
                            case .streamDisplay:
                                guard feed == nil else { return }
                                let opened = DisplayFeed(connection: connection)
                                feed = opened
                                lock.withLock { feeds.append(opened); frameWanted = true }
                            case .streamEncoded:
                                guard encoded == nil else { return }
                                let opened = EncodedFeed(connection: connection) { [weak self] in self?.requestKeyFrame() }
                                encoded = opened
                                lock.withLock { encodedFeeds.append(opened); keyFrameWanted = true }
                            }
                        }
                    }
                    if let feed {
                        lock.withLock { feeds.removeAll { $0 === feed } }
                        feed.close()
                    }
                    if let encoded {
                        lock.withLock { encodedFeeds.removeAll { $0 === encoded } }
                        encoded.close()
                    }
                    connection.close()
                    lock.withLock { connections.removeAll { $0 === connection } }
                }.start()
            }
        }.start()
    }

    public func broadcast(_ event: Doppel_Vmm_V1_VmmEvent) {
        var frame = Doppel_Vmm_V1_VmmFrame()
        frame.event = event
        for connection in lock.withLock({ connections }) { _ = connection.send(frame) }
    }

    /// Connections receiving display frames.
    public var displayClients: Int { lock.withLock { feeds.count } }

    /// Offers a frame to every display connection; one that hasn't taken the
    /// last frame yet gets this one instead. Never blocks.
    public func publish(_ displayFrame: Doppel_Vmm_V1_DisplayFrame) {
        for feed in lock.withLock({ feeds }) { feed.offer(displayFrame) }
    }

    /// Connections receiving encoded frames.
    public var encodedClients: Int { lock.withLock { encodedFeeds.count } }

    /// Queues an encoded frame for every encoded connection. Never blocks.
    public func publishEncoded(_ frame: Doppel_Vmm_V1_EncodedFrame) {
        for feed in lock.withLock({ encodedFeeds }) { feed.offer(frame) }
    }

    func requestKeyFrame() { lock.withLock { keyFrameWanted = true } }

    /// Whether a display connection joined since last asked; asking clears it.
    public func takeFrameRequest() -> Bool {
        lock.withLock {
            defer { frameWanted = false }
            return frameWanted
        }
    }

    /// Whether the next frame encoded should be a key frame; asking clears it.
    public func takeKeyFrameRequest() -> Bool {
        lock.withLock {
            defer { keyFrameWanted = false }
            return keyFrameWanted
        }
    }
}

/// Sends encoded frames to one connection from a thread of its own, in
/// order: each depends on the ones before it. A reader too far behind loses
/// what is queued and gets nothing until the next key frame, which the feed
/// asks for, so a slow viewer never stalls the VMM or grows its memory.
final class EncodedFeed: @unchecked Sendable {
    /// About a second of a changing screen, or a few key frames.
    static let maxQueuedFrames = 60
    static let maxQueuedBytes = 4 << 20

    private let send: (Doppel_Vmm_V1_EncodedFrame) -> Bool
    private let wantKeyFrame: @Sendable () -> Void
    private let condition = NSCondition()
    private var queue: [Doppel_Vmm_V1_EncodedFrame] = []
    private var queuedBytes = 0
    /// Dropping frames until a key frame: at the start, and after a backlog.
    private var awaitingKey = true
    private var closed = false

    /// A feed sending each frame with `send`, which returns false once the
    /// connection is gone.
    init(send: @escaping (Doppel_Vmm_V1_EncodedFrame) -> Bool, wantKeyFrame: @escaping @Sendable () -> Void) {
        self.send = send
        self.wantKeyFrame = wantKeyFrame
        Thread { [self] in run() }.start()
    }

    fileprivate convenience init(connection: Connection, wantKeyFrame: @escaping @Sendable () -> Void) {
        self.init(send: { frame in
            var message = Doppel_Vmm_V1_VmmFrame()
            message.event.encodedFrame = frame
            return connection.send(message)
        }, wantKeyFrame: wantKeyFrame)
    }

    func offer(_ frame: Doppel_Vmm_V1_EncodedFrame) {
        let behind: Bool = condition.withLock {
            guard !closed else { return false }
            if awaitingKey {
                guard frame.key else { return false }
                awaitingKey = false
            }
            if queue.count >= Self.maxQueuedFrames || queuedBytes + frame.data.count > Self.maxQueuedBytes {
                queue.removeAll()
                queuedBytes = 0
                // The key frame that ends this wait may be this one.
                awaitingKey = !frame.key
                guard frame.key else { return true }
            }
            queue.append(frame)
            queuedBytes += frame.data.count
            condition.signal()
            return false
        }
        if behind { wantKeyFrame() }
    }

    func close() {
        condition.withLock { closed = true; condition.signal() }
    }

    private func run() {
        while true {
            let next: Doppel_Vmm_V1_EncodedFrame? = condition.withLock {
                while queue.isEmpty && !closed { condition.wait() }
                guard !closed else { return nil }
                let first = queue.removeFirst()
                queuedBytes -= first.data.count
                return first
            }
            guard let next, send(next) else { return }
        }
    }
}

/// Sends display frames to one connection from a thread of its own, so a
/// slow reader skips frames instead of stalling the VMM.
private final class DisplayFeed: @unchecked Sendable {
    private let connection: Connection
    private let condition = NSCondition()
    private var latest: Doppel_Vmm_V1_DisplayFrame?
    private var closed = false

    init(connection: Connection) {
        self.connection = connection
        Thread { [self] in run() }.start()
    }

    func offer(_ frame: Doppel_Vmm_V1_DisplayFrame) {
        condition.withLock { latest = frame; condition.signal() }
    }

    func close() {
        condition.withLock { closed = true; condition.signal() }
    }

    private func run() {
        while true {
            let next: Doppel_Vmm_V1_DisplayFrame? = condition.withLock {
                while latest == nil && !closed { condition.wait() }
                defer { latest = nil }
                return closed ? nil : latest
            }
            guard let next else { return }
            var frame = Doppel_Vmm_V1_VmmFrame()
            frame.event.displayFrame = next
            guard connection.send(frame) else { return }
        }
    }
}

private final class Connection: @unchecked Sendable {
    private let fd: Int32
    private let writeLock = NSLock()
    private var decoder = FrameDecoder<Doppel_Vmm_V1_VmmFrame>()

    init(fd: Int32) {
        self.fd = fd
        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
    }

    func close() { Darwin.close(fd) }

    /// Reads requests until the peer goes away. The peer's Hello is checked
    /// for protocol compatibility; requests are answered in order.
    func run(handler: @escaping ControlServer.Handler, then follow: (AfterReply) -> Void) {
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        var greeted = false
        while true {
            let frame: Received<Doppel_Vmm_V1_VmmFrame>?
            do { frame = try decoder.nextFrame() } catch { return }
            guard let frame else {
                let n = read(fd, &buffer, buffer.count)
                if n < 0 && errno == EINTR { continue }
                guard n > 0 else { return }
                decoder.push(buffer[0..<n])
                continue
            }
            guard case .recognized(let message, _) = frame else { return }
            switch message.kind {
            case .hello(let hello):
                guard hello.protocolVersion == 1 else { return }
                greeted = true
            case .request(let request) where greeted:
                let reply = Self.await { await handler(request.operation) }
                var response = Doppel_Vmm_V1_VmmFrame()
                response.response.id = request.id
                response.response.outcome = reply.outcome
                let sent = send(response, passing: reply.passing)
                if let fd = reply.passing { Darwin.close(fd) }
                guard sent else { return }
                follow(reply.then)
            case .request, .response, .event, nil:
                return
            }
        }
    }

    /// Bridges this blocking connection thread to async work on other actors.
    private static func await(_ work: @escaping @Sendable () async -> Reply) -> Reply {
        let done = DispatchSemaphore(value: 0)
        let box = Box()
        Task { box.value = await work(); done.signal() }
        done.wait()
        return box.value ?? Reply(.error(.internal("no reply")))
    }

    private final class Box: @unchecked Sendable { var value: Reply? }

    /// Writes one frame. When `passing` is set, the descriptor rides with the
    /// first chunk as SCM_RIGHTS, so it arrives with this frame's bytes.
    @discardableResult
    func send(_ frame: Doppel_Vmm_V1_VmmFrame, passing: Int32? = nil) -> Bool {
        guard let data = try? FrameEncoder().encode(frame) else { return false }
        return writeLock.withLock {
            let bytes = [UInt8](data)
            var offset = 0
            var descriptor = passing
            while offset < bytes.count {
                let sent = bytes[offset...].withUnsafeBytes { chunk -> Int in
                    var iov = iovec(iov_base: UnsafeMutableRawPointer(mutating: chunk.baseAddress), iov_len: chunk.count)
                    var message = msghdr()
                    message.msg_iov = withUnsafeMutablePointer(to: &iov) { $0 }
                    message.msg_iovlen = 1
                    guard let fd = descriptor else { return sendmsg(self.fd, &message, 0) }
                    return Self.withRights(fd) { control, length in
                        message.msg_control = control
                        message.msg_controllen = length
                        return sendmsg(self.fd, &message, 0)
                    }
                }
                if sent < 0 { if errno == EINTR { continue }; return false }
                offset += sent
                descriptor = nil
            }
            return true
        }
    }

    /// A control buffer holding one SCM_RIGHTS descriptor. Swift has no CMSG_*
    /// macros; Darwin aligns cmsg data to 4 bytes.
    private static func withRights(_ fd: Int32, _ body: (UnsafeMutableRawPointer, socklen_t) -> Int) -> Int {
        func align(_ n: Int) -> Int { (n + 3) & ~3 }
        let header = align(MemoryLayout<cmsghdr>.size)
        let length = header + MemoryLayout<Int32>.size
        let space = header + align(MemoryLayout<Int32>.size)
        let buffer = UnsafeMutableRawPointer.allocate(byteCount: space, alignment: 4)
        defer { buffer.deallocate() }
        buffer.initializeMemory(as: UInt8.self, repeating: 0, count: space)
        let cmsg = buffer.assumingMemoryBound(to: cmsghdr.self)
        cmsg.pointee.cmsg_len = socklen_t(length)
        cmsg.pointee.cmsg_level = SOL_SOCKET
        cmsg.pointee.cmsg_type = SCM_RIGHTS
        buffer.storeBytes(of: fd, toByteOffset: header, as: Int32.self)
        return body(buffer, socklen_t(space))
    }
}
