// The recorder on synthetic frames: no VM needed.

import AVFoundation
import CoreMedia
import CoreVideo
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

private func scratchFile(_ name: String) -> URL {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("doppel-recorder-\(getpid())-\(name).mp4")
    try? FileManager.default.removeItem(at: url)
    return url
}

@Test func framesAtTheirTimesMakeAVideoAsLongAsTheRecording() async throws {
    let url = scratchFile("timed")
    defer { try? FileManager.default.removeItem(at: url) }
    let origin = CMTime(seconds: 1_000, preferredTimescale: 600)
    let recorder = try Recorder(url: url, width: 320, height: 200, origin: origin)
    // A screen that changes 10 times in the first half second, then holds.
    for i in 0..<10 {
        recorder.append(try surface(width: 320, height: 200, shade: UInt8(i * 20)),
                        at: CMTimeAdd(origin, CMTime(value: Int64(i) * 30, timescale: 600)))
        try await Task.sleep(for: .milliseconds(10))
    }
    let stopped = try await recorder.finish(at: CMTimeAdd(origin, CMTime(seconds: 2, preferredTimescale: 600)))

    #expect(stopped.path == url.path)
    #expect(stopped.frames == 10)
    #expect(abs(stopped.seconds - 2) < 0.01, "the held frame lasts until the stop")
    #expect(stopped.bytes > 0)
    let asset = AVURLAsset(url: url)
    let tracks = try await asset.loadTracks(withMediaType: .video)
    #expect(tracks.count == 1)
    let duration = try await asset.load(.duration)
    #expect(abs(duration.seconds - 2) < 0.1, "\(duration.seconds)")
    let size = try await tracks[0].load(.naturalSize)
    #expect(size == CGSize(width: 320, height: 200))
}

@Test func hostClockTimesOverSeveralFragmentsMakeAWholeFile() async throws {
    // Live frames carry host-clock times (nanosecond timescale), and a
    // recording outlasts its 2 s fragments.
    let url = scratchFile("host-clock")
    defer { try? FileManager.default.removeItem(at: url) }
    let origin = CMClockGetTime(CMClockGetHostTimeClock())
    let recorder = try Recorder(url: url, width: 64, height: 64, origin: origin)
    for i in 0..<60 {
        recorder.append(try surface(width: 64, height: 64, shade: UInt8(i * 4)),
                        at: CMTimeAdd(origin, CMTime(value: Int64(i) * 100_000_000, timescale: 1_000_000_000)))
        try await Task.sleep(for: .milliseconds(5))
    }
    let end = CMTimeAdd(origin, CMTime(value: 6_500_000_000, timescale: 1_000_000_000))
    let stopped = try await recorder.finish(at: end)
    // A frame the encoder isn't ready for is dropped, by design: under load
    // one may be.
    #expect(stopped.frames >= 55, "\(stopped.frames)")
    let asset = AVURLAsset(url: url)
    let duration = try await asset.load(.duration)
    #expect(abs(duration.seconds - 6.5) < 0.1, "\(duration.seconds)")
    let track = try #require(try await asset.loadTracks(withMediaType: .video).first)
    let reader = try AVAssetReader(asset: asset)
    let output = AVAssetReaderTrackOutput(track: track, outputSettings: nil)
    reader.add(output)
    reader.startReading()
    var samples = 0
    while let sample = output.copyNextSampleBuffer() {
        if CMSampleBufferGetNumSamples(sample) > 0 { samples += 1 }
    }
    #expect(samples == Int(stopped.frames), "the file holds every frame the recorder kept")
}

@Test func outOfOrderTimesStillIncrease() async throws {
    let url = scratchFile("order")
    defer { try? FileManager.default.removeItem(at: url) }
    let origin = CMTime(seconds: 50, preferredTimescale: 600)
    let recorder = try Recorder(url: url, width: 64, height: 64, origin: origin)
    // A stamp before the origin, then the same stamp twice.
    for at in [CMTime(seconds: 49, preferredTimescale: 600), origin, origin] {
        recorder.append(try surface(width: 64, height: 64, shade: 128), at: at)
        try await Task.sleep(for: .milliseconds(10))
    }
    let stopped = try await recorder.finish(at: CMTimeAdd(origin, CMTime(seconds: 1, preferredTimescale: 600)))
    #expect(stopped.frames == 3, "none rejected for its time")
}

@Test func recordingErrorsMapToTheirWireKinds() {
    #expect(RecordingError.exists("/a").wire.invalidArgument.field == "path")
    #expect(RecordingError.notRecording(nil).wire.invalidState.reason == "nothing is being recorded")
}
