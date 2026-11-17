//! H.264 frames into pixel buffers GPUI can draw, with VideoToolbox. The
//! decoder writes IOSurface-backed NV12 straight into buffers of its own,
//! so a decoded frame goes to the pane with no copy. Decoding is
//! synchronous, on the reader's thread.

#![expect(
    unsafe_code,
    reason = "VideoToolbox and CoreMedia are C APIs: raw pointers, a C callback, and CF objects shared with core-video"
)]

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::{Mutex, PoisonError};

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_video::pixel_buffer::{
    CVPixelBuffer, CVPixelBufferKeys, CVPixelBufferRef,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime,
    CMVideoFormatDescriptionCreateFromH264ParameterSets, kCMBlockBufferAssureMemoryNowFlag,
};
use objc2_core_video::CVImageBuffer;
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession,
};

use super::pixels::Buffer;

/// Why a frame wasn't decoded; each carries VideoToolbox's or CoreMedia's
/// status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DecodeError {
    #[error("the stream's parameter sets were refused ({0})")]
    Format(i32),
    #[error("VideoToolbox couldn't make a decoder ({0})")]
    Session(i32),
    #[error("CoreMedia couldn't hold a frame ({0})")]
    Sample(i32),
    #[error("a frame failed to decode ({0})")]
    Decode(i32),
}

/// What the decoder's callback left for the frame being decoded.
type Slot = Mutex<Option<Result<CVPixelBuffer, i32>>>;

/// A decoder for one stream's parameter sets.
pub(crate) struct Decoder {
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
    parameter_sets: Vec<Vec<u8>>,
    /// Where the callback puts each frame. Boxed: the session holds its
    /// address until invalidated, in `drop`.
    output: Box<Slot>,
}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder").finish_non_exhaustive()
    }
}

impl Decoder {
    /// AVCC length prefixes, as `EncodedFrame` has them.
    const LENGTH_BYTES: i32 = 4;

    /// A decoder for frames that follow `parameter_sets` (SPS, then PPS).
    pub(crate) fn new(parameter_sets: &[Vec<u8>]) -> Result<Self, DecodeError> {
        let pointers = parameter_sets
            .iter()
            .map(|set| NonNull::new(set.as_ptr().cast_mut()))
            .collect::<Option<Vec<_>>>()
            .ok_or(DecodeError::Format(0))?;
        let sizes: Vec<usize> = parameter_sets.iter().map(Vec::len).collect();
        let (Some(pointers_at), Some(sizes_at)) = (
            NonNull::new(pointers.as_ptr().cast_mut()),
            NonNull::new(sizes.as_ptr().cast_mut()),
        ) else {
            return Err(DecodeError::Format(0));
        };
        let mut format: *const CMFormatDescription = ptr::null();
        // SAFETY: `pointers` and `sizes` hold one entry per parameter set,
        // each pointing at that many bytes, alive for the call; CoreMedia
        // copies them. The out pointer is a local.
        let status = unsafe {
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                None,
                parameter_sets.len(),
                pointers_at,
                sizes_at,
                Self::LENGTH_BYTES,
                NonNull::from(&mut format),
            )
        };
        let format = NonNull::new(format.cast_mut())
            .filter(|_| status == 0)
            // SAFETY: a Create function's result: we own its one reference.
            .map(|format| unsafe { CFRetained::from_raw(format) })
            .ok_or(DecodeError::Format(status))?;

