// The display stream's encoder and per-connection feeds, on synthetic
// frames: no VM needed.

import CoreMedia
import CoreVideo
import DoppelWire
import Foundation
import IOSurface
import Testing
@testable import DoppelVMM

private func surface(width: Int, height: Int, shade: UInt8) throws -> IOSurfaceRef {
    var buffer: CVPixelBuffer?
    let attributes = [kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary] as CFDictionary
    try #require(CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_32BGRA, attributes, &buffer) == kCVReturnSuccess)
    let pixels = try #require(buffer)
    CVPixelBufferLockBaseAddress(pixels, [])
    memset(CVPixelBufferGetBaseAddress(pixels), Int32(shade), CVPixelBufferGetDataSize(pixels))
    CVPixelBufferUnlockBaseAddress(pixels, [])
    return try #require(CVPixelBufferGetIOSurface(pixels)?.takeUnretainedValue())
}

private final class Collected: @unchecked Sendable {
    private let lock = NSLock()
    private var frames: [Doppel_Vmm_V1_EncodedFrame] = []
    func add(_ frame: Doppel_Vmm_V1_EncodedFrame) { lock.withLock { frames.append(frame) } }
    var all: [Doppel_Vmm_V1_EncodedFrame] { lock.withLock { frames } }
}

/// The NAL unit types in AVCC data (4-byte big-endian lengths), or nil if
/// the lengths don't add up to the data.
private func nalTypes(_ data: Data) -> [UInt8]? {
    let bytes = [UInt8](data)
    var types: [UInt8] = []
    var offset = 0
    while offset < bytes.count {
        guard offset + 4 < bytes.count else { return nil }
        let length = bytes[offset..<offset + 4].reduce(0) { $0 << 8 | Int($1) }
        offset += 4
        guard length > 0, offset + length <= bytes.count else { return nil }
        types.append(bytes[offset] & 0x1F)
        offset += length
    }
    return types
}

@Test func theStreamStartsWithAKeyFrameAndItsParameterSets() throws {
    let collected = Collected()
    let streamer = try Streamer(width: 320, height: 200) { collected.add($0) }
    let origin = CMClockGetTime(CMClockGetHostTimeClock())
    for i in 0..<6 {
        streamer.encode(try surface(width: 320, height: 200, shade: UInt8(i * 40)),
                        at: CMTimeAdd(origin, CMTime(value: Int64(i), timescale: 60)), key: i == 4)
    }
    streamer.flush()
    let frames = collected.all

    #expect(frames.count == 6)
    #expect(frames.map(\.sequence) == Array(1...6))
    #expect(frames.map(\.key) == [true, false, false, false, true, false], "the first, and the one asked for")
    for frame in frames {
        let types = try #require(nalTypes(frame.data), "AVCC with 4-byte lengths")
        #expect(types.contains(frame.key ? 5 : 1), "\(types)")
        if frame.key {
            #expect(frame.parameterSets.count == 2)
            #expect(frame.parameterSets.map { $0.first.map { $0 & 0x1F } } == [7, 8], "the SPS, then the PPS")
        } else {
            #expect(frame.parameterSets.isEmpty)
        }
    }
}

private func frame(_ sequence: UInt64, key: Bool, bytes: Int = 10) -> Doppel_Vmm_V1_EncodedFrame {
    .with { $0.sequence = sequence; $0.key = key; $0.data = Data(count: bytes) }
}

/// A connection that takes frames only when let.
private final class Reader: @unchecked Sendable {
    private let condition = NSCondition()
    private var taken: [UInt64] = []
    private var allowed = 0
    private var wanted = 0
    private var waiting = 0

    func send(_ frame: Doppel_Vmm_V1_EncodedFrame) -> Bool {
        condition.withLock {
            waiting += 1
            condition.broadcast()
            defer { waiting -= 1 }
            while allowed == 0 { condition.wait() }
            allowed -= 1
            taken.append(frame.sequence)
            condition.broadcast()
            return true
        }
    }

