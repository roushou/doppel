//! Input for a screen, in the viewer's own terms: positions are fractions
//! of the screen, so the viewer needn't know the guest's size, and keys are
//! parsed once from GPUI's keystrokes.

/// A point on the screen as a fraction of its size: (0, 0) is the top-left
/// corner, (1, 1) the bottom-right.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    x: f64,
    y: f64,
}

impl Position {
    /// `None` unless both are finite and within 0..=1.
    #[must_use]
    pub fn new(x: f64, y: f64) -> Option<Self> {
        let inside = |v: f64| v.is_finite() && (0.0..=1.0).contains(&v);
        (inside(x) && inside(y)).then_some(Self { x, y })
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Command,
    Shift,
    Option,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    /// `clicks`: 1 for a single click, 2 for a double, and so on.
    Press {
        button: Button,
        clicks: u32,
    },
    Release {
        button: Button,
        clicks: u32,
    },
    /// With a button held, a drag.
    Move {
        held: Option<Button>,
    },
}

/// Keys without a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    Return,
    Tab,
    Space,
    /// Backspace.
    Delete,
    /// The key that deletes forward (fn+delete on a Mac keyboard).
    ForwardDelete,
    Escape,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// F1 to F12.
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Named(NamedKey),
    /// The character on the key, unshifted: shift travels as a modifier.
    Character(char),
}

/// What the person did on a screen.
#[derive(Debug, Clone, PartialEq)]
pub enum GuestInput {
    Pointer {
        phase: PointerPhase,
        at: Position,
        modifiers: Vec<Modifier>,
    },
    /// `dx` and `dy` are fractions of the screen's width and height;
    /// positive `dy` scrolls towards the top of the content.
    Scroll { at: Position, dx: f64, dy: f64 },
    /// A key pressed with modifiers held; a held key repeats.
    Key { key: Key, modifiers: Vec<Modifier> },
}

impl Modifier {
    pub(crate) fn from_gpui(modifiers: &gpui::Modifiers) -> Vec<Self> {
        [
            (modifiers.platform, Self::Command),
            (modifiers.shift, Self::Shift),
            (modifiers.alt, Self::Option),
            (modifiers.control, Self::Control),
        ]
        .into_iter()
        .filter_map(|(held, modifier)| held.then_some(modifier))
        .collect()
    }
}

impl Button {
    /// `None` for buttons the guest isn't sent.
    pub(crate) const fn from_gpui(button: gpui::MouseButton) -> Option<Self> {
        match button {
            gpui::MouseButton::Left => Some(Self::Left),
            gpui::MouseButton::Right => Some(Self::Right),
            gpui::MouseButton::Middle | gpui::MouseButton::Navigate(_) => None,
        }
    }
}

impl TryFrom<&gpui::Keystroke> for Key {
    /// A key the guest isn't sent: a lone modifier, media keys.
    type Error = ();

    fn try_from(keystroke: &gpui::Keystroke) -> Result<Self, Self::Error> {
        let named = match keystroke.key.as_str() {
            "enter" => NamedKey::Return,
            "tab" => NamedKey::Tab,
            "space" => NamedKey::Space,
            "backspace" => NamedKey::Delete,
            "delete" => NamedKey::ForwardDelete,
            "escape" => NamedKey::Escape,
            "left" => NamedKey::Left,
            "right" => NamedKey::Right,
            "up" => NamedKey::Up,
            "down" => NamedKey::Down,
            "home" => NamedKey::Home,
            "end" => NamedKey::End,
            "pageup" => NamedKey::PageUp,
            "pagedown" => NamedKey::PageDown,
            other => {
                if let Some(n) = other.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
                    && (1..=12).contains(&n)
                {
                    return Ok(Self::Named(NamedKey::F(n)));
                }
                let mut chars = other.chars();
                return match (chars.next(), chars.next()) {
                    (Some(c), None) => Ok(Self::Character(c)),
                    (Some(_), Some(_)) | (None, _) => Err(()),
                };
            }
        };
        Ok(Self::Named(named))
    }
}

/// Where a screen's image is drawn: the frame scaled to fit its element,
/// centred, aspect kept (GPUI's `ObjectFit::Contain`). Window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    left: f64,
    top: f64,
    width: f64,
    height: f64,
}

