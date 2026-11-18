//! Input actions, in guest display points with a top-left origin.
//! The same type drives host-side input (VMM) and, later, the guest agent.

use std::str::FromStr;

use crate::geometry::Point;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum InputError {
    #[error("click count must be 1, 2 or 3 (got {0})")]
    ClickCount(u32),
    #[error(
        "unknown key {0:?}; use a single character or one of: return tab space delete forwarddelete escape left right up down home end pageup pagedown f1–f12"
    )]
    UnknownKey(String),
    #[error("unknown modifier {0:?}; use cmd, shift, alt or ctrl")]
    UnknownModifier(String),
}

/// Which path input and capture take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The guest agent: the logged-in session's own events and capture.
    Agent,
    /// The VMM: works before login and on a locked screen.
    Host,
}

impl Via {
    /// As tools and the command line spell it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Host => "host",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickCount {
    Single,
    Double,
    Triple,
}

impl ClickCount {
    #[must_use]
    pub const fn get(self) -> u32 {
        match self {
            Self::Single => 1,
            Self::Double => 2,
            Self::Triple => 3,
        }
    }
}

impl TryFrom<u32> for ClickCount {
    type Error = InputError;
    fn try_from(n: u32) -> Result<Self, Self::Error> {
        match n {
            1 => Ok(Self::Single),
            2 => Ok(Self::Double),
            3 => Ok(Self::Triple),
            other => Err(InputError::ClickCount(other)),
        }
    }
}

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
    F(FunctionKey),
}

/// A function key: F1 to F12, the ones a Mac keyboard has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionKey {
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

impl FunctionKey {
    /// Every one, F1 first.
    pub const ALL: [Self; 12] = [
        Self::F1,
        Self::F2,
        Self::F3,
        Self::F4,
        Self::F5,
        Self::F6,
        Self::F7,
        Self::F8,
        Self::F9,
        Self::F10,
        Self::F11,
        Self::F12,
    ];

    /// Fn: `None` outside 1..=12.
    #[must_use]
    pub fn numbered(n: u8) -> Option<Self> {
        Self::ALL.get(usize::from(n).checked_sub(1)?).copied()
    }

    /// Its number, 1..=12.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::F1 => 1,
            Self::F2 => 2,
            Self::F3 => 3,
            Self::F4 => 4,
            Self::F5 => 5,
            Self::F6 => 6,
            Self::F7 => 7,
            Self::F8 => 8,
            Self::F9 => 9,
            Self::F10 => 10,
            Self::F11 => 11,
            Self::F12 => 12,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Named(NamedKey),
    Character(char),
}

impl FromStr for Key {
    type Err = InputError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let named = match s.to_ascii_lowercase().as_str() {
            "return" | "enter" => NamedKey::Return,
            "tab" => NamedKey::Tab,
            "space" => NamedKey::Space,
            "delete" | "backspace" => NamedKey::Delete,
            "forwarddelete" => NamedKey::ForwardDelete,
            "escape" | "esc" => NamedKey::Escape,
            "left" => NamedKey::Left,
            "right" => NamedKey::Right,
            "up" => NamedKey::Up,
            "down" => NamedKey::Down,
            "home" => NamedKey::Home,
            "end" => NamedKey::End,
            "pageup" => NamedKey::PageUp,
            "pagedown" => NamedKey::PageDown,
            other => match (
                other
                    .strip_prefix('f')
                    .and_then(|n| n.parse::<u8>().ok())
                    .and_then(FunctionKey::numbered),
                s.chars().collect::<Vec<_>>().as_slice(),
            ) {
                (Some(key), _) => NamedKey::F(key),
                (_, [c]) => return Ok(Self::Character(*c)),
                (None, _) => return Err(InputError::UnknownKey(s.to_owned())),
            },
        };
        Ok(Self::Named(named))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Command,
    Shift,
    Option,
    Control,
}

impl FromStr for Modifier {
    type Err = InputError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "cmd" | "command" => Ok(Self::Command),
            "shift" => Ok(Self::Shift),
            "alt" | "option" | "opt" => Ok(Self::Option),
            "ctrl" | "control" => Ok(Self::Control),
            _ => Err(InputError::UnknownModifier(s.to_owned())),
        }
    }
}

/// `cmd+shift+4`, `return`, `a`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChord {
    pub modifiers: Vec<Modifier>,
    pub key: Key,
}

impl FromStr for KeyChord {
    type Err = InputError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // "+" alone (or ending in "++") is the plus key itself.
        let (mods, key) = match s.strip_suffix("++") {
            Some(rest) => (rest, "+"),
            None => s
                .rsplit_once('+')
                .filter(|(m, _)| !m.is_empty())
                .unwrap_or(("", s)),
        };
        let modifiers = if mods.is_empty() {
            Vec::new()
        } else {
            mods.split('+').map(str::parse).collect::<Result<_, _>>()?
        };
        Ok(Self {
            modifiers,
            key: key.parse()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Click {
        at: Point,
        button: Button,
        count: ClickCount,
    },
    MoveTo(Point),
    Drag {
        from: Point,
        to: Point,
    },
    Scroll {
        at: Point,
        dx: f64,
        dy: f64,
    },
    Key(KeyChord),
    Type(String),
    Pointer(Pointer),
}

/// One pointer event as it happens, as an interactive console forwards it.
/// [`Input::Click`] and [`Input::Drag`] are gestures made of these.
#[derive(Debug, Clone, PartialEq)]
pub struct Pointer {
    pub phase: PointerPhase,
    pub at: Point,
    pub modifiers: Vec<Modifier>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    Press(Button, ClickCount),
    Release(Button, ClickCount),
    /// A move; with a button held, a drag.
    Move(Option<Button>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_function_key_is_one_of_f1_to_f12() {
        assert_eq!(FunctionKey::numbered(0), None);
        assert_eq!(FunctionKey::numbered(13), None);
        for n in 1..=12 {
            assert_eq!(FunctionKey::numbered(n).map(FunctionKey::number), Some(n));
        }
        assert!("f13".parse::<KeyChord>().is_err());
    }

    #[test]
    fn chords() {
        assert_eq!(
            "cmd+shift+4".parse::<KeyChord>().unwrap(),
            KeyChord {
                modifiers: vec![Modifier::Command, Modifier::Shift],
                key: Key::Character('4')
            }
        );
        assert_eq!(
            "return".parse::<KeyChord>().unwrap().key,
            Key::Named(NamedKey::Return)
        );
        assert_eq!(
            "ForwardDelete".parse::<KeyChord>().unwrap().key,
            Key::Named(NamedKey::ForwardDelete)
        );
        assert_eq!(
            "F12".parse::<KeyChord>().unwrap().key,
            Key::Named(NamedKey::F(FunctionKey::F12))
        );
        assert_eq!(
            "cmd++".parse::<KeyChord>().unwrap(),
            KeyChord {
                modifiers: vec![Modifier::Command],
                key: Key::Character('+')
            }
        );
        assert_eq!("+".parse::<KeyChord>().unwrap().key, Key::Character('+'));
        assert!("hyper+a".parse::<KeyChord>().is_err());
        assert!("f13".parse::<KeyChord>().is_err());
        assert!("ab".parse::<KeyChord>().is_err());
    }

    #[test]
    fn points_and_counts() {
        assert_eq!(ClickCount::try_from(4), Err(InputError::ClickCount(4)));
    }
}
