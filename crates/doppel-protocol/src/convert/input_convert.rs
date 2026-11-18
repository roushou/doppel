use doppel_desktop::geometry::Point;
use doppel_desktop::input::{
    Button, ClickCount, FunctionKey, Input, Key, KeyChord, Modifier, NamedKey, Pointer,
    PointerPhase,
};

use crate::pb::common::v1 as common;

impl From<Point> for common::Point {
    fn from(p: Point) -> Self {
        Self { x: p.x(), y: p.y() }
    }
}

impl From<NamedKey> for common::NamedKey {
    fn from(key: NamedKey) -> Self {
        match key {
            NamedKey::Return => common::NamedKey::Return,
            NamedKey::Tab => common::NamedKey::Tab,
            NamedKey::Space => common::NamedKey::Space,
            NamedKey::Delete => common::NamedKey::Delete,
            NamedKey::ForwardDelete => common::NamedKey::ForwardDelete,
            NamedKey::Escape => common::NamedKey::Escape,
            NamedKey::Left => common::NamedKey::Left,
            NamedKey::Right => common::NamedKey::Right,
            NamedKey::Up => common::NamedKey::Up,
            NamedKey::Down => common::NamedKey::Down,
            NamedKey::Home => common::NamedKey::Home,
            NamedKey::End => common::NamedKey::End,
            NamedKey::PageUp => common::NamedKey::PageUp,
            NamedKey::PageDown => common::NamedKey::PageDown,
            NamedKey::F(key) => match key {
                FunctionKey::F1 => common::NamedKey::F1,
                FunctionKey::F2 => common::NamedKey::F2,
                FunctionKey::F3 => common::NamedKey::F3,
                FunctionKey::F4 => common::NamedKey::F4,
                FunctionKey::F5 => common::NamedKey::F5,
                FunctionKey::F6 => common::NamedKey::F6,
                FunctionKey::F7 => common::NamedKey::F7,
                FunctionKey::F8 => common::NamedKey::F8,
                FunctionKey::F9 => common::NamedKey::F9,
                FunctionKey::F10 => common::NamedKey::F10,
                FunctionKey::F11 => common::NamedKey::F11,
                FunctionKey::F12 => common::NamedKey::F12,
            },
        }
    }
}

impl From<Modifier> for common::Modifier {
    fn from(modifier: Modifier) -> Self {
        match modifier {
            Modifier::Command => Self::Command,
            Modifier::Shift => Self::Shift,
            Modifier::Option => Self::Option,
            Modifier::Control => Self::Control,
        }
    }
}

impl From<Button> for common::MouseButton {
    fn from(button: Button) -> Self {
        match button {
            Button::Left => Self::Left,
            Button::Right => Self::Right,
        }
    }
}

impl From<&Pointer> for common::Pointer {
    fn from(pointer: &Pointer) -> Self {
        let (phase, button, click_count) = match pointer.phase {
            PointerPhase::Press(button, count) => {
                (common::PointerPhase::Press, Some(button), count.get())
            }
            PointerPhase::Release(button, count) => {
                (common::PointerPhase::Release, Some(button), count.get())
            }
            PointerPhase::Move(held) => (common::PointerPhase::Move, held, 0),
        };
        Self {
            phase: phase as i32,
            at: Some(pointer.at.into()),
            button: button.map_or(common::MouseButton::Unspecified, common::MouseButton::from)
                as i32,
            click_count,
            modifiers: pointer
                .modifiers
                .iter()
                .map(|m| common::Modifier::from(*m) as i32)
                .collect(),
        }
    }
}

impl From<&KeyChord> for common::KeyChord {
    fn from(chord: &KeyChord) -> Self {
        let modifiers = chord
            .modifiers
            .iter()
            .map(|m| common::Modifier::from(*m) as i32)
            .collect();
        let key = match chord.key {
            Key::Named(key) => common::key::Key::Named(common::NamedKey::from(key) as i32),
            Key::Character(c) => common::key::Key::Character(c.to_string()),
        };
        Self {
            modifiers,
            key: Some(common::Key { key: Some(key) }),
        }
    }
}

