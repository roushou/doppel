//! A mark an agent puts on a machine's screen for the people watching: a
//! box, a dot or an arrow on an element, a point or an area, with a short
//! note, for a while. It changes nothing on the machine; consoles draw it.

use std::time::Duration;

use crate::ax::tree::ElementTarget;
use crate::geometry::{Point, Rect};

/// The longest a note may be, in characters: a caption, not a message.
pub const NOTE_MAX_CHARS: usize = 200;
/// How long a mark lasts when the agent doesn't say.
pub const DEFAULT_LASTS: Duration = Duration::from_secs(5);
/// The longest a mark may last.
pub const MAX_LASTS: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MarkError {
    #[error("an area needs a finite, non-negative origin and a positive width and height")]
    InvalidArea,
    #[error("a note is at most {NOTE_MAX_CHARS} characters (got {0})")]
    NoteTooLong(usize),
    #[error("a mark lasts from 1 to 60 seconds (got {0} ms)")]
    Lasts(u64),
}

/// An area of the display to mark: a [`Rect`] that starts on the display
/// (an agent names it, so it can't overhang an edge) and has area.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area(Rect);

impl Area {
    /// # Errors
    /// [`MarkError::InvalidArea`].
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Result<Self, MarkError> {
        Rect::new(x, y, width, height)
            .ok()
            .filter(|rect| rect.left() >= 0.0 && rect.top() >= 0.0 && rect.has_area())
            .map(Self)
            .ok_or(MarkError::InvalidArea)
    }

    #[must_use]
    pub const fn rect(self) -> Rect {
        self.0
    }
}

/// What a mark is on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MarkAt {
    Point(Point),
    Area(Area),
    /// An element of a snapshot the agent saw; the daemon finds its frame.
    Element(ElementTarget),
}

/// How a mark is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    /// Outlines what it's on.
    Box,
    /// A dot on what it's on.
    Dot,
    /// An arrow to what it's on, from `from` when given, else from where
    /// the console finds room.
    Arrow { from: Option<Point> },
}

/// How long a mark stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lasts {
    /// For this long.
    For(Duration),
    /// Until the session's next call that acts on the machine's screen, or
    /// this long at most.
    UntilActed(Duration),
}

impl Lasts {
    /// # Errors
    /// [`MarkError::Lasts`] outside 1 s ..= [`MAX_LASTS`].
    pub fn checked(self) -> Result<Self, MarkError> {
        let (Self::For(at_most) | Self::UntilActed(at_most)) = self;
        if (Duration::from_secs(1)..=MAX_LASTS).contains(&at_most) {
            Ok(self)
        } else {
            Err(MarkError::Lasts(
                u64::try_from(at_most.as_millis()).unwrap_or(u64::MAX),
            ))
        }
    }

    /// The longest it can stay.
    #[must_use]
    pub const fn at_most(self) -> Duration {
        let (Self::For(at_most) | Self::UntilActed(at_most)) = self;
        at_most
    }
}

/// A short note shown by a mark: trimmed, not empty, at most
/// [`NOTE_MAX_CHARS`] characters, one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note(String);

impl Note {
    /// `None` for blank text.
    ///
    /// # Errors
    /// [`MarkError::NoteTooLong`].
    pub fn new(text: &str) -> Result<Option<Self>, MarkError> {
        let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let chars = line.chars().count();
        if chars > NOTE_MAX_CHARS {
            Err(MarkError::NoteTooLong(chars))
        } else if line.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Self(line)))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A mark, as an agent asks for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    at: MarkAt,
    shape: Shape,
    note: Option<Note>,
    lasts: Lasts,
}

impl Mark {
    /// # Errors
    /// [`MarkError::Lasts`] for a lifetime out of range.
    pub fn new(
        at: MarkAt,
        shape: Shape,
        note: Option<Note>,
        lasts: Lasts,
    ) -> Result<Self, MarkError> {
        Ok(Self {
            at,
            shape,
            note,
            lasts: lasts.checked()?,
        })
    }

    #[must_use]
    pub const fn at(&self) -> MarkAt {
        self.at
    }
    #[must_use]
    pub const fn shape(&self) -> Shape {
        self.shape
    }
    #[must_use]
    pub const fn note(&self) -> Option<&Note> {
        self.note.as_ref()
    }
    #[must_use]
    pub const fn lasts(&self) -> Lasts {
        self.lasts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_are_one_trimmed_line_and_bounded() {
        assert_eq!(Note::new("  \n ").unwrap(), None);
        assert_eq!(
            Note::new(" the  Save\nbutton ").unwrap().unwrap().as_str(),
            "the Save button"
        );
        assert_eq!(
            Note::new(&"x".repeat(NOTE_MAX_CHARS + 1)),
            Err(MarkError::NoteTooLong(NOTE_MAX_CHARS + 1))
        );
        assert!(Note::new(&"é".repeat(NOTE_MAX_CHARS)).unwrap().is_some());
    }

    #[test]
    fn lifetimes_are_one_second_to_a_minute() {
        assert!(Lasts::For(Duration::from_millis(999)).checked().is_err());
        assert!(
            Lasts::UntilActed(MAX_LASTS + Duration::from_millis(1))
                .checked()
                .is_err()
        );
        assert_eq!(Lasts::For(MAX_LASTS).checked(), Ok(Lasts::For(MAX_LASTS)));
        assert_eq!(Lasts::UntilActed(DEFAULT_LASTS).at_most(), DEFAULT_LASTS);
    }

    #[test]
    fn areas_have_size() {
        assert!(Area::new(1.0, 2.0, 0.0, 4.0).is_err());
        assert!(Area::new(-1.0, 2.0, 3.0, 4.0).is_err());
        assert!(Area::new(1.0, 2.0, f64::NAN, 4.0).is_err());
        assert_eq!(
            Area::new(1.0, 2.0, 3.0, 4.0).unwrap().rect(),
            Rect::new(1.0, 2.0, 3.0, 4.0).unwrap()
        );
    }
}
