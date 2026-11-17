//! Frames into CoreVideo pixel buffers GPUI can draw: IOSurface-backed,
//! Metal-compatible NV12, from a pool so buffers are reused once GPUI
//! releases them. The only raw pointers in the viewer.

#![expect(
    unsafe_code,
    reason = "CoreVideo hands out plane base addresses as raw pointers"
)]

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_video::pixel_buffer::{
    CVPixelBuffer, CVPixelBufferKeys, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use core_video::pixel_buffer_pool::CVPixelBufferPool;
use core_video::r#return::kCVReturnSuccess;
use gpui::{AnyElement, IntoElement as _, ObjectFit, Styled as _, surface};
use objc2_core_foundation::CFRetained;
use std::ptr::NonNull;

use super::ports::{FrameRef, Plane};

/// A pixel buffer handed from a reader thread to the UI.
#[derive(Debug, Clone)]
pub(crate) struct Buffer(CVPixelBuffer);

// SAFETY: CVPixelBuffer is a reference-counted CoreFoundation object, safe
// to retain and release from any thread; the reader writes its pixels only
// before handing it over and never touches it after.
unsafe impl Send for Buffer {}

impl Buffer {
    /// A buffer the decoder filled, and won't touch again.
    pub(crate) const fn decoded(buffer: CVPixelBuffer) -> Self {
        Self(buffer)
    }

    #[cfg(test)]
    pub(crate) fn pixel_buffer(&self) -> CVPixelBuffer {
        self.0.clone()
    }

    /// The frame, fitted into its box.
    pub(crate) fn draw(&self) -> Option<AnyElement> {
        self.surface().map(|pixels| {
            surface(pixels)
                .object_fit(ObjectFit::Contain)
                .size_full()
                .into_any_element()
        })
    }

    /// The buffer as GPUI draws it, with a reference of its own.
    fn surface(&self) -> Option<CFRetained<objc2_core_video::CVPixelBuffer>> {
        let raw = self
            .0
            .as_concrete_TypeRef()
            .cast::<objc2_core_video::CVPixelBuffer>();
        // SAFETY: both types name the same CoreFoundation object; retaining
        // it gives the result its own reference, released on drop.
        NonNull::new(raw).map(|buffer| unsafe { CFRetained::retain(buffer) })
    }

    /// Width and height in pixels.
    pub(crate) fn size(&self) -> (usize, usize) {
        (self.0.get_width(), self.0.get_height())
    }
}

/// Why a frame couldn't be put in a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PixelError {
    #[error("CoreVideo couldn't make a pixel buffer pool ({0})")]
    Pool(i32),
    #[error("CoreVideo couldn't provide a pixel buffer ({0})")]
    Buffer(i32),
    #[error("CoreVideo couldn't lock a pixel buffer ({0})")]
    Lock(i32),
    #[error("the pixel buffer is smaller than the frame")]
    TooSmall,
}

/// Buffers of one frame size.
#[derive(Debug)]
pub(crate) struct Pixels {
    pool: CVPixelBufferPool,
    size: (u32, u32),
}

impl Pixels {
    pub(crate) fn new(width: u32, height: u32) -> Result<Self, PixelError> {
        let key = |key: CVPixelBufferKeys| CFString::from(key);
        let format = i64::from(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
        let attributes = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[
            (
                key(CVPixelBufferKeys::PixelFormatType),
                CFNumber::from(format).as_CFType(),
            ),
            (
                key(CVPixelBufferKeys::Width),
                CFNumber::from(i64::from(width)).as_CFType(),
            ),
            (
                key(CVPixelBufferKeys::Height),
                CFNumber::from(i64::from(height)).as_CFType(),
            ),
            (
                key(CVPixelBufferKeys::IOSurfaceProperties),
                CFDictionary::<CFString, CFType>::from_CFType_pairs(&[]).as_CFType(),
            ),
            (
                key(CVPixelBufferKeys::MetalCompatibility),
                CFBoolean::true_value().as_CFType(),
            ),
        ]);
        let pool = CVPixelBufferPool::new(None, Some(&attributes)).map_err(PixelError::Pool)?;
        Ok(Self {
            pool,
            size: (width, height),
        })
    }

    pub(crate) const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// A buffer holding `frame`, which must be this pool's size.
    pub(crate) fn fill(&self, frame: FrameRef<'_>) -> Result<Buffer, PixelError> {
        if (frame.width(), frame.height()) != self.size {
            return Err(PixelError::TooSmall);
        }
        let buffer = self
            .pool
            .create_pixel_buffer()
            .map_err(PixelError::Buffer)?;
        let locked = buffer.lock_base_address(0);
        if locked != kCVReturnSuccess {
            return Err(PixelError::Lock(locked));
        }
        let copied = [(0, Plane::Luma), (1, Plane::Chroma)]
            .into_iter()
            .try_for_each(|(index, plane)| Self::copy_plane(&buffer, index, frame, plane));
        buffer.unlock_base_address(0);
        copied.map(|()| Buffer(buffer))
    }

    fn copy_plane(
        buffer: &CVPixelBuffer,
        index: usize,
        frame: FrameRef<'_>,
        plane: Plane,
    ) -> Result<(), PixelError> {
        let rows = frame.rows(plane);
        let row_len = frame.width() as usize;
        let stride = buffer.get_bytes_per_row_of_plane(index);
        if stride < row_len || buffer.get_height_of_plane(index) < rows.len() {
            return Err(PixelError::TooSmall);
        }
        // SAFETY: the buffer is locked for writing; the plane has at least
        // `rows.len()` rows of `stride >= row_len` bytes (checked above), and
        // the frame's rows are distinct memory.
        unsafe {
            let base = buffer.get_base_address_of_plane(index).cast::<u8>();
            if base.is_null() {
                return Err(PixelError::TooSmall);
            }
            for (row, bytes) in rows.enumerate() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(row * stride), row_len);
            }
        }
        Ok(())
    }
}
