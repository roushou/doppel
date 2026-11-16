// Encodes the display as an H.264 stream for viewers across a network.
//
// One per VMM, shared by every encoded connection. Tuned for latency, not
// for a file (that is the Recorder's job): the hardware encoder in real-time
// mode, no frame reordering, and key frames when a viewer needs one rather
// than on a schedule. Frames arrive when the screen changes, each stamped
// with the host time it was seen.

import CoreMedia
import CoreVideo
import DoppelWire
import Foundation
import IOSurface
import VideoToolbox

public enum StreamerError: Error, CustomStringConvertible {
    case encoder(String, OSStatus)

    public var description: String {
        switch self {
        case .encoder(let call, let status): "\(call) failed (\(status))"
        }
    }
}

/// Encodes frames given on one queue; `output` gets each encoded frame, in
/// order, on the encoder's thread, and `dropped` is called for a frame the
/// encoder dropped (its rate control does, after a large key frame): the
/// screen it showed must be encoded again, or a viewer keeps the frame
/// before it while the screen stays still.
final class Streamer: @unchecked Sendable {
    static let bitsPerSecond = 8_000_000
    /// A key frame at least this often, so a stream that lost one recovers
    /// even if no request reaches the encoder.
    static let keyFrameSeconds = 10
    /// AVCC length prefixes: the encoder's default, and what EncodedFrame says.
    static let lengthBytes: Int32 = 4

    private let session: VTCompressionSession
    private let transfer: VTPixelTransferSession
    private let output: @Sendable (Doppel_Vmm_V1_EncodedFrame) -> Void
    private let dropped: @Sendable () -> Void
    private let lock = NSLock()
    private var sequence: UInt64 = 0

    init(width: Int, height: Int, dropped: @escaping @Sendable () -> Void = {},
         output: @escaping @Sendable (Doppel_Vmm_V1_EncodedFrame) -> Void) throws {
        self.output = output
        self.dropped = dropped
        let source = [
            kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelBufferWidthKey: width,
            kCVPixelBufferHeightKey: height,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
        ] as CFDictionary
        // Low-latency rate control where the encoder offers it; else the
        // ordinary real-time encoder.
        let lowLatency = [kVTVideoEncoderSpecification_EnableLowLatencyRateControl: true] as CFDictionary
        var created: VTCompressionSession?
        var status = OSStatus(0)
        for specification in [lowLatency, nil] {
            status = VTCompressionSessionCreate(
                allocator: nil, width: Int32(width), height: Int32(height), codecType: kCMVideoCodecType_H264,
                encoderSpecification: specification, imageBufferAttributes: source,
                compressedDataAllocator: nil, outputCallback: nil, refcon: nil, compressionSessionOut: &created)
            if status == noErr { break }
        }
        guard status == noErr, let created else { throw StreamerError.encoder("VTCompressionSessionCreate", status) }
        session = created
        let properties: [CFString: Any] = [
            kVTCompressionPropertyKey_RealTime: true,
            kVTCompressionPropertyKey_AllowFrameReordering: false,
            kVTCompressionPropertyKey_ProfileLevel: kVTProfileLevel_H264_High_AutoLevel,
            kVTCompressionPropertyKey_AverageBitRate: Self.bitsPerSecond,
            kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration: Self.keyFrameSeconds,
            kVTCompressionPropertyKey_ExpectedFrameRate: 60,
        ]
        for (key, value) in properties {
            // Optional tuning: an encoder that refuses one still encodes.
            let set = VTSessionSetProperty(created, key: key, value: value as CFTypeRef)
            if set != noErr { log("the stream encoder refused \(key) (\(set))") }
        }
        VTCompressionSessionPrepareToEncodeFrames(created)

        var transfer: VTPixelTransferSession?
        status = VTPixelTransferSessionCreate(allocator: nil, pixelTransferSessionOut: &transfer)
        guard status == noErr, let transfer else {
            VTCompressionSessionInvalidate(created)
            throw StreamerError.encoder("VTPixelTransferSessionCreate", status)
        }
        self.transfer = transfer
    }

