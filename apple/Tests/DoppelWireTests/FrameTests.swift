import Foundation
import Testing
@testable import DoppelWire

typealias AgentFrame = Doppel_Agent_V1_AgentFrame

private func typeText(_ id: UInt64, _ text: String) -> AgentFrame {
    var frame = AgentFrame()
    frame.request.id = id
    frame.request.input.typeText.text = text
    return frame
}

private func drain<M>(_ decoder: inout FrameDecoder<M>) throws -> [M] {
    var out: [M] = []
    while let frame = try decoder.nextFrame() {
        guard case .recognized(let message, _) = frame else { Issue.record("unexpected \(frame)"); continue }
        out.append(message)
    }
    return out
}

/// Deterministic generator so failures reproduce.
private struct SplitMix64: RandomNumberGenerator {
    var state: UInt64
    mutating func next() -> UInt64 {
        state &+= 0x9E37_79B9_7F4A_7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }
}

@Suite struct FrameCodec {
    @Test func roundTripsOneFrame() throws {
        let frame = typeText(7, "héllo 👋")
        var decoder = FrameDecoder<AgentFrame>()
        decoder.push(try FrameEncoder().encode(frame))
        #expect(try drain(&decoder) == [frame])
        #expect(decoder.pendingBytes == 0)
    }

    @Test(arguments: 0..<200)
    func outputIsIndependentOfChunking(seed: UInt64) throws {
        var rng = SplitMix64(state: seed)
        let frames = (0..<Int.random(in: 1...12, using: &rng)).map { i in
            typeText(UInt64(i), String((0..<Int.random(in: 0...64, using: &rng)).map { _ in
                "aé😀\n/".randomElement(using: &rng)!
            }))
        }
        let stream = try frames.flatMap { try FrameEncoder().encode($0) }
        var cuts = (0..<Int.random(in: 0...24, using: &rng)).map { _ in Int.random(in: 0...stream.count, using: &rng) }
        cuts += [0, stream.count]; cuts.sort()

        var decoder = FrameDecoder<AgentFrame>()
        var decoded: [AgentFrame] = []
        for (a, b) in zip(cuts, cuts.dropFirst()) {
            decoder.push(stream[a..<b])
            decoded += try drain(&decoder)
        }
        #expect(decoded == frames)
        #expect(decoder.pendingBytes == 0)
    }

    @Test func oversizedFrameIsFatalAndStaysFatal() throws {
        var decoder = FrameDecoder<AgentFrame>(limit: MaxFrameLen(bytes: 16))
        decoder.push(withUnsafeBytes(of: UInt32(17).littleEndian) { Array($0) })
        #expect(throws: StreamError.frameTooLarge(length: 17, limit: 16)) { try decoder.nextFrame() }
        decoder.push(try FrameEncoder().encode(typeText(1, "x")))
        #expect(throws: StreamError.frameTooLarge(length: 17, limit: 16)) { try decoder.nextFrame() }
        #expect(decoder.pendingBytes == 0)
    }

    @Test func unknownVariantFromNewerPeerIsKept() throws {
        // AgentFrame field 99, length-delimited, payload "new".
        let payload: [UInt8] = [0x9A, 0x06, 0x03] + Array("new".utf8)
        var decoder = FrameDecoder<AgentFrame>()
        decoder.push(withUnsafeBytes(of: UInt32(payload.count).littleEndian) { Array($0) } + payload)
        guard case .unrecognized(let raw) = try decoder.nextFrame() else { Issue.record("expected unrecognized"); return }
        #expect(raw.payload == Data(payload))
    }

    @Test func malformedPayloadDoesNotPoisonTheStream() throws {
        var decoder = FrameDecoder<AgentFrame>()
        decoder.push([2, 0, 0, 0, 0xFF, 0xFF])
        decoder.push(try FrameEncoder().encode(typeText(2, "after")))
        guard case .malformed = try decoder.nextFrame() else { Issue.record("expected malformed"); return }
        #expect(try drain(&decoder) == [typeText(2, "after")])
    }
}
