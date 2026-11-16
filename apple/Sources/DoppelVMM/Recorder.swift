// Records the display to a video file as frames come.
//
// Frames arrive when the screen changes, not at a fixed rate: each one is
// stamped with the host time it was seen, so a still screen costs nothing
// and plays back as long as it lasted. The file is fragmented MP4, so one
// cut short by a crash plays up to its last fragment.

import AVFoundation
import CoreMedia
import CoreVideo
import DoppelWire
import Foundation
import IOSurface
import VideoToolbox

public enum RecordingError: Error, CustomStringConvertible {
    case notAbsolute(String)
    case exists(String)
    case notRecording(String?)
    case writer(String)

    public var description: String {
        switch self {
        case .notAbsolute(let path): "\(path) isn't an absolute path"
        case .exists(let path): "\(path) already exists"
        case .notRecording(nil): "nothing is being recorded"
        case .notRecording(let path?): "\(path) isn't being recorded"
        case .writer(let reason): "the video writer failed: \(reason)"
        }
    }

    var wire: Doppel_Common_V1_Error {
        switch self {
        case .notAbsolute, .exists: .invalidArgument("path", description)
        case .notRecording: .invalidState(description)
        case .writer: .internal(description)
        }
    }
}

/// One recording. Appends happen on the display writer's queue; `finish`
/// runs once nothing appends any more.
final class Recorder: @unchecked Sendable {
    /// Encoder settings: H.264 for players everywhere; the hardware encoder
    /// keeps up with 2560x1600 at 60 fps.
    private static let bitsPerSecond = 8_000_000
    private static let keyFrameSeconds = 2
    private static let fragmentSeconds = 2.0
    /// Presentation times are kept in this timescale.
    private static let timescale: CMTimeScale = 60_000

    let url: URL
    private let writer: AVAssetWriter
    private let input: AVAssetWriterInput
    private let adaptor: AVAssetWriterInputPixelBufferAdaptor
    private let session: VTPixelTransferSession
    /// Host time of the recording's time zero.
    private let origin: CMTime
    private var frames: UInt64 = 0
    private var last: CMTime = .zero

    init(url: URL, width: Int, height: Int, origin: CMTime) throws {
        self.url = url
        self.origin = origin
        do {
            writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
        } catch {
            throw RecordingError.writer(error.localizedDescription)
        }
        writer.movieFragmentInterval = CMTime(seconds: Self.fragmentSeconds, preferredTimescale: 600)
        input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: width,
            AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: [
                AVVideoAverageBitRateKey: Self.bitsPerSecond,
                AVVideoMaxKeyFrameIntervalDurationKey: Self.keyFrameSeconds,
                AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel,
                // With B-frames, live frames' uneven times fail the
                // writer (-16341) at a fragment boundary.
                AVVideoAllowFrameReorderingKey: false,
            ],
        ])
        input.expectsMediaDataInRealTime = true
        adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelBufferWidthKey as String: width,
            kCVPixelBufferHeightKey as String: height,
            kCVPixelBufferIOSurfacePropertiesKey as String: [String: Any](),
        ])
        guard writer.canAdd(input) else { throw RecordingError.writer("can't add a video track") }
        writer.add(input)
        guard writer.startWriting() else {
            throw RecordingError.writer(writer.error?.localizedDescription ?? "couldn't start writing")
        }
        writer.startSession(atSourceTime: .zero)
        var created: VTPixelTransferSession?
        let status = VTPixelTransferSessionCreate(allocator: nil, pixelTransferSessionOut: &created)
        guard status == noErr, let created else {
            writer.cancelWriting()
            throw RecordingError.writer("VTPixelTransferSessionCreate failed (\(status))")
        }
        session = created
    }

    /// The frame shown from host time `time` on. Copied out of `surface`
    /// first: VZ draws into it again while the encoder still works. Dropped
    /// when the encoder is behind.
    func append(_ surface: IOSurfaceRef, at time: CMTime) {
        guard input.isReadyForMoreMediaData, let pool = adaptor.pixelBufferPool else { return }
        var source: Unmanaged<CVPixelBuffer>?
        var target: CVPixelBuffer?
        guard CVPixelBufferCreateWithIOSurface(nil, surface, nil, &source) == kCVReturnSuccess,
              let source = source?.takeRetainedValue(),
              CVPixelBufferPoolCreatePixelBuffer(nil, pool, &target) == kCVReturnSuccess,
              let target,
              VTPixelTransferSessionTransferImage(session, from: source, to: target) == noErr
        else { return }
        // Presentation times must increase.
        var at = CMTimeConvertScale(CMTimeSubtract(time, origin), timescale: Self.timescale, method: .roundHalfAwayFromZero)
        if frames > 0, CMTimeCompare(at, last) <= 0 {
            at = CMTimeAdd(last, CMTime(value: 1, timescale: Self.timescale))
        }
        if at.seconds < 0 { at = .zero }
        guard adaptor.append(target, withPresentationTime: at) else { return }
        frames += 1
        last = at
    }

    /// Completes the file; the last frame lasts until host time `time`.
    /// A writer that failed along the way leaves the file cut at its last
    /// whole fragment, and throws.
    func finish(at time: CMTime) async throws -> Doppel_Vmm_V1_RecordingStopped {
        input.markAsFinished()
        let end = CMTimeMaximum(CMTimeSubtract(time, origin), last)
        writer.endSession(atSourceTime: end)
        await writer.finishWriting()
        if writer.status == .failed {
            let reason = writer.error.map { String(describing: $0) } ?? "unknown"
            log("recording \(url.path) failed: \(reason)")
            throw RecordingError.writer(reason)
        }
        let bytes = (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? UInt64) ?? 0
        return .with {
            $0.path = url.path
            $0.frames = frames
            $0.seconds = end.seconds
            $0.bytes = bytes
        }
    }
}