    deinit { VTCompressionSessionInvalidate(session) }

    /// Encodes the frame shown from host time `time` on; `key` forces a key
    /// frame. Copied out of `surface` first: VZ draws into it again while
    /// the encoder still works. A frame that can't be copied is skipped.
    func encode(_ surface: IOSurfaceRef, at time: CMTime, key: Bool) {
        guard let pool = VTCompressionSessionGetPixelBufferPool(session) else { return }
        var source: Unmanaged<CVPixelBuffer>?
        var target: CVPixelBuffer?
        guard CVPixelBufferCreateWithIOSurface(nil, surface, nil, &source) == kCVReturnSuccess,
              let source = source?.takeRetainedValue(),
              CVPixelBufferPoolCreatePixelBuffer(nil, pool, &target) == kCVReturnSuccess,
              let target,
              VTPixelTransferSessionTransferImage(transfer, from: source, to: target) == noErr
        else { return }
        let options = key ? [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary : nil
        VTCompressionSessionEncodeFrame(session, imageBuffer: target, presentationTimeStamp: time, duration: .invalid,
                                        frameProperties: options, infoFlagsOut: nil) { [self] status, flags, sample in
            guard status == noErr, !flags.contains(.frameDropped), let sample, let frame = Self.frame(of: sample) else {
                log("the stream encoder dropped a frame (status \(status), flags \(flags.rawValue))")
                dropped()
                return
            }
            emit(frame)
        }
    }

    /// Waits for every frame given so far to be output.
    func flush() {
        VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
    }

    private func emit(_ frame: Doppel_Vmm_V1_EncodedFrame) {
        var frame = frame
        // Under the lock: numbered and output in the encoder's order.
        lock.withLock {
            sequence += 1
            frame.sequence = sequence
            output(frame)
        }
    }

    /// An encoded sample as a frame, without its sequence. Nil when it
    /// isn't AVCC with 4-byte lengths or holds no data.
    static func frame(of sample: CMSampleBuffer) -> Doppel_Vmm_V1_EncodedFrame? {
        guard let block = CMSampleBufferGetDataBuffer(sample), let format = CMSampleBufferGetFormatDescription(sample)
        else { return nil }
        let length = CMBlockBufferGetDataLength(block)
        guard length > 0 else { return nil }
        var data = Data(count: length)
        let copied = data.withUnsafeMutableBytes { bytes -> OSStatus in
            guard let base = bytes.baseAddress else { return kCMBlockBufferBadPointerParameterErr }
            return CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: length, destination: base)
        }
        guard copied == kCMBlockBufferNoErr else { return nil }

        let key = !isNotSync(sample)
        var sets: [Data] = []
        if key {
            var count = 0
            var prefix: Int32 = 0
            guard CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                format, parameterSetIndex: 0, parameterSetPointerOut: nil, parameterSetSizeOut: nil,
                parameterSetCountOut: &count, nalUnitHeaderLengthOut: &prefix) == noErr,
                prefix == lengthBytes
            else { return nil }
            for index in 0..<count {
                var pointer: UnsafePointer<UInt8>?
                var size = 0
                guard CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    format, parameterSetIndex: index, parameterSetPointerOut: &pointer, parameterSetSizeOut: &size,
                    parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil) == noErr,
                    let pointer
                else { return nil }
                sets.append(Data(bytes: pointer, count: size))
            }
        }
        return .with {
            $0.key = key
            $0.parameterSets = sets
            $0.data = data
        }
    }

    /// Whether the sample depends on earlier ones: its NotSync attachment.
    private static func isNotSync(_ sample: CMSampleBuffer) -> Bool {
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false)
            as? [[CFString: Any]], let first = attachments.first
        else { return false }
        return first[kCMSampleAttachmentKey_NotSync] as? Bool ?? false
    }
}
