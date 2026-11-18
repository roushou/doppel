// The agent channel's transport: a listener (vsock in a VM's guest, or a
// unix socket on this Mac) speaking framed AgentFrames. One thread per
// connection, requests answered in order. The agent speaks first (Hello),
// like the VMM.

import Darwin
import Foundation
import DoppelWire

public enum AgentServerError: Error, Equatable, CustomStringConvertible {
    case system(String, Int32)
    /// Another agent already listens at this path.
    case alreadyServed(UnixSocketPath)
    /// Something that isn't a socket is at this path; it is left alone.
    case notASocket(UnixSocketPath)

    public var description: String {
        switch self {
        case .system(let call, let code): "\(call): \(String(cString: strerror(code)))"
        case .alreadyServed(let path): "another agent is listening at \(path)"
        case .notASocket(let path): "\(path) exists and isn't a socket; remove it or choose another path"
        }
    }
}

public final class AgentServer: Sendable {
    public typealias Handler = @Sendable (Doppel_Agent_V1_AgentRequest.OneOf_Operation?) async -> Doppel_Agent_V1_AgentResponse.OneOf_Outcome

    public let listener: Listener
    private let socket: Int32

    public init(listening listener: Listener) throws(AgentServerError) {
        self.listener = listener
        socket = try listener.bind()
    }

    deinit { close(socket) }

    /// Serves connections; those that subscribe get `events`' pushes.
    public func serve(hello: Doppel_Common_V1_Hello, events: EventHub, handler: @escaping Handler) {
        let socket = self.socket
        let listener = self.listener
        Thread {
            while true {
                let fd = accept(socket, nil, nil)
                if fd < 0 { if errno == EINTR || errno == ECONNABORTED { continue }; return }
                guard listener.admits(fd) else { close(fd); continue }
                Thread { AgentConnection(fd: fd, events: events).run(hello: hello, handler: handler) }.start()
            }
        }.start()
    }
}

final class AgentConnection: @unchecked Sendable {
    private let fd: Int32
    private let events: EventHub
    private var decoder = FrameDecoder<Doppel_Agent_V1_AgentFrame>()
    /// Replies and pushed events share the socket: one writer at a time,
    /// and none once the connection has closed it (its number may be reused).
    private let writing = NSLock()
    private var closed = false
    private var outbox: EventOutbox?

    /// Pushed events are paced, so a UI in constant motion (a progress bar)
    /// costs at most this many frames a second.
    static let eventsPerSecond = 20.0

    init(fd: Int32, events: EventHub) {
        self.fd = fd
        self.events = events
        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
    }

    func run(hello: Doppel_Common_V1_Hello, handler: @escaping AgentServer.Handler) {
        defer {
            if let outbox { events.unsubscribe(outbox); outbox.close() }
            writing.lock(); closed = true; close(fd); writing.unlock()
        }
        guard send(.with { $0.hello = hello }) else { return }
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        var greeted = false
        while true {
            let frame: Received<Doppel_Agent_V1_AgentFrame>?
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
            case .hello(let peer):
                guard peer.protocolVersion == WireVersion.agent else { return }
                greeted = true
            case .request(let request) where greeted:
                if case .subscribe = request.operation {
                    subscribe()
                    guard send(.with { $0.response.id = request.id; $0.response.done = .init() }) else { return }
                    continue
                }
                let outcome = Self.await(watching: fd) { await handler(request.operation) }
                guard send(.with {
                    $0.response.id = request.id
                    $0.response.outcome = outcome
                }) else { return }
            case .request, .response, .event, nil:
                return
            }
        }
    }

    /// Bridges this blocking connection thread to async work on other actors.
    /// The host sends nothing while it waits for a reply, so a hang-up meanwhile
    /// means it gave up: the work is cancelled (an exec stops its program).
    private static func await(watching fd: Int32, _ work: @escaping @Sendable () async -> Doppel_Agent_V1_AgentResponse.OneOf_Outcome) -> Doppel_Agent_V1_AgentResponse.OneOf_Outcome {
        let done = DispatchSemaphore(value: 0)
        let box = Box()
        let task = Task { box.value = await work(); done.signal() }
        let watch = HangUpWatch(fd: fd) { task.cancel() }
        done.wait()
        watch.end()
        return box.value ?? .error(.with { $0.internal.message = "no reply" })
    }

    private final class Box: @unchecked Sendable { var value: Doppel_Agent_V1_AgentResponse.OneOf_Outcome? }

    /// Starts pushing events to this connection, from a thread of its own
    /// so a host that isn't reading holds up only its own pushes.
    private func subscribe() {
        guard outbox == nil else { return }
        let outbox = EventOutbox()
        self.outbox = outbox
        events.subscribe(outbox)
        Thread.detachNewThread { [self] in
            while let pending = outbox.take() {
                for event in pending {
                    guard send(.with { $0.event = event }) else { outbox.close(); return }
                }
                Thread.sleep(forTimeInterval: 1 / Self.eventsPerSecond)
            }
        }
    }

    private func send(_ frame: Doppel_Agent_V1_AgentFrame) -> Bool {
        guard let data = try? FrameEncoder().encode(frame) else { return false }
        writing.lock()
        defer { writing.unlock() }
        guard !closed else { return false }
        return data.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return false }
            var offset = 0
            while offset < raw.count {
                let n = write(fd, base + offset, raw.count - offset)
                if n < 0 { if errno == EINTR { continue }; return false }
                offset += n
            }
            return true
        }
    }
}

/// Watches a connection, while a request runs, for the peer hanging up.
/// Data from the peer is not a hang-up: the watch ends and leaves it unread.
final class HangUpWatch: @unchecked Sendable {
    private let lock = NSLock()
    private var ended = false

    init(fd: Int32, onHangUp: @escaping @Sendable () -> Void) {
        Thread.detachNewThread { [self] in
            var watched = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
            while !isEnded {
                let ready = poll(&watched, 1, 100)
                if ready < 0 && errno == EINTR { continue }
                if ready < 0 { return }
                if ready == 0 { continue }
                var byte: UInt8 = 0
                let n = recv(fd, &byte, 1, MSG_PEEK | MSG_DONTWAIT)
                if n < 0 && (errno == EINTR || errno == EAGAIN) { continue }
                if n <= 0, !isEnded { onHangUp() }
                return
            }
        }
    }

    /// Stops watching. A hang-up after this is the connection's own business.
    func end() { lock.lock(); ended = true; lock.unlock() }

    private var isEnded: Bool { lock.lock(); defer { lock.unlock() }; return ended }
}
