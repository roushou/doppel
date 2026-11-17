//! The desktop's screen ports: where a screen's frames come from, raw or
//! encoded, and where its input and requests for control go. Each is called
//! on a thread of its own, and may block.

use doppel_machine::geometry::Size;
use doppel_scene::encoded::EncodedFrame;

use crate::{GuestInput, SourceError};

/// A frame lent by a source: 4:2:0 bi-planar YCbCr (NV12), planes packed
/// without row padding. Checked once, when made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRef<'a> {
    width: u32,
    height: u32,
    luma: &'a [u8],
    chroma: &'a [u8],
}

/// Why planes don't make a frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("a {width}x{height} frame: NV12 needs even, non-zero sides")]
    Size { width: u32, height: u32 },
    #[error("the {plane} plane has {actual} bytes, not {expected}")]
    PlaneLength {
        plane: &'static str,
        expected: usize,
        actual: usize,
    },
}

impl<'a> FrameRef<'a> {
    /// `luma`: `width` bytes per row, `height` rows. `chroma`: Cb, Cr pairs,
    /// `width` bytes per row, `height / 2` rows.
    ///
    /// # Errors
    /// See [`FrameError`].
    pub fn new(
        width: u32,
        height: u32,
        luma: &'a [u8],
        chroma: &'a [u8],
    ) -> Result<Self, FrameError> {
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(FrameError::Size { width, height });
        }
        let pixels = width as usize * height as usize;
        for (plane, expected, actual) in [
            ("luma", pixels, luma.len()),
            ("chroma", pixels / 2, chroma.len()),
        ] {
            if actual != expected {
                return Err(FrameError::PlaneLength {
                    plane,
                    expected,
                    actual,
                });
            }
        }
        Ok(Self {
            width,
            height,
            luma,
            chroma,
        })
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The rows of a plane, top to bottom.
    pub(crate) fn rows(&self, plane: Plane) -> std::slice::ChunksExact<'a, u8> {
        match plane {
            Plane::Luma => self.luma.chunks_exact(self.width as usize),
            Plane::Chroma => self.chroma.chunks_exact(self.width as usize),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Plane {
    Luma,
    Chroma,
}

/// What a source has next.
#[derive(Debug)]
pub enum Next<'a> {
    Frame(FrameRef<'a>),
    /// Nothing changed for a while: ask again.
    Unchanged,
    /// The source is done (the machine stopped).
    Ended,
}

/// Where a screen's frames come from. Called on a thread of its own, so it
/// may block, but should return [`Next::Unchanged`] within about a second.
pub trait FrameSource: Send + 'static {
    /// # Errors
    /// The source failed; its screen shows the error and reads no more.
    fn next_frame(&mut self) -> Result<Next<'_>, SourceError>;
}

/// What an encoded source has next.
#[derive(Debug)]
pub enum NextEncoded {
    Frame(EncodedFrame),
    /// Nothing changed for a while: ask again.
    Unchanged,
    /// The source is done (the machine stopped).
    Ended,
}

/// Where a screen's H.264 frames come from, in order. Called on a thread of
/// its own, as [`FrameSource`] is.
pub trait EncodedSource: Send + 'static {
    /// # Errors
    /// The source failed; its screen shows the error and reads no more.
    fn next_frame(&mut self) -> Result<NextEncoded, SourceError>;
}

/// Where a screen's input goes. Called on a thread of its own, one input
/// at a time, in the order the person gave them.
pub trait InputSink: Send + 'static {
    /// # Errors
    /// The input wasn't delivered; the screen shows why, and later input is
    /// still sent.
    fn send(&mut self, input: GuestInput) -> Result<(), SourceError>;
}

/// Taking control of a screen's machine from whoever drives it (an agent),
/// and handing it back. Called from the screen's input thread, in order
/// with its input.
pub trait Takeover: Send + 'static {
    /// # Errors
    /// Control wasn't given (someone else has it); the screen shows why.
    fn take(&mut self) -> Result<(), SourceError>;
    /// # Errors
    /// Control couldn't be handed back; the screen shows why.
    fn hand_back(&mut self) -> Result<(), SourceError>;
}

/// One machine's display: its frames, its size in points, and where its
/// input goes.
#[derive(Debug)]
pub struct Screen {
    pub(crate) source: Frames,
    pub(crate) points: Option<Size>,
    pub(crate) input: Option<Box<dyn InputSink>>,
    pub(crate) takeover: Option<Box<dyn Takeover>>,
}

impl std::fmt::Debug for dyn Takeover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Takeover")
    }
}

impl std::fmt::Debug for dyn InputSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InputSink")
    }
}

/// Where a screen's frames come from: a closed set, read by its view.
#[derive(Debug)]
pub(crate) enum Frames {
    Raw(Box<dyn FrameSource>),
    Encoded(Box<dyn EncodedSource>),
}

impl std::fmt::Debug for dyn EncodedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EncodedSource")
    }
}

impl std::fmt::Debug for dyn FrameSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FrameSource")
    }
}

impl Screen {
    pub fn new(source: impl FrameSource) -> Self {
        Self {
            source: Frames::Raw(Box::new(source)),
            points: None,
            input: None,
            takeover: None,
        }
    }

    /// A screen whose frames come encoded, and are decoded here.
    pub fn encoded(source: impl EncodedSource) -> Self {
        Self {
            source: Frames::Encoded(Box::new(source)),
            points: None,
            input: None,
            takeover: None,
        }
    }

    /// The display's size in points: where what agents do is drawn over it.
    /// Without it, the screen shows no overlay.
    #[must_use]
    pub const fn with_points(mut self, points: Size) -> Self {
        self.points = Some(points);
        self
    }

    /// Sends what the person does on this screen to `sink`. Without one,
    /// the screen is view-only.
    #[must_use]
    pub fn with_input(mut self, sink: impl InputSink) -> Self {
        self.input = Some(Box::new(sink));
        self
    }

    /// Makes input wait for control: the screen is view-only until the
    /// person presses "Take control", and sends input until they hand it
    /// back. Without one, input is always sent.
    #[must_use]
    pub fn with_takeover(mut self, takeover: impl Takeover) -> Self {
        self.takeover = Some(Box::new(takeover));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planes_must_match_the_frame_size() {
        let luma = [0_u8; 8];
        let chroma = [0_u8; 4];
        let frame = FrameRef::new(4, 2, &luma, &chroma).unwrap();
        assert_eq!(frame.rows(Plane::Luma).count(), 2);
        assert_eq!(frame.rows(Plane::Chroma).count(), 1);
        assert_eq!(
            FrameRef::new(3, 2, &luma, &chroma),
            Err(FrameError::Size {
                width: 3,
                height: 2
            })
        );
        assert_eq!(
            FrameRef::new(4, 2, &luma[..7], &chroma),
            Err(FrameError::PlaneLength {
                plane: "luma",
                expected: 8,
                actual: 7
            })
        );
        assert_eq!(
            FrameRef::new(4, 2, &luma, &chroma[..2]),
            Err(FrameError::PlaneLength {
                plane: "chroma",
                expected: 4,
                actual: 2
            })
        );
    }
}
