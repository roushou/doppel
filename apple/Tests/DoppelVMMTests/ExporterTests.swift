// The exporter on synthetic videos and reels: no VM needed.

import AVFoundation
import CoreMedia
import CoreVideo
import DoppelWire
import Foundation
import IOSurface
import Testing
@testable import DoppelVMM

private func scratch(_ name: String) -> URL {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("doppel-export-\(getpid())-\(name).mp4")
    try? FileManager.default.removeItem(at: url)
    return url
}

/// A grey BGRA frame.
private func buffer(width: Int, height: Int, shade: UInt8) throws -> CVPixelBuffer {
    var made: CVPixelBuffer?
    let attributes = [kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary] as CFDictionary
    try #require(CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_32BGRA, attributes, &made) == kCVReturnSuccess)
    let pixels = try #require(made)
    CVPixelBufferLockBaseAddress(pixels, [])
    memset(CVPixelBufferGetBaseAddress(pixels), Int32(shade), CVPixelBufferGetDataSize(pixels))
    CVPixelBufferUnlockBaseAddress(pixels, [])
    return pixels
}

/// The (blue, green, red) of the pixel at (x, y), y down from the top.
private func pixel(_ buffer: CVPixelBuffer, x: Int, y: Int) -> (UInt8, UInt8, UInt8) {
    CVPixelBufferLockBaseAddress(buffer, .readOnly)
    defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
    let row = CVPixelBufferGetBaseAddress(buffer)!.advanced(by: y * CVPixelBufferGetBytesPerRow(buffer))
    let bytes = row.advanced(by: x * 4).assumingMemoryBound(to: UInt8.self)
    return (bytes[0], bytes[1], bytes[2])
}

private func spot(_ x: Double, _ y: Double) -> Doppel_Vmm_V1_Spot {
    var spot = Doppel_Vmm_V1_Spot()
    spot.x = x
    spot.y = y
    return spot
}

/// A red frame round (10, 10, 40×20) in points, filled half.
private func redFrame() -> Doppel_Vmm_V1_Shape {
    var shape = Doppel_Vmm_V1_Shape()
    shape.opacity = 1
    shape.colour = 0xff0000
    var frame = Doppel_Vmm_V1_FrameShape()
    frame.rect.origin.x = 10
    frame.rect.origin.y = 10
    frame.rect.size.width = 40
    frame.rect.size.height = 20
    frame.wash = 1
    frame.width = 2
    shape.frame = frame
    return shape
}

@Suite struct ExportCommand {
    @Test func parsesItsThreePaths() throws {
        let command = try Command(arguments: ["export", "--video", "/a.mp4", "--reel", "/r.pb", "--out", "/o.mp4"])
        guard case .export(let video, let reel, let out) = command else {
            Issue.record("parsed as \(command)")
            return
        }
        #expect((video.path, reel.path, out.path) == ("/a.mp4", "/r.pb", "/o.mp4"))
    }

    @Test(arguments: [
        ["export", "--video", "/a.mp4", "--reel", "/r.pb"],
        ["export", "--video", "a.mp4", "--reel", "/r.pb", "--out", "/o.mp4"],
        ["export", "--video", "/a.mp4", "--reel", "/r.pb", "--out", "/o.mp4", "--spec", "/s"],
    ])
    func refusesMissingRelativeOrStrayPaths(arguments: [String]) {
        #expect(throws: UsageError.self) { try Command(arguments: arguments) }
    }
}

@Suite struct Painting {
    @Test func shapesLandWhereTheirPointsSayAtTheVideosScale() throws {
        // A 200×100 video of a 100×50 display: two pixels a point.
        let frame = try buffer(width: 200, height: 100, shade: 0)
        var picture = Doppel_Vmm_V1_Picture()
        picture.shapes = [redFrame()]
        Exporter.draw(picture, with: Painter(scale: 2), on: frame)
        // Inside the frame, top-left origin: (20…100, 20…60) in pixels.
        #expect(pixel(frame, x: 50, y: 40) == (0, 0, 255))
        #expect(pixel(frame, x: 5, y: 5) == (0, 0, 0))
        #expect(pixel(frame, x: 150, y: 90) == (0, 0, 0))
    }

    @Test func aFadedShapeBlendsWithTheFrame() throws {
        let frame = try buffer(width: 200, height: 100, shade: 0)
        var shape = redFrame()
        shape.opacity = 0.5
        var picture = Doppel_Vmm_V1_Picture()
        picture.shapes = [shape]
        Exporter.draw(picture, with: Painter(scale: 2), on: frame)
        let (_, _, red) = pixel(frame, x: 50, y: 40)
        #expect(red > 100 && red < 160, "\(red)")
    }