impl From<&Input> for common::Input {
    fn from(input: &Input) -> Self {
        use common::input::Action;
        let action = match input {
            Input::Click { at, button, count } => Action::Click(common::Click {
                at: Some((*at).into()),
                button: common::MouseButton::from(*button) as i32,
                count: count.get(),
            }),
            Input::MoveTo(at) => Action::MoveTo((*at).into()),
            Input::Drag { from, to } => Action::Drag(common::Drag {
                from: Some((*from).into()),
                to: Some((*to).into()),
                button: common::MouseButton::Left as i32,
            }),
            Input::Scroll { at, dx, dy } => Action::Scroll(common::Scroll {
                at: Some((*at).into()),
                dx: *dx,
                dy: *dy,
            }),
            Input::Key(chord) => Action::Key(chord.into()),
            Input::Type(text) => Action::TypeText(common::TypeText { text: text.clone() }),
            Input::Pointer(pointer) => Action::Pointer(pointer.into()),
        };
        Self {
            action: Some(action),
        }
    }
}

use super::ParseError;

impl TryFrom<common::Point> for Point {
    type Error = ParseError;
    fn try_from(p: common::Point) -> Result<Self, ParseError> {
        Self::new(p.x, p.y).map_err(|error| ParseError::input("point", &error))
    }
}

impl ParseError {
    fn input(field: &'static str, error: &dyn std::fmt::Display) -> Self {
        Self::Invalid {
            field,
            reason: error.to_string(),
        }
    }

    fn count(field: &'static str, raw: u32) -> Result<ClickCount, Self> {
        raw.try_into().map_err(|error| Self::input(field, &error))
    }

    fn point(field: &'static str, point: Option<common::Point>) -> Result<Point, Self> {
        point.ok_or(Self::Missing(field))?.try_into()
    }

    fn named_key(raw: i32) -> Result<NamedKey, Self> {
        let unknown = Self::UnknownEnum {
            field: "key.named",
            value: raw,
        };
        Ok(
            match common::NamedKey::try_from(raw).map_err(|_| unknown.clone())? {
                common::NamedKey::Return => NamedKey::Return,
                common::NamedKey::Tab => NamedKey::Tab,
                common::NamedKey::Space => NamedKey::Space,
                common::NamedKey::Delete => NamedKey::Delete,
                common::NamedKey::ForwardDelete => NamedKey::ForwardDelete,
                common::NamedKey::Escape => NamedKey::Escape,
                common::NamedKey::Left => NamedKey::Left,
                common::NamedKey::Right => NamedKey::Right,
                common::NamedKey::Up => NamedKey::Up,
                common::NamedKey::Down => NamedKey::Down,
                common::NamedKey::Home => NamedKey::Home,
                common::NamedKey::End => NamedKey::End,
                common::NamedKey::PageUp => NamedKey::PageUp,
                common::NamedKey::PageDown => NamedKey::PageDown,
                common::NamedKey::F1 => NamedKey::F(FunctionKey::F1),
                common::NamedKey::F2 => NamedKey::F(FunctionKey::F2),
                common::NamedKey::F3 => NamedKey::F(FunctionKey::F3),
                common::NamedKey::F4 => NamedKey::F(FunctionKey::F4),
                common::NamedKey::F5 => NamedKey::F(FunctionKey::F5),
                common::NamedKey::F6 => NamedKey::F(FunctionKey::F6),
                common::NamedKey::F7 => NamedKey::F(FunctionKey::F7),
                common::NamedKey::F8 => NamedKey::F(FunctionKey::F8),
                common::NamedKey::F9 => NamedKey::F(FunctionKey::F9),
                common::NamedKey::F10 => NamedKey::F(FunctionKey::F10),
                common::NamedKey::F11 => NamedKey::F(FunctionKey::F11),
                common::NamedKey::F12 => NamedKey::F(FunctionKey::F12),
                common::NamedKey::Unspecified => return Err(unknown),
            },
        )
    }

    fn modifiers(raw: &[i32]) -> Result<Vec<Modifier>, Self> {
        raw.iter()
            .map(|&raw| match common::Modifier::try_from(raw) {
                Ok(common::Modifier::Command) => Ok(Modifier::Command),
                Ok(common::Modifier::Shift) => Ok(Modifier::Shift),
                Ok(common::Modifier::Option) => Ok(Modifier::Option),
                Ok(common::Modifier::Control) => Ok(Modifier::Control),
                Ok(common::Modifier::Function | common::Modifier::Unspecified) | Err(_) => {
                    Err(Self::UnknownEnum {
                        field: "modifiers",
                        value: raw,
                    })
                }
            })
            .collect()
    }