    func wantKeyFrame() { condition.withLock { wanted += 1 } }

    /// Lets `count` more frames through and waits for them.
    func take(_ count: Int) -> [UInt64] {
        condition.withLock {
            let target = taken.count + count
            allowed += count
            condition.broadcast()
            while taken.count < target { condition.wait() }
            return taken
        }
    }

    /// Waits until the feed is stuck sending a frame.
    func waitForAStalledSend() {
        condition.withLock { while waiting == 0 { condition.wait() } }
    }

    var keyFramesWanted: Int { condition.withLock { wanted } }
}

@Test func aFeedStartsAtAKeyFrame() {
    let reader = Reader()
    let feed = EncodedFeed(send: reader.send, wantKeyFrame: reader.wantKeyFrame)
    defer { feed.close() }
    feed.offer(frame(1, key: false))
    feed.offer(frame(2, key: true))
    feed.offer(frame(3, key: false))
    #expect(reader.take(2) == [2, 3])
}

@Test func aFeedTooFarBehindSkipsToTheNextKeyFrameAndAsksForOne() {
    let reader = Reader()
    let feed = EncodedFeed(send: reader.send, wantKeyFrame: reader.wantKeyFrame)
    defer { feed.close() }
    feed.offer(frame(1, key: true))
    #expect(reader.take(1) == [1])
    // The reader stalls with frame 2 in hand; then the queue fills.
    feed.offer(frame(2, key: false))
    reader.waitForAStalledSend()
    for sequence in 3...UInt64(3 + EncodedFeed.maxQueuedFrames) { feed.offer(frame(sequence, key: false)) }
    #expect(reader.keyFramesWanted == 1)
    // Frames after the overflow wait for a key frame.
    feed.offer(frame(100, key: false))
    feed.offer(frame(101, key: true))
    feed.offer(frame(102, key: false))
    let taken = reader.take(3)
    #expect(taken == [1, 2, 101, 102], "the frame in hand, then from the key frame on")
    #expect(reader.keyFramesWanted == 1)
}

/// Writes fixtures/h264/shades.avcc, which the viewer's decoder test reads:
/// three 64x64 frames of grey (shades 40, 120, 200), a key frame then two
/// deltas. Encoders differ between Macs, so it's written only when asked:
/// DOPPEL_UPDATE_H264=1 swift test --filter shadesFixture
///
/// Per frame: 1 byte key (0 or 1); 1 byte parameter set count, then each as
/// a 2-byte big-endian length and its bytes; then a 4-byte big-endian
/// length and the frame's AVCC data.
@Test func shadesFixture() throws {
    guard ProcessInfo.processInfo.environment["DOPPEL_UPDATE_H264"] == "1" else { return }
    let collected = Collected()
    let streamer = try Streamer(width: 64, height: 64) { collected.add($0) }
    let origin = CMClockGetTime(CMClockGetHostTimeClock())
    for (i, shade) in [UInt8(40), 120, 200].enumerated() {
        streamer.encode(try surface(width: 64, height: 64, shade: shade),
                        at: CMTimeAdd(origin, CMTime(value: Int64(i), timescale: 60)), key: false)
    }
    streamer.flush()
    let frames = collected.all
    try #require(frames.map(\.key) == [true, false, false])
    var bytes = Data()
    func bigEndian<T: FixedWidthInteger>(_ value: T) { withUnsafeBytes(of: value.bigEndian) { bytes.append(contentsOf: $0) } }
    for frame in frames {
        bytes.append(frame.key ? 1 : 0)
        bytes.append(UInt8(frame.parameterSets.count))
        for set in frame.parameterSets {
            bigEndian(UInt16(set.count))
            bytes.append(set)
        }
        bigEndian(UInt32(frame.data.count))
        bytes.append(frame.data)
    }
    let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("../../../fixtures/h264")
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    try bytes.write(to: root.appendingPathComponent("shades.avcc").standardizedFileURL)
}
