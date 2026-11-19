//! Which colour each agent on a machine wears.

use super::Rgb;
use crate::timeline::Moment;

/// An agent's colours: `fill` for what it draws, `ink` for words on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionHue {
    pub fill: Rgb,
    pub ink: Rgb,
}

/// The palette, in the order agents take it. The first is an agent's colour
/// when it is alone, whatever its session's number.
const HUES: [SessionHue; 4] = [
    SessionHue {
        fill: Rgb(0x22_e5ff),
        ink: Rgb(0x0b_5d6b),
    },
    SessionHue {
        fill: Rgb(0x3d_8bff),
        ink: Rgb(0x1f_4f9e),
    },
    SessionHue {
        fill: Rgb(0xb1_8cff),
        ink: Rgb(0x5b_3aa8),
    },
    SessionHue {
        fill: Rgb(0xc3_d94e),
        ink: Rgb(0x56_6314),
    },
];

/// The agents seen on one machine's calls, in the order they first acted:
/// the first takes the palette's first colour, the next the second, and so
/// on around it. A session's number only grows over the daemon's life, so
/// colouring by it would give a lone agent whichever colour its number
/// lands on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionColours(Vec<u64>);

impl SessionColours {
    /// From `moments`, in start order; a person's turns take no colour.
    #[must_use]
    pub fn of(moments: &[Moment]) -> Self {
        let mut sessions = Vec::new();
        for moment in moments.iter().filter(|moment| !moment.person) {
            let session = moment.call.key.session;
            if !sessions.contains(&session) {
                sessions.push(session);
            }
        }
        Self(sessions)
    }

    /// `session`'s colours: by when it first acted, or as the next to act
    /// if it hasn't yet.
    #[must_use]
    pub fn hue(&self, session: u64) -> SessionHue {
        let slot = self
            .0
            .iter()
            .position(|known| *known == session)
            .unwrap_or(self.0.len());
        HUES.get(slot % HUES.len()).copied().unwrap_or(HUES[0])
    }

    /// Every colour, in the order agents take them.
    #[must_use]
    pub const fn palette() -> [SessionHue; 4] {
        HUES
    }
}