    fn button(field: &'static str, raw: i32) -> Result<Option<Button>, Self> {
        match common::MouseButton::try_from(raw) {
            Ok(common::MouseButton::Left) => Ok(Some(Button::Left)),
            Ok(common::MouseButton::Right) => Ok(Some(Button::Right)),
            Ok(common::MouseButton::Unspecified) => Ok(None),
            // On the wire; no button this build sends.
            Ok(common::MouseButton::Middle) | Err(_) => {
                Err(Self::UnknownEnum { field, value: raw })
            }
        }
    }

    fn pressed(field: &'static str, raw: i32) -> Result<Button, Self> {
        Self::button(field, raw)?.ok_or(Self::Missing(field))
    }
}

impl TryFrom<common::KeyChord> for KeyChord {
    type Error = ParseError;
    fn try_from(chord: common::KeyChord) -> Result<Self, ParseError> {
        let key = match chord.key.and_then(|key| key.key) {
            None => return Err(ParseError::Missing("key")),
            Some(common::key::Key::Named(raw)) => Key::Named(ParseError::named_key(raw)?),
            Some(common::key::Key::VirtualKeyCode(code)) => {
                return Err(ParseError::Invalid {
                    field: "key.virtual_key_code",
                    reason: format!("{code}: this build only sends named keys and characters"),
                });
            }
            Some(common::key::Key::Character(text)) => {
                let mut chars = text.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Key::Character(c),
                    (None | Some(_), _) => {
                        return Err(ParseError::Invalid {
                            field: "key.character",
                            reason: format!("{text:?} is not one character"),
                        });
                    }
                }
            }
        };
        Ok(Self {
            modifiers: ParseError::modifiers(&chord.modifiers)?,
            key,
        })
    }
}

impl TryFrom<common::Pointer> for Pointer {
    type Error = ParseError;
    fn try_from(pointer: common::Pointer) -> Result<Self, ParseError> {
        let phase = match common::PointerPhase::try_from(pointer.phase) {
            Ok(common::PointerPhase::Press) => PointerPhase::Press(
                ParseError::pressed("pointer.button", pointer.button)?,
                ParseError::count("pointer.click_count", pointer.click_count)?,
            ),
            Ok(common::PointerPhase::Release) => PointerPhase::Release(
                ParseError::pressed("pointer.button", pointer.button)?,
                ParseError::count("pointer.click_count", pointer.click_count)?,
            ),
            Ok(common::PointerPhase::Move) => {
                PointerPhase::Move(ParseError::button("pointer.button", pointer.button)?)
            }
            Ok(common::PointerPhase::Unspecified) | Err(_) => {
                return Err(ParseError::UnknownEnum {
                    field: "pointer.phase",
                    value: pointer.phase,
                });
            }
        };
        Ok(Self {
            phase,
            at: ParseError::point("pointer.at", pointer.at)?,
            modifiers: ParseError::modifiers(&pointer.modifiers)?,
        })
    }
}

