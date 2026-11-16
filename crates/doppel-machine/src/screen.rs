//! Screen images and the coordinate space input uses.

use std::fmt;

use crate::geometry::Size;

/// How big a screenshot is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotSize {
    /// One image pixel per display point.
    MatchPoints,
    /// Its longest side at most this many pixels.
    MaxPixels(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
}

impl ImageFormat {
    #[must_use]
    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }
}

/// A captured display: encoded pixels and the point space they show.
#[derive(Clone, PartialEq)]
pub struct Screenshot {
    pub format: ImageFormat,
    pub width_pixels: u32,
    pub height_pixels: u32,
    /// The display's size in points: the coordinate space of
    /// [`crate::input::Input`].
    pub display: Size,
    pub data: Vec<u8>,
}

impl fmt::Debug for Screenshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Screenshot")
            .field("format", &self.format)
            .field("pixels", &(self.width_pixels, self.height_pixels))
            .field("display", &self.display)
            .field("bytes", &self.data.len())
            .finish()
    }
}

impl Screenshot {
    /// One line telling a model how pixels in this image map to input points.
    #[must_use]
    pub fn coordinate_note(&self) -> String {
        let scale = f64::from(self.width_pixels) / self.display.width();
        if (scale - 1.0).abs() < 0.01 {
            format!(
                "{}×{} px; image pixels are display points (input coordinates).",
                self.width_pixels, self.height_pixels
            )
        } else {
            format!(
                "{}×{} px of a {} display; divide image pixels by {scale:.3} to get input coordinates.",
                self.width_pixels, self.height_pixels, self.display
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(width_pixels: u32) -> Screenshot {
        Screenshot {
            format: ImageFormat::Png,
            width_pixels,
            height_pixels: width_pixels * 10 / 16,
            display: Size::new(1280.0, 800.0).unwrap(),
            data: vec![1, 2, 3],
        }
    }

    #[test]
    fn coordinate_note_states_the_scale() {
        assert!(shot(1280).coordinate_note().contains("are display points"));
        let retina = shot(2560).coordinate_note();
        assert!(retina.contains("divide image pixels by 2.000"), "{retina}");
    }

    #[test]
    fn debug_omits_pixel_data() {
        assert!(format!("{:?}", shot(1280)).contains("bytes: 3"));
    }
}
