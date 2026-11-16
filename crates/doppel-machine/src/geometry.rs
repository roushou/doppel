//! Places and extents on a machine's display, in display points with a
//! top-left origin: the coordinate space input uses, accessibility frames
//! are reported in, and text is recognized in.
//!
//! Each type is checked once, when made: past that, every coordinate is
//! finite and every size non-negative, so nothing downstream re-checks.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum GeometryError {
    #[error("coordinates must be finite and non-negative (got {x}, {y})")]
    Point { x: f64, y: f64 },
    #[error("a size must be finite and non-negative (got {width}×{height})")]
    Size { width: f64, height: f64 },
    #[error("a rectangle's origin must be finite (got {x}, {y})")]
    Origin { x: f64, y: f64 },
}

/// A point on the display: finite, and never left of or above its
/// top-left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    x: f64,
    y: f64,
}

impl Point {
    /// # Errors
    /// [`GeometryError::Point`].
    pub fn new(x: f64, y: f64) -> Result<Self, GeometryError> {
        if x.is_finite() && y.is_finite() && x >= 0.0 && y >= 0.0 {
            Ok(Self { x, y })
        } else {
            Err(GeometryError::Point { x, y })
        }
    }

    /// The nearest point on the display to (`x`, `y`): negative coordinates
    /// become 0. `None` if either isn't finite.
    #[must_use]
    pub fn clamped(x: f64, y: f64) -> Option<Self> {
        (x.is_finite() && y.is_finite()).then_some(Self {
            x: x.max(0.0),
            y: y.max(0.0),
        })
    }

    #[must_use]
    pub const fn x(self) -> f64 {
        self.x
    }

    #[must_use]
    pub const fn y(self) -> f64 {
        self.y
    }
}

/// A width and a height: finite and non-negative.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    width: f64,
    height: f64,
}

impl Size {
    /// # Errors
    /// [`GeometryError::Size`].
    pub fn new(width: f64, height: f64) -> Result<Self, GeometryError> {
        if width.is_finite() && height.is_finite() && width >= 0.0 && height >= 0.0 {
            Ok(Self { width, height })
        } else {
            Err(GeometryError::Size { width, height })
        }
    }

    /// A whole number of points each way, at `scale` pixels per point.
    #[must_use]
    pub fn of_pixels(width: u32, height: u32, scale: Scale) -> Self {
        let per_point = scale.pixels_per_point();
        Self {
            width: f64::from(width) / per_point,
            height: f64::from(height) / per_point,
        }
    }

    #[must_use]
    pub const fn width(self) -> f64 {
        self.width
    }

    #[must_use]
    pub const fn height(self) -> f64 {
        self.height
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}×{} pt", self.width, self.height)
    }
}

/// Pixels per display point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    /// One pixel per point.
    Standard,
    /// Two pixels per point: macOS's HiDPI modes.
    Retina,
}

impl Scale {
    const fn pixels_per_point(self) -> f64 {
        match self {
            Self::Standard => 1.0,
            Self::Retina => 2.0,
        }
    }
}

/// A rectangle on the display. Its origin may lie a little outside the
/// display (a window or a recognized box can overhang an edge); its size
/// is a [`Size`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    x: f64,
    y: f64,
    size: Size,
}

impl Rect {
    /// # Errors
    /// [`GeometryError::Origin`] or [`GeometryError::Size`].
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Result<Self, GeometryError> {
        if !(x.is_finite() && y.is_finite()) {
            return Err(GeometryError::Origin { x, y });
        }
        Ok(Self {
            x,
            y,
            size: Size::new(width, height)?,
        })
    }

    #[must_use]
    pub const fn left(self) -> f64 {
        self.x
    }

    #[must_use]
    pub const fn top(self) -> f64 {
        self.y
    }

    #[must_use]
    pub const fn width(self) -> f64 {
        self.size.width
    }

    #[must_use]
    pub const fn height(self) -> f64 {
        self.size.height
    }

    #[must_use]
    pub fn right(self) -> f64 {
        self.x + self.size.width
    }