impl TryFrom<common::Input> for Input {
    type Error = ParseError;
    fn try_from(input: common::Input) -> Result<Self, ParseError> {
        use common::input::Action;
        Ok(
            match input.action.ok_or(ParseError::Missing("input.action"))? {
                Action::Click(click) => Self::Click {
                    at: ParseError::point("click.at", click.at)?,
                    button: ParseError::pressed("click.button", click.button)?,
                    count: ParseError::count("click.count", click.count)?,
                },
                Action::MoveTo(at) => Self::MoveTo(at.try_into()?),
                // Drags are with the left button.
                Action::Drag(drag) => Self::Drag {
                    from: ParseError::point("drag.from", drag.from)?,
                    to: ParseError::point("drag.to", drag.to)?,
                },
                Action::Scroll(scroll) => Self::Scroll {
                    at: ParseError::point("scroll.at", scroll.at)?,
                    dx: scroll.dx,
                    dy: scroll.dy,
                },
                Action::Key(chord) => Self::Key(chord.try_into()?),
                Action::TypeText(text) => Self::Type(text.text),
                Action::Pointer(pointer) => Self::Pointer(pointer.try_into()?),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_input_round_trips() {
        let at = Point::new(10.5, 20.0).unwrap();
        let inputs = [
            Input::Click {
                at,
                button: Button::Right,
                count: ClickCount::Double,
            },
            Input::MoveTo(at),
            Input::Drag {
                from: at,
                to: Point::new(1.0, 2.0).unwrap(),
            },
            Input::Scroll {
                at,
                dx: -3.0,
                dy: 4.5,
            },
            Input::Key("cmd+shift+f12".parse().unwrap()),
            Input::Key("ctrl+é".parse().unwrap()),
            Input::Type("Grüße ✓ 🦊".into()),
            Input::Pointer(Pointer {
                phase: PointerPhase::Press(Button::Left, ClickCount::Triple),
                at,
                modifiers: vec![Modifier::Option],
            }),
            Input::Pointer(Pointer {
                phase: PointerPhase::Move(None),
                at,
                modifiers: Vec::new(),
            }),
            Input::Pointer(Pointer {
                phase: PointerPhase::Release(Button::Right, ClickCount::Single),
                at,
                modifiers: Vec::new(),
            }),
        ];
        for input in inputs {
            let wire = common::Input::from(&input);
            assert_eq!(Input::try_from(wire), Ok(input));
        }
    }

    #[test]
    fn malformed_input_is_refused() {
        let click = |button| common::Input {
            action: Some(common::input::Action::Click(common::Click {
                at: Some(common::Point { x: 1.0, y: 1.0 }),
                button,
                count: 1,
            })),
        };
        assert_eq!(
            Input::try_from(click(common::MouseButton::Unspecified as i32)),
            Err(ParseError::Missing("click.button"))
        );
        assert!(Input::try_from(click(99)).is_err());
        assert_eq!(
            Input::try_from(common::Input::default()),
            Err(ParseError::Missing("input.action"))
        );
    }

    #[test]
    fn a_pointer_carries_a_button_only_where_it_has_one() {
        use doppel_desktop::input::ClickCount;
        let at = Point::new(3.0, 4.0).unwrap();
        let press = common::Pointer::from(&Pointer {
            phase: PointerPhase::Press(Button::Right, ClickCount::Double),
            at,
            modifiers: vec![Modifier::Command],
        });
        assert_eq!(press.phase, common::PointerPhase::Press as i32);
        assert_eq!(press.button, common::MouseButton::Right as i32);
        assert_eq!(press.click_count, 2);
        assert_eq!(press.modifiers, vec![common::Modifier::Command as i32]);
        let hover = common::Pointer::from(&Pointer {
            phase: PointerPhase::Move(None),
            at,
            modifiers: Vec::new(),
        });
        assert_eq!(hover.phase, common::PointerPhase::Move as i32);
        assert_eq!(hover.button, common::MouseButton::Unspecified as i32);
        assert_eq!(hover.click_count, 0);
    }

    #[test]
    fn every_named_key_survives_the_wire() {
        let keys = [
            NamedKey::Return,
            NamedKey::Tab,
            NamedKey::Space,
            NamedKey::Delete,
            NamedKey::ForwardDelete,
            NamedKey::Escape,
            NamedKey::Left,
            NamedKey::Right,
            NamedKey::Up,
            NamedKey::Down,
            NamedKey::Home,
            NamedKey::End,
            NamedKey::PageUp,
            NamedKey::PageDown,
        ]
        .into_iter()
        .chain(FunctionKey::ALL.map(NamedKey::F));
        for key in keys {
            let wire = common::NamedKey::from(key) as i32;
            assert_eq!(ParseError::named_key(wire), Ok(key), "{key:?}");
        }
    }

    #[test]
    fn each_function_key_has_its_own_wire_key() {
        let wire: std::collections::BTreeSet<i32> = FunctionKey::ALL
            .map(|key| common::NamedKey::from(NamedKey::F(key)) as i32)
            .into_iter()
            .collect();
        assert_eq!(wire.len(), 12);
        assert!(!wire.contains(&(common::NamedKey::Unspecified as i32)));
        assert_eq!(
            common::NamedKey::from(NamedKey::F(FunctionKey::F12)),
            common::NamedKey::F12
        );
    }
}