        let output: Box<Slot> = Box::new(Mutex::new(None));
        let callback = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(Self::decoded),
            decompressionOutputRefCon: ptr::from_ref::<Slot>(&output).cast_mut().cast(),
        };
        let attributes = Self::destination();
        // SAFETY: a CFDictionary from core-foundation is the same CF object
        // objc2 names; it lives for the call, which retains it.
        let attributes = unsafe {
            &*attributes
                .as_concrete_TypeRef()
                .cast::<objc2_core_foundation::CFDictionary>()
        };
        let mut session: *mut VTDecompressionSession = ptr::null_mut();
        // SAFETY: the record is read during the call; its refcon points at
        // the boxed slot, which outlives the session (see `drop`).
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                None,
                Some(attributes),
                &callback,
                NonNull::from(&mut session),
            )
        };
        let session = NonNull::new(session)
            .filter(|_| status == 0)
            // SAFETY: a Create function's result: we own its one reference.
            .map(|session| unsafe { CFRetained::from_raw(session) })
            .ok_or(DecodeError::Session(status))?;
        Ok(Self {
            session,
            format,
            parameter_sets: parameter_sets.to_vec(),
            output,
        })
    }

    /// Whether this decoder is for these parameter sets.
    pub(crate) fn decodes(&self, parameter_sets: &[Vec<u8>]) -> bool {
        self.parameter_sets == parameter_sets
    }

    /// Decodes one frame's AVCC data. `None` when the decoder dropped it
    /// without an error.
    pub(crate) fn decode(&mut self, data: &[u8]) -> Result<Option<Buffer>, DecodeError> {
        let sample = self.sample(data)?;
        *self.output.lock().unwrap_or_else(PoisonError::into_inner) = None;
        // SAFETY: the session and sample are valid; no per-frame refcon.
        // Without the asynchronous flag the callback runs before this
        // returns, and the wait covers a decoder that defers it anyway.
        let status = unsafe {
            let status = self.session.decode_frame(
                &sample,
                VTDecodeFrameFlags(0),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            self.session.wait_for_asynchronous_frames();
            status
        };
        if status != 0 {
            return Err(DecodeError::Decode(status));
        }
        match self
            .output
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            Some(Ok(buffer)) => Ok(Some(Buffer::decoded(buffer))),
            Some(Err(status)) => Err(DecodeError::Decode(status)),
            None => Ok(None),
        }
    }

    /// `data` as a ready sample buffer of one frame, copied.
    fn sample(&self, data: &[u8]) -> Result<CFRetained<CMSampleBuffer>, DecodeError> {
        let source = NonNull::new(data.as_ptr().cast_mut().cast::<c_void>())
            .ok_or(DecodeError::Sample(0))?;
        let mut block: *mut CMBlockBuffer = ptr::null_mut();
        // SAFETY: a null memory block asks CoreMedia to allocate `len`
        // bytes now; the out pointer is a local.
        let status = unsafe {
            CMBlockBuffer::create_with_memory_block(
                None,
                ptr::null_mut(),
                data.len(),
                None,
                ptr::null(),
                0,
                data.len(),
                kCMBlockBufferAssureMemoryNowFlag,
                NonNull::from(&mut block),
            )
        };
        let block = NonNull::new(block)
            .filter(|_| status == 0)
            // SAFETY: a Create function's result: we own its one reference.
            .map(|block| unsafe { CFRetained::from_raw(block) })
            .ok_or(DecodeError::Sample(status))?;
        // SAFETY: `source` points at `data.len()` bytes; the block holds as
        // many, allocated above.
        let status = unsafe { CMBlockBuffer::replace_data_bytes(source, &block, 0, data.len()) };
        if status != 0 {
            return Err(DecodeError::Sample(status));
        }
        let size = data.len();
        let mut sample: *mut CMSampleBuffer = ptr::null_mut();
        // SAFETY: one sample of `size` bytes, no timing; the block and
        // format are valid, and the out pointer is a local.
        let status = unsafe {
            CMSampleBuffer::create_ready(
                None,
                Some(&block),
                Some(&self.format),
                1,
                0,
                ptr::null(),
                1,
                &size,
                NonNull::from(&mut sample),
            )
        };
        NonNull::new(sample)
            .filter(|_| status == 0)
            // SAFETY: a Create function's result: we own its one reference.
            .map(|sample| unsafe { CFRetained::from_raw(sample) })
            .ok_or(DecodeError::Sample(status))
    }

    /// What the decoder makes: NV12 full range (as raw frames are), on
    /// IOSurfaces GPUI's Metal renderer can use.
    pub(crate) fn destination() -> CFDictionary<CFString, CFType> {
        let key = |key: CVPixelBufferKeys| CFString::from(key);
        CFDictionary::from_CFType_pairs(&[
            (
                key(CVPixelBufferKeys::PixelFormatType),
                CFNumber::from(i64::from(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange))
                    .as_CFType(),
            ),
            (
                key(CVPixelBufferKeys::IOSurfaceProperties),
                CFDictionary::<CFString, CFType>::from_CFType_pairs(&[]).as_CFType(),
            ),
            (
                key(CVPixelBufferKeys::MetalCompatibility),
                CFBoolean::true_value().as_CFType(),
            ),
        ])
    }

    /// The session's output callback: keeps the decoded buffer, or the
    /// failure, in the slot `refcon` points at.
    unsafe extern "C-unwind" fn decoded(
        refcon: *mut c_void,
        _frame: *mut c_void,
        status: i32,
        _flags: VTDecodeInfoFlags,
        image: *mut CVImageBuffer,
        _presented: CMTime,
        _duration: CMTime,
    ) {
        // SAFETY: the session was made with the address of a `Slot` that
        // outlives it.
        let Some(slot) = (unsafe { refcon.cast::<Slot>().as_ref() }) else {
            return;
        };
        let result = if status != 0 {
            Some(Err(status))
        } else if image.is_null() {
            // Dropped by the decoder.
            None
        } else {
            // SAFETY: a CVImageBuffer from the decoder is a CVPixelBuffer;
            // the get rule retains it past this call.
            Some(Ok(unsafe {
                CVPixelBuffer::wrap_under_get_rule(image.cast::<c_void>() as CVPixelBufferRef)
            }))
        };
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = result;
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: after this, the session calls back no more, so the slot
        // may go with the rest of the fields.
        unsafe {
            self.session.wait_for_asynchronous_frames();
            self.session.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of fixtures/h264/shades.avcc (format in the Swift test that
    /// writes it): whether it's a key frame, its parameter sets, its data.
    type Fixture = (bool, Vec<Vec<u8>>, Vec<u8>);

    fn fixture() -> Vec<Fixture> {
        let bytes = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/h264/shades.avcc"),
        )
        .unwrap();
        let mut rest = bytes.as_slice();
        let mut take = |count: usize| {
            let (taken, left) = rest.split_at(count);
            rest = left;
            taken.to_vec()
        };
        let mut frames = Vec::new();
        // Three frames, as written.
        for _ in 0..3 {
            let key = take(1)[0] == 1;
            let count = take(1)[0];
            let sets = (0..count)
                .map(|_| {
                    let length = u16::from_be_bytes(take(2).try_into().unwrap());
                    take(usize::from(length))
                })
                .collect();
            let length = u32::from_be_bytes(take(4).try_into().unwrap());
            frames.push((key, sets, take(length as usize)));
        }
        assert!(take(0).is_empty() && rest.is_empty(), "nothing after them");
        frames
    }

    /// The mean of the luma plane.
    fn mean_luma(buffer: &Buffer) -> f64 {
        let pixels = buffer.pixel_buffer();
        assert_eq!(pixels.lock_base_address(1), 0);
        let (width, height) = (pixels.get_width(), pixels.get_height());
        let stride = pixels.get_bytes_per_row_of_plane(0);
        // SAFETY: locked for reading; the plane holds `height` rows of
        // `stride >= width` bytes.
        let sum: u64 = unsafe {
            let base = pixels.get_base_address_of_plane(0).cast::<u8>();
            (0..height)
                .flat_map(|row| (0..width).map(move |column| (row, column)))
                .map(|(row, column)| u64::from(*base.add(row * stride + column)))
                .sum()
        };
        pixels.unlock_base_address(1);
        sum as f64 / (width * height) as f64
    }

    #[test]
    fn a_stream_decodes_into_frames_of_its_size_and_content() {
        let frames = fixture();
        assert_eq!(frames.len(), 3);
        let (key, sets, _) = &frames[0];
        assert!(key);
        let mut decoder = Decoder::new(sets).unwrap();
        assert!(decoder.decodes(sets));
        assert!(!decoder.decodes(&[]));
        for ((_, _, data), shade) in frames.iter().zip([40.0, 120.0, 200.0]) {
            let buffer = decoder.decode(data).unwrap().unwrap();
            assert_eq!(buffer.size(), (64, 64));
            let mean = mean_luma(&buffer);
            assert!((mean - shade).abs() < 12.0, "{mean} for {shade}");
        }
    }

    #[test]
    fn a_delta_s_bytes_alone_make_no_decoder_and_garbage_fails_to_decode() {
        let frames = fixture();
        assert!(matches!(
            Decoder::new(&[vec![0x65, 0x88]]),
            Err(DecodeError::Format(_))
        ));
        let mut decoder = Decoder::new(&frames[0].1).unwrap();
        assert!(decoder.decode(&[0, 0, 0, 2, 0xff, 0xff]).is_err());
    }
}