    #[must_use]
    pub fn bottom(self) -> f64 {
        self.y + self.size.height
    }

    #[must_use]
    pub const fn size(self) -> Size {
        self.size
    }

    /// Whether it covers any of the display: both sides longer than zero.
    #[must_use]
    pub fn has_area(self) -> bool {
        self.size.width > 0.0 && self.size.height > 0.0
    }

    /// Whether `at` lies in it, its edges included.
    #[must_use]
    pub fn contains(self, at: Point) -> bool {
        (self.x..=self.right()).contains(&at.x) && (self.y..=self.bottom()).contains(&at.y)
    }

    /// Its centre, kept on the display: the point to click.
    #[must_use]
    pub fn center(self) -> Point {
        Point {
            x: (self.x + self.size.width / 2.0).max(0.0),
            y: (self.y + self.size.height / 2.0).max(0.0),
        }
    }

    /// The smallest rectangle holding both.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        let (left, top) = (self.x.min(other.x), self.y.min(other.y));
        Self {
            x: left,
            y: top,
            size: Size {
                width: self.right().max(other.right()) - left,
                height: self.bottom().max(other.bottom()) - top,
            },
        }
    }

    /// Where both overlap, if they share any area.
    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        let (left, top) = (self.x.max(other.x), self.y.max(other.y));
        let (right, bottom) = (
            self.right().min(other.right()),
            self.bottom().min(other.bottom()),
        );
        (right > left && bottom > top).then_some(Self {
            x: left,
            y: top,
            size: Size {
                width: right - left,
                height: bottom - top,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_are_finite_and_on_the_display() {
        assert!(Point::new(-1.0, 0.0).is_err());
        assert!(Point::new(f64::NAN, 0.0).is_err());
        assert_eq!(Point::new(1.5, 2.0).map(|p| (p.x(), p.y())), Ok((1.5, 2.0)));
        assert_eq!(
            Point::clamped(-3.0, 4.0).map(|p| (p.x(), p.y())),
            Some((0.0, 4.0))
        );
        assert_eq!(Point::clamped(f64::INFINITY, 4.0), None);
    }

    #[test]
    fn sizes_are_finite_and_non_negative() {
        assert!(Size::new(-1.0, 1.0).is_err());
        assert!(Size::new(1.0, f64::NAN).is_err());
        assert_eq!(Size::new(0.0, 2.0).map(Size::height), Ok(2.0));
        assert_eq!(
            Size::of_pixels(2560, 1600, Scale::Retina).to_string(),
            "1280×800 pt"
        );
    }

    #[test]
    fn rectangles_may_overhang_but_not_turn_inside_out() {
        assert!(Rect::new(-2.0, 0.0, 1.0, 1.0).is_ok());
        assert!(Rect::new(0.0, 0.0, -1.0, 1.0).is_err());
        assert!(Rect::new(f64::NAN, 0.0, 1.0, 1.0).is_err());
        assert!(!Rect::new(0.0, 0.0, 0.0, 1.0).unwrap().has_area());
    }

    #[test]
    fn a_centre_off_the_display_is_brought_onto_it() {
        let rect = Rect::new(-10.0, 4.0, 4.0, 2.0).unwrap();
        assert_eq!(rect.center(), Point::new(0.0, 5.0).unwrap());
    }

    #[test]
    fn union_and_intersection() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0).unwrap();
        let b = Rect::new(5.0, 5.0, 10.0, 10.0).unwrap();
        assert_eq!(a.union(b), Rect::new(0.0, 0.0, 15.0, 15.0).unwrap());
        assert_eq!(a.intersection(b), Rect::new(5.0, 5.0, 5.0, 5.0).ok());
        assert!(a.contains(Point::new(10.0, 0.0).unwrap()));
        assert!(!a.contains(Point::new(10.5, 0.0).unwrap()));
        let apart = Rect::new(20.0, 0.0, 1.0, 1.0).unwrap();
        assert_eq!(a.intersection(apart), None);
    }
}
