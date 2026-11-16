// Burns a recording's overlay into its video: each frame of the video,
// resampled to the reel's rate, with the reel's picture for that instant
// drawn over it. The reel decides everything drawn; this only paints.

import AVFoundation
import CoreGraphics
import CoreText
import DoppelWire
import Foundation

public enum ExportError: Error, CustomStringConvertible {
    case video(String)
    case reel(String)
    case writer(String)

    public var description: String {
        switch self {
        case .video(let why): "can't read the video: \(why)"
        case .reel(let why): "can't read the overlay: \(why)"
        case .writer(let why): "can't write the export: \(why)"
        }
    }
}

/// What an export wrote.
public struct Exported: Sendable, Equatable {
    public let frames: Int
    public let seconds: Double
}

/// Draws a reel's pictures: shapes in display points onto a context the
/// size of the video, its origin top-left.
struct Painter {
    /// Video pixels per display point.
    let scale: CGFloat
    /// The mono face the viewers' captions use.
    private let font: CTFont

    init(scale: CGFloat) {
        self.scale = scale
        font = CTFontCreateUIFontForLanguage(.userFixedPitch, 11 * scale, nil)
            ?? CTFontCreateWithName("Menlo" as CFString, 11 * scale, nil)
    }

    private func colour(_ hex: UInt32, _ alpha: Double) -> CGColor {
        CGColor(
            srgbRed: CGFloat((hex >> 16) & 0xff) / 255,
            green: CGFloat((hex >> 8) & 0xff) / 255,
            blue: CGFloat(hex & 0xff) / 255,
            alpha: CGFloat(max(0, min(1, alpha)))
        )
    }

    private func point(_ spot: Doppel_Vmm_V1_Spot) -> CGPoint {
        CGPoint(x: spot.x * scale, y: spot.y * scale)
    }

    /// Draws `picture` on `context`, whose y grows downwards.
    func paint(_ picture: Doppel_Vmm_V1_Picture, on context: CGContext) {
        for shape in picture.shapes where shape.opacity > 0 {
            context.saveGState()
            defer { context.restoreGState() }
            let opacity = shape.opacity
            switch shape.kind {
            case .frame(let frame):
                let rect = CGRect(
                    x: frame.rect.origin.x * scale, y: frame.rect.origin.y * scale,
                    width: frame.rect.size.width * scale, height: frame.rect.size.height * scale
                )
                context.setFillColor(colour(shape.colour, frame.wash * opacity))
                context.fill(rect)
                context.setStrokeColor(colour(shape.colour, opacity))
                context.setLineWidth(frame.width * scale)
                context.stroke(rect)
            case .ring(let ring):
                let r = ring.radius * scale
                let c = point(ring.centre)
                let rect = CGRect(x: c.x - r, y: c.y - r, width: 2 * r, height: 2 * r)
                context.setFillColor(colour(shape.colour, ring.wash * opacity))
                context.fillEllipse(in: rect)
                context.setStrokeColor(colour(shape.colour, opacity))
                context.setLineWidth(ring.width * scale)
                context.strokeEllipse(in: rect)
            case .stroke(let stroke):
                stroked(stroke, colour: colour(shape.colour, opacity), on: context)
            case .pointer(let pointer):
                pointed(at: point(pointer.tip), colour: colour(shape.colour, opacity), opacity: opacity, on: context)
            case .tag(let tag):
                tagged(tag, plate: colour(shape.colour, opacity), ink: colour(tag.ink, opacity), on: context)
            case nil:
                // A shape a newer doppel added: nothing to draw.
                break
            }
        }
    }

    private func stroked(_ stroke: Doppel_Vmm_V1_StrokeShape, colour: CGColor, on context: CGContext) {
        let from = point(stroke.from)
        var to = point(stroke.to)
        let width = stroke.width * scale
        context.setStrokeColor(colour)
        context.setFillColor(colour)
        context.setLineWidth(width)
        switch stroke.line {
        case .dashed:
            context.setLineDash(phase: 0, lengths: [3 * scale, 5 * scale])
        case .arrow:
            // The head ends at `to`; the line stops at its base.
            let length = max(1, hypot(to.x - from.x, to.y - from.y))
            let (ux, uy) = ((to.x - from.x) / length, (to.y - from.y) / length)
            let head = 14 * scale, half = 8.4 * scale
            let base = CGPoint(x: to.x - ux * head, y: to.y - uy * head)
            context.beginPath()
            context.move(to: to)
            context.addLine(to: CGPoint(x: base.x - uy * half, y: base.y + ux * half))
            context.addLine(to: CGPoint(x: base.x + uy * half, y: base.y - ux * half))
            context.closePath()
            context.fillPath()
            to = base
        case .solid, .unspecified, .UNRECOGNIZED:
            break
        }
        context.beginPath()
        context.move(to: from)
        context.addLine(to: to)
        context.strokePath()
    }

