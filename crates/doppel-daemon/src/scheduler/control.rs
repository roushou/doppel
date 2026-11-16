//! Who controls a machine: the agents, through their sessions, or a person
//! at a viewer. While a person holds it, agents' calls on it wait; handing
//! it back lets them go on, and they're owed a note that someone had it.
//!
//! A pure state machine: `(state, event) -> (state, actions)`, with
//! illegal transitions as typed errors. `H` names who holds it (in the
//! daemon, a connection).

use std::time::{Duration, Instant};

/// A machine's control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control<H> {
    Agents,
    Person { holder: H, since: Instant },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlEvent<H> {
    Take {
        by: H,
        at: Instant,
    },
    HandBack {
        by: H,
        at: Instant,
    },
    /// `holder` went away (its connection closed): whatever it held goes
    /// back. Not an error when it held nothing.
    Left {
        holder: H,
        at: Instant,
    },
}

/// What the shell does after a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAction {
    /// Agents' calls on the machine wait from now on.
    Hold,
    /// They go on; the sessions that used the machine are owed a note that
    /// a person had it for `held`.
    Release { held: Duration },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ControlError<H> {
    /// Someone (perhaps the one asking) already has it.
    #[error("a person already has control")]
    Held { by: H },
    #[error("no one has control to hand back")]
    NotHeld,
    #[error("someone else has control")]
    NotTheHolder { holder: H },
}

impl<H: Copy + PartialEq> Control<H> {
    /// # Errors
    /// Taking what someone holds, or handing back what you don't.
    pub fn on(self, event: ControlEvent<H>) -> Result<(Self, Vec<ControlAction>), ControlError<H>> {
        match (self, event) {
            (Self::Agents, ControlEvent::Take { by, at }) => Ok((
                Self::Person {
                    holder: by,
                    since: at,
                },
                vec![ControlAction::Hold],
            )),
            (Self::Person { holder, .. }, ControlEvent::Take { .. }) => {
                Err(ControlError::Held { by: holder })
            }
            (Self::Agents, ControlEvent::HandBack { .. }) => Err(ControlError::NotHeld),
            (Self::Person { holder, since }, ControlEvent::HandBack { by, at }) => {
                if holder == by {
                    Ok(Self::released(since, at))
                } else {
                    Err(ControlError::NotTheHolder { holder })
                }
            }
            (Self::Agents, ControlEvent::Left { .. }) => Ok((self, Vec::new())),
            (Self::Person { holder, since }, ControlEvent::Left { holder: left, at }) => {
                if holder == left {
                    Ok(Self::released(since, at))
                } else {
                    Ok((self, Vec::new()))
                }
            }
        }
    }

    fn released(since: Instant, at: Instant) -> (Self, Vec<ControlAction>) {
        (
            Self::Agents,
            vec![ControlAction::Release {
                held: at.saturating_duration_since(since),
            }],
        )
    }

    /// Whether agents' calls on the machine may run.
    #[must_use]
    pub const fn agents_may_act(&self) -> bool {
        match self {
            Self::Agents => true,
            Self::Person { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: u8 = 1;
    const OTHER: u8 = 2;

    #[expect(
        clippy::disallowed_methods,
        reason = "an arbitrary anchor: only differences from it are asserted"
    )]
    fn times() -> (Instant, Instant) {
        let start = Instant::now();
        (start, start + Duration::from_secs(42))
    }

    #[test]
    fn a_person_takes_control_and_hands_it_back() {
        let (t0, t1) = times();
        let (held, actions) = Control::Agents
            .on(ControlEvent::Take { by: ME, at: t0 })
            .unwrap();
        assert_eq!(actions, [ControlAction::Hold]);
        assert!(!held.agents_may_act());
        let (back, actions) = held.on(ControlEvent::HandBack { by: ME, at: t1 }).unwrap();
        assert_eq!(back, Control::Agents);
        assert_eq!(
            actions,
            [ControlAction::Release {
                held: Duration::from_secs(42)
            }]
        );
        assert!(back.agents_may_act());
    }

    #[test]
    fn a_holder_that_leaves_hands_back_and_others_leaving_change_nothing() {
        let (t0, t1) = times();
        let held = Control::Person {
            holder: ME,
            since: t0,
        };
        assert_eq!(
            held.on(ControlEvent::Left {
                holder: OTHER,
                at: t1
            }),
            Ok((held, Vec::new()))
        );
        assert_eq!(
            held.on(ControlEvent::Left { holder: ME, at: t1 }),
            Ok((
                Control::Agents,
                vec![ControlAction::Release {
                    held: Duration::from_secs(42)
                }]
            ))
        );
        assert_eq!(
            Control::Agents.on(ControlEvent::Left { holder: ME, at: t1 }),
            Ok((Control::Agents, Vec::new()))
        );
    }

    #[test]
    fn taking_held_control_and_handing_back_another_s_are_refused() {
        let (t0, t1) = times();
        let held = Control::Person {
            holder: ME,
            since: t0,
        };
        assert_eq!(
            held.on(ControlEvent::Take { by: OTHER, at: t1 }),
            Err(ControlError::Held { by: ME })
        );
        assert_eq!(
            held.on(ControlEvent::Take { by: ME, at: t1 }),
            Err(ControlError::Held { by: ME })
        );
        assert_eq!(
            held.on(ControlEvent::HandBack { by: OTHER, at: t1 }),
            Err(ControlError::NotTheHolder { holder: ME })
        );
        assert_eq!(
            Control::<u8>::Agents.on(ControlEvent::HandBack { by: ME, at: t1 }),
            Err(ControlError::NotHeld)
        );
    }
}
