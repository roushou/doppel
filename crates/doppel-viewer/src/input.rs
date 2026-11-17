//! Input for a screen, in the viewer's own terms: positions are fractions
//! of the screen, so the viewer needn't know the guest's size, and keys are
//! parsed once from GPUI's keystrokes. [`GuestInput::on`] places it on the
//! guest's display, as the host input the machine takes.

use doppel_machine::geometry::{GeometryError, Point, Size};
use doppel_machine::input as host;

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

/// A person's input placed on a guest display `display` points big: the
/// host input the machine takes. Fractions of the screen scale to points
/// inside the display (the far edges are its last point, which the VMM
/// accepts, not one past it).
impl GuestInput {
    /// # Errors
    /// A point that isn't a valid one on the display.
    pub fn on(self, display: Size) -> Result<host::Input, GeometryError> {
        let point = |at: Position| {
            let (width, height) = (display.width(), display.height());
            Point::new(
                (at.x() * width).min(width - 1.0).max(0.0),
                (at.y() * height).min(height - 1.0).max(0.0),
            )
        };
        Ok(match self {
            Self::Pointer {
                phase,
                at,
                modifiers,
            } => host::Input::Pointer(host::Pointer {
                phase: match phase {
                    PointerPhase::Press { button, clicks } => {
                        host::PointerPhase::Press(button.on_host(), clicks_on_host(clicks))
                    }
                    PointerPhase::Release { button, clicks } => {
                        host::PointerPhase::Release(button.on_host(), clicks_on_host(clicks))
                    }
                    PointerPhase::Move { held } => {
                        host::PointerPhase::Move(held.map(Button::on_host))
                    }
                },
                at: point(at)?,
                modifiers: Modifier::on_host(modifiers),
            }),
            Self::Scroll { at, dx, dy } => host::Input::Scroll {
                at: point(at)?,
                dx: dx * display.width(),
                dy: dy * display.height(),
            },
            Self::Key { key, modifiers } => host::Input::Key(host::KeyChord {
                modifiers: Modifier::on_host(modifiers),
                key: key.on_host(),
            }),
        })
    }
}

/// Clicking on past a triple still selects as a triple.
fn clicks_on_host(clicks: u32) -> host::ClickCount {
    host::ClickCount::try_from(clicks.clamp(1, 3)).unwrap_or(host::ClickCount::Single)
}

impl Button {
    const fn on_host(self) -> host::Button {
        match self {
            Self::Left => host::Button::Left,
            Self::Right => host::Button::Right,
        }
    }
}

impl Modifier {
    fn on_host(modifiers: Vec<Self>) -> Vec<host::Modifier> {
        modifiers
            .into_iter()
            .map(|modifier| match modifier {
                Self::Command => host::Modifier::Command,
                Self::Shift => host::Modifier::Shift,
                Self::Option => host::Modifier::Option,
                Self::Control => host::Modifier::Control,
            })
            .collect()
    }
}

impl Key {
    fn on_host(self) -> host::Key {
        match self {
            Self::Character(c) => host::Key::Character(c),
            Self::Named(named) => host::Key::Named(match named {
                NamedKey::Return => host::NamedKey::Return,
                NamedKey::Tab => host::NamedKey::Tab,
                NamedKey::Space => host::NamedKey::Space,
                NamedKey::Delete => host::NamedKey::Delete,
                NamedKey::ForwardDelete => host::NamedKey::ForwardDelete,
                NamedKey::Escape => host::NamedKey::Escape,
                NamedKey::Left => host::NamedKey::Left,
                NamedKey::Right => host::NamedKey::Right,
                NamedKey::Up => host::NamedKey::Up,
                NamedKey::Down => host::NamedKey::Down,
                NamedKey::Home => host::NamedKey::Home,
                NamedKey::End => host::NamedKey::End,
                NamedKey::PageUp => host::NamedKey::PageUp,
                NamedKey::PageDown => host::NamedKey::PageDown,
                NamedKey::F(n) => host::NamedKey::F(n),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display() -> Size {
        Size::new(1280.0, 800.0).unwrap()
    }

    fn at(x: f64, y: f64) -> Position {
        Position::new(x, y).unwrap()
    }

    #[test]
    fn input_becomes_host_input_inside_the_display() {
        let press = GuestInput::Pointer {
            phase: PointerPhase::Press {
                button: Button::Right,
                clicks: 5,
            },
            at: at(1.0, 1.0),
            modifiers: vec![Modifier::Shift],
        }
        .on(display())
        .unwrap();
        assert_eq!(
            press,
            host::Input::Pointer(host::Pointer {
                phase: host::PointerPhase::Press(host::Button::Right, host::ClickCount::Triple),
                // The far edges are the display's last point.
                at: Point::new(1279.0, 799.0).unwrap(),
                modifiers: vec![host::Modifier::Shift],
            })
        );
        let scroll = GuestInput::Scroll {
            at: at(0.5, 0.5),
            dx: 0.0,
            dy: -0.1,
        }
        .on(display())
        .unwrap();
        assert_eq!(
            scroll,
            host::Input::Scroll {
                at: Point::new(640.0, 400.0).unwrap(),
                dx: 0.0,
                dy: -80.0,
            }
        );
        let key = GuestInput::Key {
            key: Key::Named(NamedKey::F(5)),
            modifiers: Vec::new(),
        }
        .on(display())
        .unwrap();
        assert_eq!(
            key,
            host::Input::Key(host::KeyChord {
                modifiers: Vec::new(),
                key: host::Key::Named(host::NamedKey::F(5)),
            })
        );
    }

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