    /// The viewers' pointer: an arrow 22 points tall, white-edged.
    private func pointed(at tip: CGPoint, colour: CGColor, opacity: Double, on context: CGContext) {
        let unit = 22.0 / 24.0 * scale
        let outline: [(CGFloat, CGFloat)] = [(4, 2.5), (4, 19), (8.6, 14.8), (11.6, 21.4), (14.6, 20), (11.7, 13.6), (18, 13.6)]
        context.beginPath()
        for (index, (x, y)) in outline.enumerated() {
            let at = CGPoint(x: tip.x + (x - 4) * unit, y: tip.y + (y - 2.5) * unit)
            if index == 0 { context.move(to: at) } else { context.addLine(to: at) }
        }
        context.closePath()
        context.setFillColor(colour)
        context.setStrokeColor(CGColor(srgbRed: 1, green: 1, blue: 1, alpha: CGFloat(opacity)))
        context.setLineWidth(1.5 * scale)
        context.setLineJoin(.miter)
        context.drawPath(using: .fillStroke)
    }

    private func tagged(_ tag: Doppel_Vmm_V1_TagShape, plate: CGColor, ink: CGColor, on context: CGContext) {
        let attributes: [CFString: Any] = [kCTFontAttributeName: font, kCTForegroundColorAttributeName: ink]
        guard let text = CFAttributedStringCreate(nil, tag.text as CFString, attributes as CFDictionary) else { return }
        let line = CTLineCreateWithAttributedString(text)
        var ascent: CGFloat = 0, descent: CGFloat = 0, leading: CGFloat = 0
        let width = CGFloat(CTLineGetTypographicBounds(line, &ascent, &descent, &leading))
        let (padX, padY) = (8 * scale, 4 * scale)
        let size = CGSize(width: min(width, 360 * scale) + 2 * padX, height: ascent + descent + 2 * padY)
        let at = point(tag.at)
        let origin: CGPoint
        switch tag.corner {
        case .topRight: origin = CGPoint(x: at.x - size.width, y: at.y)
        case .bottomLeft: origin = CGPoint(x: at.x, y: at.y - size.height)
        case .topLeft, .unspecified, .UNRECOGNIZED: origin = at
        }
        let plateRect = CGRect(origin: origin, size: size)
        context.setFillColor(plate)
        context.fill(plateRect)
        context.clip(to: plateRect.insetBy(dx: padX / 2, dy: 0))
        // Text draws upwards: flip around the line's baseline.
        context.textMatrix = .identity
        context.translateBy(x: origin.x + padX, y: origin.y + padY + ascent)
        context.scaleBy(x: 1, y: -1)
        context.textPosition = .zero
        CTLineDraw(line, context)
    }
}

/// Burns a reel into a video.
public enum Exporter {
    private static let bitsPerSecond = 12_000_000

