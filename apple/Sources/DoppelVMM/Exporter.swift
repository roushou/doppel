// Burns a recording's overlay into its video: each frame of the video,
// resampled to the reel's rate, with the reel's picture for that instant
// drawn over it. The reel decides everything drawn; this only paints.

import AVFoundation
import CoreGraphics
import AppKit
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

    private func rect(_ rect: Doppel_Common_V1_Rect) -> CGRect {
        CGRect(
            x: rect.origin.x * scale, y: rect.origin.y * scale,
            width: rect.size.width * scale, height: rect.size.height * scale
        )
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
            case .glyph(let glyph):
                badge(glyph, colour: colour(shape.colour, opacity), opacity: opacity, on: context)
            case .veil(let veil):
                // Even-odd: the whole picture, less each hole.
                context.addRect(context.boundingBoxOfClipPath)
                for hole in veil.holes { context.addRect(rect(hole)) }
                context.setFillColor(colour(shape.colour, opacity))
                context.fillPath(using: .evenOdd)
            case .brackets(let brackets):
                let r = rect(brackets.rect)
                let arm = min(brackets.arm * scale, r.width / 2, r.height / 2)
                for (corner, dx, dy) in [
                    (CGPoint(x: r.minX, y: r.minY), 1.0, 1.0), (CGPoint(x: r.maxX, y: r.minY), -1.0, 1.0),
                    (CGPoint(x: r.minX, y: r.maxY), 1.0, -1.0), (CGPoint(x: r.maxX, y: r.maxY), -1.0, -1.0),
                ] {
                    context.move(to: CGPoint(x: corner.x + dx * arm, y: corner.y))
                    context.addLine(to: corner)
                    context.addLine(to: CGPoint(x: corner.x, y: corner.y + dy * arm))
                }
                context.setStrokeColor(colour(shape.colour, opacity))
                context.setLineWidth(brackets.width * scale)
                context.setLineCap(.square)
                context.strokePath()
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

    /// The SF Symbol for a glyph; `nil` for one a newer doppel added.
    private static func symbol(_ glyph: Doppel_Vmm_V1_Glyph) -> String? {
        switch glyph {
        case .press: "hand.tap.fill"
        case .type: "character.cursor.ibeam"
        case .keys: "command"
        case .command: "apple.terminal.fill"
        case .wait: "hourglass"
        case .look: "eye.fill"
        case .point: "mappin"
        case .drag: "hand.draw.fill"
        case .scroll: "arrow.up.and.down"
        case .machine: "power"
        case .checkpoint: "flag.fill"
        case .rollback: "backward.fill"
        case .done: "checkmark"
        case .unspecified, .UNRECOGNIZED: nil
        }
    }

    /// A glyph's symbol, white, on a round badge of `colour`.
    private func badge(_ glyph: Doppel_Vmm_V1_GlyphShape, colour badge: CGColor, opacity: Double, on context: CGContext) {
        let side = glyph.size * scale
        let centre = point(glyph.centre)
        let disc = CGRect(x: centre.x - side / 2, y: centre.y - side / 2, width: side, height: side)
        context.setShadow(offset: CGSize(width: 0, height: 1 * scale), blur: 4 * scale, color: colour(0x000000, 0.45 * opacity))
        context.setFillColor(badge)
        context.fillEllipse(in: disc)
        context.setShadow(offset: .zero, blur: 0, color: nil)
        guard
            let name = Self.symbol(glyph.glyph),
            let image = NSImage(systemSymbolName: name, accessibilityDescription: nil)?
                .withSymbolConfiguration(.init(pointSize: side * 0.5, weight: .bold)),
            let mask = image.cgImage(forProposedRect: nil, context: nil, hints: nil)
        else { return }
        // Fit the symbol in the disc's middle, keeping its shape.
        let fit = side * 0.56 / max(CGFloat(mask.width), CGFloat(mask.height))
        let size = CGSize(width: CGFloat(mask.width) * fit, height: CGFloat(mask.height) * fit)
        let rect = CGRect(x: centre.x - size.width / 2, y: centre.y - size.height / 2, width: size.width, height: size.height)
        // The context's y grows down; images draw up: flip around the rect.
        context.translateBy(x: 0, y: rect.midY * 2)
        context.scaleBy(x: 1, y: -1)
        context.clip(to: rect, mask: mask)
        context.setFillColor(colour(0xffffff, opacity))
        context.fill(rect)
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

    /// A frame of a source: its image, its moment on the session's clock,
    /// and when its source ends there.
    private struct Frame {
        let image: CVPixelBuffer
        let at: Double
        let until: Double
    }

    /// The frames of the reel's sources in order, each at its moment on the
    /// session's clock: one source after the other.
    private final class Frames {
        private var sources: [Doppel_Vmm_V1_Source]
        private var reader: AVAssetReader?
        private var output: AVAssetReaderTrackOutput?
        /// Where the source shows nothing: a file starts its clock before its
        /// first frame, and that stretch reads back as black.
        private var empty: [CMTimeRange] = []
        private var startMs: Double = 0
        private var endMs: Double = 0
        private(set) var size: CGSize = .zero

        init(_ sources: [Doppel_Vmm_V1_Source]) async throws {
            guard !sources.isEmpty else { throw ExportError.reel("it names no video") }
            self.sources = sources.sorted { $0.startMs < $1.startMs }
            try await open()
        }

        /// Opens the next source, if one is left.
        private func open() async throws {
            reader?.cancelReading()
            (reader, output) = (nil, nil)
            guard !sources.isEmpty else { return }
            let source = sources.removeFirst()
            let asset = AVURLAsset(url: URL(fileURLWithPath: source.path))
            guard let track = try await asset.loadTracks(withMediaType: .video).first else {
                throw ExportError.video("\(source.path) has no video track")
            }
            if size == .zero { size = try await track.load(.naturalSize) }
            empty = try await track.load(.segments).filter(\.isEmpty).map(\.timeMapping.target)
            startMs = source.startMs
            endMs = source.startMs + CMTimeGetSeconds(try await track.load(.timeRange).end) * 1000
            let reader: AVAssetReader
            do { reader = try AVAssetReader(asset: asset) } catch { throw ExportError.video(error.localizedDescription) }
            let output = AVAssetReaderTrackOutput(track: track, outputSettings: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA])
            output.alwaysCopiesSampleData = false
            reader.add(output)
            guard reader.startReading() else {
                throw ExportError.video(reader.error?.localizedDescription ?? "couldn't start reading \(source.path)")
            }
            (self.reader, self.output) = (reader, output)
        }

        /// The next frame and its moment on the session's clock, or nil at the end.
        func next() async throws -> Frame? {
            while let output {
                if let sample = output.copyNextSampleBuffer() {
                    let presented = CMSampleBufferGetPresentationTimeStamp(sample)
                    guard let image = CMSampleBufferGetImageBuffer(sample),
                          !empty.contains(where: { $0.containsTime(presented) })
                    else { continue }
                    // A source's clock starts at its `start_ms`.
                    return Frame(image: image, at: startMs + CMTimeGetSeconds(presented) * 1000, until: endMs)
                }
                try await open()
            }
            return nil
        }

        func cancel() { reader?.cancelReading() }
    }

    /// Writes the reel's videos with its pictures drawn over them to `out`,
    /// at the reel's rate: a frame for each picture, over the latest frame
    /// by the moment the picture names (`at_ms`). The reel sets the pace: a
    /// stretch can play faster or slower than it was recorded.
    public static func export(reel: Doppel_Vmm_V1_Reel, to out: URL) async throws -> Exported {
        guard reel.fps > 0, reel.display.width > 0, reel.display.height > 0 else {
            throw ExportError.reel("it has no frame rate or display size")
        }
        let frames = try await Frames(reel.sources)
        let (width, height) = (Int(frames.size.width), Int(frames.size.height))
        guard width > 0, height > 0 else {
            throw ExportError.video("it has no frames")
        }
        let format = kCVPixelFormatType_32BGRA

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
        guard writer.startWriting() else { throw ExportError.writer(writer.error?.localizedDescription ?? "couldn't start writing") }
        writer.startSession(atSourceTime: .zero)

        let painter = Painter(scale: CGFloat(width) / reel.display.width)
        let fps = Int32(reel.fps)
        let count = reel.pictures.count
        var shown: Frame?
        var next = try await frames.next()
        for frame in 0..<count {
            let at = CMTime(value: CMTimeValue(frame), timescale: fps)
            let picture = reel.pictures[frame]
            // The latest frame of the videos by the moment the picture names.
            // Once a video has ended, what comes next shows: across a gap
            // (a rollback, a restart), the machine is already as the next
            // video starts it.
            while let candidate = next,
                  candidate.at <= picture.atMs || (shown.map { $0.until <= picture.atMs } ?? false) {
                shown = candidate
                next = try await frames.next()
            }
            guard let image = shown?.image else { continue }
            while !input.isReadyForMoreMediaData {
                try await Task.sleep(nanoseconds: 2_000_000)
            }
            guard let pool = adaptor.pixelBufferPool else { throw ExportError.writer(writer.error?.localizedDescription ?? "no buffers") }
            var target: CVPixelBuffer?
            guard CVPixelBufferPoolCreatePixelBuffer(nil, pool, &target) == kCVReturnSuccess, let target else {
                throw ExportError.writer("no buffer for frame \(frame)")
            }
            copy(image, into: target)
            draw(picture, with: painter, on: target)
            guard adaptor.append(target, withPresentationTime: at) else {
                throw ExportError.writer(writer.error?.localizedDescription ?? "frame \(frame) refused")
            }
        }
        frames.cancel()
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
