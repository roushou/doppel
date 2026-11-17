//! A decoded picture as GPUI draws it in the browser: an image of its own
//! on the GPU, which stays there until it's released.

use std::sync::Arc;

use gpui::{
    AnyElement, App, IntoElement as _, ObjectFit, RenderImage, Styled as _, StyledImage as _, img,
};
use image::{ImageBuffer, Rgba};

/// A picture handed from the browser to the window.
#[derive(Debug, Clone)]
pub(crate) struct Frame {
    image: Arc<RenderImage>,
    size: (usize, usize),
}

impl Frame {
    /// A picture from the browser's RGBA pixels; `None` when they aren't
    /// `width` × `height`.
    pub(crate) fn rgba(width: u32, height: u32, mut pixels: Vec<u8>) -> Option<Self> {
        // GPUI takes BGRA.
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        let buffer = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, pixels)?;
        Some(Self {
            image: Arc::new(RenderImage::new([image::Frame::new(buffer)])),
            size: (width as usize, height as usize),
        })
    }

    /// The picture, fitted into its box.
    pub(crate) fn draw(&self) -> Option<AnyElement> {
        Some(
            img(Arc::clone(&self.image))
                .object_fit(ObjectFit::Contain)
                .size_full()
                .into_any_element(),
        )
    }

    /// Width and height in pixels.
    pub(crate) const fn size(&self) -> (usize, usize) {
        self.size
    }

    /// Frees its image on the GPU: it's been replaced.
    pub(crate) fn release(self, cx: &mut App) {
        cx.drop_image(self.image, None);
    }
}