    /// Writes `video` with `reel` drawn over it to `out`, at the reel's
    /// rate: each frame is the video's latest frame by then, with that
    /// instant's picture over it. Past the reel's end, frames go out as
    /// they are.
    public static func export(video: URL, reel: Doppel_Vmm_V1_Reel, to out: URL) async throws -> Exported {
        guard reel.fps > 0, reel.display.width > 0, reel.display.height > 0 else {
            throw ExportError.reel("it has no frame rate or display size")
        }
        let asset = AVURLAsset(url: video)
        guard let track = try await asset.loadTracks(withMediaType: .video).first else {
            throw ExportError.video("it has no video track")
        }
        let natural = try await track.load(.naturalSize)
        let duration = try await asset.load(.duration).seconds
        let (width, height) = (Int(natural.width), Int(natural.height))
        guard width > 0, height > 0, duration.isFinite, duration > 0 else {
            throw ExportError.video("it has no frames")
        }

        let reader: AVAssetReader
        do { reader = try AVAssetReader(asset: asset) } catch { throw ExportError.video(error.localizedDescription) }
        let format = kCVPixelFormatType_32BGRA
        let source = AVAssetReaderTrackOutput(track: track, outputSettings: [kCVPixelBufferPixelFormatTypeKey as String: format])
        source.alwaysCopiesSampleData = false
        reader.add(source)

        let writer: AVAssetWriter
        do { writer = try AVAssetWriter(outputURL: out, fileType: .mp4) } catch { throw ExportError.writer(error.localizedDescription) }
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: width,
            AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: [
                AVVideoAverageBitRateKey: bitsPerSecond,
                AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel,
            ],
        ])
        input.expectsMediaDataInRealTime = false
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
            kCVPixelBufferPixelFormatTypeKey as String: format,
            kCVPixelBufferWidthKey as String: width,
            kCVPixelBufferHeightKey as String: height,
        ])
        guard writer.canAdd(input) else { throw ExportError.writer("can't add a video track") }
        writer.add(input)
        guard reader.startReading() else { throw ExportError.video(reader.error?.localizedDescription ?? "couldn't start reading") }
        guard writer.startWriting() else { throw ExportError.writer(writer.error?.localizedDescription ?? "couldn't start writing") }
        writer.startSession(atSourceTime: .zero)

        let painter = Painter(scale: CGFloat(width) / reel.display.width)
        let fps = Int32(reel.fps)
        let count = Int((duration * Double(fps)).rounded(.up))
        var shown: CMSampleBuffer?
        var next = source.copyNextSampleBuffer()
        for frame in 0..<count {
            let at = CMTime(value: CMTimeValue(frame), timescale: fps)
            // The latest frame of the video by `at`.
            while let candidate = next, CMTimeCompare(CMSampleBufferGetPresentationTimeStamp(candidate), at) <= 0 {
                shown = candidate
                next = source.copyNextSampleBuffer()
            }
            guard let shown, let image = CMSampleBufferGetImageBuffer(shown) else { continue }
            while !input.isReadyForMoreMediaData {
                try await Task.sleep(nanoseconds: 2_000_000)
            }
            guard let pool = adaptor.pixelBufferPool else { throw ExportError.writer(writer.error?.localizedDescription ?? "no buffers") }
            var target: CVPixelBuffer?
            guard CVPixelBufferPoolCreatePixelBuffer(nil, pool, &target) == kCVReturnSuccess, let target else {
                throw ExportError.writer("no buffer for frame \(frame)")
            }
            copy(image, into: target)
            if frame < reel.pictures.count {
                draw(reel.pictures[frame], with: painter, on: target)
            }
            guard adaptor.append(target, withPresentationTime: at) else {
                throw ExportError.writer(writer.error?.localizedDescription ?? "frame \(frame) refused")
            }
        }
        reader.cancelReading()
        input.markAsFinished()
        await writer.finishWriting()
        guard writer.status == .completed else {
            throw ExportError.writer(writer.error?.localizedDescription ?? "didn't complete")
        }
        return Exported(frames: count, seconds: Double(count) / Double(fps))
    }

    /// Copies a BGRA frame row by row: the two may pad their rows differently.
    private static func copy(_ from: CVPixelBuffer, into to: CVPixelBuffer) {
        CVPixelBufferLockBaseAddress(from, .readOnly)
        CVPixelBufferLockBaseAddress(to, [])
        defer {
            CVPixelBufferUnlockBaseAddress(to, [])
            CVPixelBufferUnlockBaseAddress(from, .readOnly)
        }
        guard let source = CVPixelBufferGetBaseAddress(from), let target = CVPixelBufferGetBaseAddress(to) else { return }
        let rows = min(CVPixelBufferGetHeight(from), CVPixelBufferGetHeight(to))
        let (sourceRow, targetRow) = (CVPixelBufferGetBytesPerRow(from), CVPixelBufferGetBytesPerRow(to))
        let bytes = min(sourceRow, targetRow)
        for row in 0..<rows {
            memcpy(target + row * targetRow, source + row * sourceRow, bytes)
        }
    }

    static func draw(_ picture: Doppel_Vmm_V1_Picture, with painter: Painter, on buffer: CVPixelBuffer) {
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        guard let context = CGContext(
            data: CVPixelBufferGetBaseAddress(buffer),
            width: CVPixelBufferGetWidth(buffer),
            height: CVPixelBufferGetHeight(buffer),
            bitsPerComponent: 8,
            bytesPerRow: CVPixelBufferGetBytesPerRow(buffer),
            space: CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue
        ) else { return }
        // Points down from the top, as the reel's are.
        context.translateBy(x: 0, y: CGFloat(CVPixelBufferGetHeight(buffer)))
        context.scaleBy(x: 1, y: -1)
        painter.paint(picture, on: context)
    }

    /// Reads a reel written as one `Reel` message.
    public static func reel(at url: URL) throws -> Doppel_Vmm_V1_Reel {
        do {
            return try Doppel_Vmm_V1_Reel(serializedBytes: Data(contentsOf: url))
        } catch {
            throw ExportError.reel(error.localizedDescription)
        }
    }
}
