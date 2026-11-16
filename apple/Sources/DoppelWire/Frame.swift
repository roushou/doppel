// Length-prefixed framing: [u32 little-endian payload length][payload].
//
// Sans-IO and behaviourally identical to `crates/doppel-protocol/src/frame.rs`:
// push bytes in chunks of any size, pull complete frames. The golden fixtures
// in /fixtures/wire pin both implementations to the same bytes.

import Foundation
import SwiftProtobuf

/// Largest accepted payload. Bounds memory a peer can make us allocate.
public struct MaxFrameLen: Sendable, Equatable {
    public let bytes: UInt32
    public init(bytes: UInt32) { self.bytes = bytes }
    /// 64 MiB, as in Rust.
    public static let `default` = MaxFrameLen(bytes: 64 << 20)
}

/// The undecoded payload of one frame. Kept so nothing a peer sends is lost.
public struct RawFrame: Sendable, Equatable {
    public let payload: Data
}

/// A top-level frame type that can tell whether it understood its payload.
public protocol Envelope: SwiftProtobuf.Message {
    var isRecognized: Bool { get }
}

extension Doppel_Agent_V1_AgentFrame: Envelope {
    public var isRecognized: Bool { kind != nil }
}

extension Doppel_Vmm_V1_VmmFrame: Envelope {
    public var isRecognized: Bool { kind != nil }
}

public enum Received<M: Envelope> {
    case recognized(M, raw: RawFrame)
    /// Well-formed protobuf of a kind this build doesn't know (a newer peer).
    case unrecognized(raw: RawFrame)
    /// Frame boundary intact, payload invalid. The stream stays usable.
    case malformed(raw: RawFrame, error: any Error)
}

/// Errors after which the byte stream can't be resynchronised.
public enum StreamError: Error, Equatable, Sendable {
    case frameTooLarge(length: UInt32, limit: UInt32)
}

public enum EncodeError: Error, Equatable, Sendable {
    case tooLarge(length: Int, limit: UInt32)
}

public struct FrameDecoder<M: Envelope> {
    private enum State {
        case reading
        /// Terminal: the same error is returned forever.
        case failed(StreamError)
    }

    private var buffer: [UInt8] = []
    private var start = 0
    private let limit: MaxFrameLen
    private var state = State.reading

    public init(limit: MaxFrameLen = .default) { self.limit = limit }

    public var pendingBytes: Int { buffer.count - start }

    public mutating func push(_ bytes: some Sequence<UInt8>) {
        switch state {
        case .reading: buffer.append(contentsOf: bytes)
        case .failed: break
        }
    }

    /// The next complete frame, `nil` if more bytes are needed.
    public mutating func nextFrame() throws(StreamError) -> Received<M>? {
        switch state {
        case .failed(let error): throw error
        case .reading: break
        }
        guard pendingBytes >= 4 else { return nil }
        let length = UInt32(buffer[start]) | UInt32(buffer[start + 1]) << 8
            | UInt32(buffer[start + 2]) << 16 | UInt32(buffer[start + 3]) << 24
        guard length <= limit.bytes else {
            let error = StreamError.frameTooLarge(length: length, limit: limit.bytes)
            state = .failed(error)
            buffer = []; start = 0
            throw error
        }
        let total = 4 + Int(length)
        guard pendingBytes >= total else { return nil }
        let raw = RawFrame(payload: Data(buffer[(start + 4)..<(start + total)]))
        start += total
        compactIfWorthwhile()
        do {
            let message = try M(serializedBytes: raw.payload)
            return message.isRecognized ? .recognized(message, raw: raw) : .unrecognized(raw: raw)
        } catch {
            return .malformed(raw: raw, error: error)
        }
    }

    /// Drops consumed bytes once they dominate the buffer: amortised O(1) per byte.
    private mutating func compactIfWorthwhile() {
        if start == buffer.count { buffer.removeAll(keepingCapacity: true); start = 0 }
        else if start > 64 * 1024 && start * 2 > buffer.count { buffer.removeFirst(start); start = 0 }
    }
}

public struct FrameEncoder: Sendable {
    public let limit: MaxFrameLen
    public init(limit: MaxFrameLen = .default) { self.limit = limit }

    public func encode(_ message: some SwiftProtobuf.Message) throws -> Data {
        let payload: Data = try message.serializedBytes()
        guard let length = UInt32(exactly: payload.count), length <= limit.bytes else {
            throw EncodeError.tooLarge(length: payload.count, limit: limit.bytes)
        }
        var out = Data(capacity: 4 + payload.count)
        withUnsafeBytes(of: length.littleEndian) { out.append(contentsOf: $0) }
        out.append(payload)
        return out
    }
}