    @Test func aVeilDimsAllButItsHoles() throws {
        let frame = try buffer(width: 200, height: 100, shade: 255)
        var shape = Doppel_Vmm_V1_Shape()
        shape.opacity = 1
        shape.colour = 0x000000
        var veil = Doppel_Vmm_V1_VeilShape()
        veil.holes = [redFrame().frame.rect]
        shape.veil = veil
        var picture = Doppel_Vmm_V1_Picture()
        picture.shapes = [shape]
        Exporter.draw(picture, with: Painter(scale: 2), on: frame)
        #expect(pixel(frame, x: 50, y: 40) == (255, 255, 255))
        #expect(pixel(frame, x: 150, y: 90) == (0, 0, 0))
    }

    @Test func bracketsDrawOnlyTheCorners() throws {
        let frame = try buffer(width: 200, height: 100, shade: 0)
        var shape = Doppel_Vmm_V1_Shape()
        shape.opacity = 1
        shape.colour = 0xff0000
        var brackets = Doppel_Vmm_V1_BracketsShape()
        brackets.rect = redFrame().frame.rect
        brackets.arm = 5
        brackets.width = 2
        shape.brackets = brackets
        var picture = Doppel_Vmm_V1_Picture()
        picture.shapes = [shape]
        Exporter.draw(picture, with: Painter(scale: 2), on: frame)
        // The top-left corner, in pixels (20, 20), is drawn; the top edge's middle isn't.
        #expect(pixel(frame, x: 22, y: 20) == (0, 0, 255))
        #expect(pixel(frame, x: 60, y: 20) == (0, 0, 0))
        #expect(pixel(frame, x: 50, y: 40) == (0, 0, 0), "nothing inside")
    }
}

@Test func anExportIsTheVideoAtTheReelsRateWithItsPicturesOver() async throws {
    // A 2 s recording of a grey 320×200 screen.
    let video = scratch("source")
    let out = scratch("out")
    defer {
        try? FileManager.default.removeItem(at: video)
        try? FileManager.default.removeItem(at: out)
    }
    let origin = CMTime(seconds: 50, preferredTimescale: 600)
    let recorder = try Recorder(url: video, width: 320, height: 200, origin: origin)
    for i in 0..<4 {
        // The buffer owns the surface: it lives until the recorder has copied it.
        let frame = try buffer(width: 320, height: 200, shade: 90)
        let surface = try #require(CVPixelBufferGetIOSurface(frame)?.takeUnretainedValue())
        withExtendedLifetime(frame) {
            recorder.append(surface, at: CMTimeAdd(origin, CMTime(value: Int64(i) * 150, timescale: 600)))
        }
        try await Task.sleep(for: .milliseconds(10))
    }
    _ = try await recorder.finish(at: CMTimeAdd(origin, CMTime(seconds: 2, preferredTimescale: 600)))

    // A 160×100 display; its pictures cover the first second only.
    var reel = Doppel_Vmm_V1_Reel()
    reel.fps = 10
    reel.display.width = 160
    reel.display.height = 100
    var picture = Doppel_Vmm_V1_Picture()
    picture.shapes = [redFrame()]
    reel.pictures = Array(repeating: picture, count: 10)

    let exported = try await Exporter.export(video: video, reel: reel, to: out)
    #expect(exported.frames == 20)
    #expect(abs(exported.seconds - 2) < 0.01)

    let asset = AVURLAsset(url: out)
    let track = try #require(try await asset.loadTracks(withMediaType: .video).first)
    #expect(try await track.load(.naturalSize) == CGSize(width: 320, height: 200))
    let reader = try AVAssetReader(asset: asset)
    let output = AVAssetReaderTrackOutput(track: track, outputSettings: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA])
    reader.add(output)
    #expect(reader.startReading())
    var reds: [Bool] = []
    while let sample = output.copyNextSampleBuffer(), let image = CMSampleBufferGetImageBuffer(sample) {
        let (blue, _, red) = pixel(image, x: 60, y: 40)
        reds.append(red > 200 && blue < 60)
    }
    #expect(reds.count == 20)
    // Drawn over the first second's frames, not over the second's.
    #expect(reds.prefix(10).allSatisfy { $0 }, "\(reds)")
    #expect(reds.suffix(10).allSatisfy { !$0 }, "\(reds)")
}