impl Placement {
    /// `None` for an empty element or frame.
    pub(crate) fn contain(element: (f64, f64, f64, f64), frame: (f64, f64)) -> Option<Self> {
        let (x, y, width, height) = element;
        let (frame_width, frame_height) = frame;
        if [width, height, frame_width, frame_height]
            .iter()
            .any(|v| !(v.is_finite() && *v > 0.0))
        {
            return None;
        }
        let scale = (width / frame_width).min(height / frame_height);
        let (drawn_width, drawn_height) = (frame_width * scale, frame_height * scale);
        Some(Self {
            left: x + (width - drawn_width) / 2.0,
            top: y + (height - drawn_height) / 2.0,
            width: drawn_width,
            height: drawn_height,
        })
    }

    /// Left, top, width and height.
    pub(crate) const fn rect(&self) -> (f64, f64, f64, f64) {
        (self.left, self.top, self.width, self.height)
    }

    fn fraction(&self, x: f64, y: f64) -> (f64, f64) {
        ((x - self.left) / self.width, (y - self.top) / self.height)
    }

    /// The screen position under a window point; `None` off the image.
    pub(crate) fn locate(&self, x: f64, y: f64) -> Option<Position> {
        let (fx, fy) = self.fraction(x, y);
        Position::new(fx, fy)
    }

    /// The nearest screen position: for a drag or release that left the image.
    pub(crate) fn clamp(&self, x: f64, y: f64) -> Position {
        let (fx, fy) = self.fraction(x, y);
        let clamp = |v: f64| {
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        Position {
            x: clamp(fx),
            y: clamp(fy),
        }
    }

    /// A distance in window points as fractions of the screen.
    pub(crate) fn scale(&self, dx: f64, dy: f64) -> (f64, f64) {
        (dx / self.width, dy / self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keystroke(key: &str) -> gpui::Keystroke {
        gpui::Keystroke {
            modifiers: gpui::Modifiers::default(),
            key: key.into(),
            key_char: None,
        }
    }

    #[test]
    fn a_wide_element_pillarboxes_and_points_map_into_the_frame() {
        // A 16:10 frame in a 400x100 element: drawn 160x100, centred.
        let placement = Placement::contain((10.0, 20.0, 400.0, 100.0), (2560.0, 1600.0)).unwrap();
        assert_eq!(
            placement,
            Placement {
                left: 130.0,
                top: 20.0,
                width: 160.0,
                height: 100.0
            }
        );
        assert_eq!(placement.locate(130.0, 20.0), Position::new(0.0, 0.0));
        assert_eq!(placement.locate(210.0, 70.0), Position::new(0.5, 0.5));
        assert_eq!(placement.locate(129.0, 70.0), None, "on the bar");
        assert_eq!(
            placement.clamp(500.0, -5.0),
            Position::new(1.0, 0.0).unwrap()
        );
        assert_eq!(placement.scale(16.0, 10.0), (0.1, 0.1));
    }

    #[test]
    fn a_tall_element_letterboxes() {
        let placement = Placement::contain((0.0, 0.0, 160.0, 300.0), (16.0, 10.0)).unwrap();
        assert_eq!((placement.top, placement.height), (100.0, 100.0));
        assert_eq!(
            Placement::contain((0.0, 0.0, 0.0, 300.0), (16.0, 10.0)),
            None
        );
    }

    #[test]
    fn positions_stay_on_the_screen() {
        assert!(Position::new(1.0, 0.0).is_some());
        assert!(Position::new(1.01, 0.0).is_none());
        assert!(Position::new(f64::NAN, 0.0).is_none());
    }

    #[test]
    fn keystrokes_parse_to_keys_the_guest_is_sent() {
        assert_eq!(Key::try_from(&keystroke("a")), Ok(Key::Character('a')));
        assert_eq!(Key::try_from(&keystroke("/")), Ok(Key::Character('/')));
        assert_eq!(
            Key::try_from(&keystroke("enter")),
            Ok(Key::Named(NamedKey::Return))
        );
        assert_eq!(
            Key::try_from(&keystroke("backspace")),
            Ok(Key::Named(NamedKey::Delete))
        );
        assert_eq!(
            Key::try_from(&keystroke("f12")),
            Ok(Key::Named(NamedKey::F(12)))
        );
        assert_eq!(Key::try_from(&keystroke("f13")), Err(()));
        assert_eq!(
            Key::try_from(&keystroke("delete")),
            Ok(Key::Named(NamedKey::ForwardDelete))
        );
        assert_eq!(Key::try_from(&keystroke("shift")), Err(()));
    }

    #[test]
    fn held_modifiers_are_listed_in_one_order() {
        let modifiers = gpui::Modifiers {
            shift: true,
            platform: true,
            ..gpui::Modifiers::default()
        };
        assert_eq!(
            Modifier::from_gpui(&modifiers),
            [Modifier::Command, Modifier::Shift]
        );
    }
}
