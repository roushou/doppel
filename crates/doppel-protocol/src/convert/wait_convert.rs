use std::time::Duration;

use doppel_desktop::ax::tree::AxSnapshot;
use doppel_desktop::screen_text::ScreenText;
use doppel_desktop::wait::{Condition, TextCondition, WaitOutcome, WaitSpec};

use super::{Micros, ParseError};
use crate::pb::daemon::v1 as daemon;

impl From<&Condition> for daemon::wait_for_element::Condition {
    fn from(condition: &Condition) -> Self {
        match condition {
            Condition::Appears(text) => Self::Appears(text.clone()),
            Condition::Disappears(text) => Self::Disappears(text.clone()),
            Condition::Frontmost(app) => Self::Frontmost(app.clone()),
        }
    }
}

impl From<&TextCondition> for daemon::wait_for_text::Condition {
    fn from(condition: &TextCondition) -> Self {
        match condition {
            TextCondition::Appears(query) => Self::Appears(query.as_str().to_owned()),
            TextCondition::Disappears(query) => Self::Disappears(query.as_str().to_owned()),
        }
    }
}

/// How a wait ended, apart from what it saw.
enum End {
    Satisfied(daemon::WaitSatisfied),
    TimedOut(daemon::WaitTimedOut),
    Failed(daemon::WaitFailed),
}

impl End {
    fn of<S>(outcome: &WaitOutcome<S>) -> (Self, Option<&S>) {
        match outcome {
            WaitOutcome::Satisfied {
                seen,
                elapsed,
                polls,
            } => (
                Self::Satisfied(daemon::WaitSatisfied {
                    elapsed_micros: Micros::saturating(*elapsed).get(),
                    polls: *polls,
                }),
                Some(seen),
            ),
            WaitOutcome::TimedOut {
                last,
                last_error,
                elapsed,
                polls,
            } => (
                Self::TimedOut(daemon::WaitTimedOut {
                    last_error: last_error.clone(),
                    elapsed_micros: Micros::saturating(*elapsed).get(),
                    polls: *polls,
                }),
                last.as_ref(),
            ),
            WaitOutcome::Failed { reason, polls } => (
                Self::Failed(daemon::WaitFailed {
                    reason: reason.clone(),
                    polls: *polls,
                }),
                None,
            ),
        }
    }

    /// Pairs the end with what was seen: required when satisfied,
    /// absent when failed.
    fn outcome<S>(self, seen: Option<S>) -> Result<WaitOutcome<S>, ParseError> {
        Ok(match self {
            Self::Satisfied(satisfied) => WaitOutcome::Satisfied {
                seen: seen.ok_or(ParseError::Missing("seen"))?,
                elapsed: Duration::from_micros(satisfied.elapsed_micros),
                polls: satisfied.polls,
            },
            Self::TimedOut(timed_out) => WaitOutcome::TimedOut {
                last: seen,
                last_error: timed_out.last_error,
                elapsed: Duration::from_micros(timed_out.elapsed_micros),
                polls: timed_out.polls,
            },
            Self::Failed(failed) => match seen {
                None => WaitOutcome::Failed {
                    reason: failed.reason,
                    polls: failed.polls,
                },
                Some(_) => {
                    return Err(ParseError::Invalid {
                        field: "seen",
                        reason: "a failed wait saw nothing".into(),
                    });
                }
            },
        })
    }
}

impl daemon::ElementWaitResult {
    /// The outcome, its elapsed time in whole microseconds: below one is
    /// dropped, and past `u64::MAX` saturates.
    #[must_use]
    pub fn in_micros(outcome: &WaitOutcome<AxSnapshot>) -> Self {
        use daemon::element_wait_result::End as Wire;
        let (end, seen) = End::of(outcome);
        Self {
            end: Some(match end {
                End::Satisfied(end) => Wire::Satisfied(end),
                End::TimedOut(end) => Wire::TimedOut(end),
                End::Failed(end) => Wire::Failed(end),
            }),
            seen: seen.map(Into::into),
        }
    }
}

impl TryFrom<daemon::ElementWaitResult> for WaitOutcome<AxSnapshot> {
    type Error = ParseError;
    fn try_from(wire: daemon::ElementWaitResult) -> Result<Self, ParseError> {
        use daemon::element_wait_result::End as Wire;
        let end = match wire.end.ok_or(ParseError::Missing("end"))? {
            Wire::Satisfied(end) => End::Satisfied(end),
            Wire::TimedOut(end) => End::TimedOut(end),
            Wire::Failed(end) => End::Failed(end),
        };
        end.outcome(wire.seen.map(AxSnapshot::try_from).transpose()?)
    }
}

impl daemon::TextWaitResult {
    /// The outcome, its elapsed time in whole microseconds: below one is
    /// dropped, and past `u64::MAX` saturates.
    #[must_use]
    pub fn in_micros(outcome: &WaitOutcome<ScreenText>) -> Self {
        use daemon::text_wait_result::End as Wire;
        let (end, seen) = End::of(outcome);
        Self {
            end: Some(match end {
                End::Satisfied(end) => Wire::Satisfied(end),
                End::TimedOut(end) => Wire::TimedOut(end),
                End::Failed(end) => Wire::Failed(end),
            }),
            seen: seen.map(Into::into),
        }
    }
}

impl TryFrom<daemon::TextWaitResult> for WaitOutcome<ScreenText> {
    type Error = ParseError;
    fn try_from(wire: daemon::TextWaitResult) -> Result<Self, ParseError> {
        use daemon::text_wait_result::End as Wire;
        let end = match wire.end.ok_or(ParseError::Missing("end"))? {
            Wire::Satisfied(end) => End::Satisfied(end),
            Wire::TimedOut(end) => End::TimedOut(end),
            Wire::Failed(end) => End::Failed(end),
        };
        end.outcome(wire.seen.map(ScreenText::try_from).transpose()?)
    }
}

impl ParseError {
    fn timeout(timeout_ms: u32) -> Duration {
        Duration::from_millis(u64::from(timeout_ms))
    }

    fn wait<C>(
        spec: Result<WaitSpec<C>, doppel_desktop::wait::WaitError>,
    ) -> Result<WaitSpec<C>, Self> {
        spec.map_err(|error| Self::Invalid {
            field: "wait",
            reason: error.to_string(),
        })
    }

    /// # Errors
    /// No condition, or one the core refuses (empty text, a timeout out
    /// of range).
    pub fn element_wait(
        condition: Option<daemon::wait_for_element::Condition>,
        timeout_ms: u32,
    ) -> Result<WaitSpec<Condition>, Self> {
        use daemon::wait_for_element::Condition as Wire;
        let condition = match condition.ok_or(Self::Missing("condition"))? {
            Wire::Appears(text) => Condition::Appears(text),
            Wire::Disappears(text) => Condition::Disappears(text),
            Wire::Frontmost(app) => Condition::Frontmost(app),
        };
        Self::wait(WaitSpec::new(condition, Self::timeout(timeout_ms)))
    }

    /// # Errors
    /// No condition, empty text, or a timeout out of range.
    pub fn text_wait(
        condition: Option<daemon::wait_for_text::Condition>,
        timeout_ms: u32,
    ) -> Result<WaitSpec<TextCondition>, Self> {
        use daemon::wait_for_text::Condition as Wire;
        let condition = match condition.ok_or(Self::Missing("condition"))? {
            Wire::Appears(text) => TextCondition::Appears(text.parse()?),
            Wire::Disappears(text) => TextCondition::Disappears(text.parse()?),
        };
        Self::wait(WaitSpec::new(condition, Self::timeout(timeout_ms)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::agent::v1 as agent;

    fn snapshot() -> AxSnapshot {
        AxSnapshot::try_from(agent::AxSnapshot {
            snapshot_id: 3,
            app: Some(agent::RunningApp {
                pid: 1,
                bundle_id: "com.apple.finder".into(),
                name: "Finder".into(),
            }),
            nodes: vec![agent::AxNode {
                role: "AXWindow".into(),
                ..Default::default()
            }],
            truncation: agent::Truncation::Complete as i32,
        })
        .unwrap()
    }

    #[test]
    fn wait_outcomes_round_trip() {
        let outcomes = [
            WaitOutcome::Satisfied {
                seen: snapshot(),
                elapsed: Duration::from_millis(250),
                polls: 2,
            },
            WaitOutcome::TimedOut {
                last: Some(snapshot()),
                last_error: None,
                elapsed: Duration::from_secs(5),
                polls: 9,
            },
            WaitOutcome::TimedOut {
                last: None,
                last_error: Some("no window yet".into()),
                elapsed: Duration::from_secs(5),
                polls: 9,
            },
            WaitOutcome::Failed {
                reason: "agent unreachable".into(),
                polls: 1,
            },
        ];
        for outcome in outcomes {
            let wire = daemon::ElementWaitResult::in_micros(&outcome);
            assert_eq!(WaitOutcome::try_from(wire), Ok(outcome));
        }
        let unseen = daemon::ElementWaitResult {
            end: Some(daemon::element_wait_result::End::Satisfied(
                daemon::WaitSatisfied::default(),
            )),
            seen: None,
        };
        assert_eq!(
            WaitOutcome::<AxSnapshot>::try_from(unseen),
            Err(ParseError::Missing("seen"))
        );
    }

    #[test]
    fn wait_specs_round_trip_through_their_requests() {
        let spec = WaitSpec::new(
            Condition::Frontmost("Finder".into()),
            Duration::from_millis(1500),
        )
        .unwrap();
        let wire = daemon::wait_for_element::Condition::from(spec.condition());
        assert_eq!(ParseError::element_wait(Some(wire), 1500), Ok(spec));
        let text = WaitSpec::new(
            TextCondition::Disappears("Saving…".parse().unwrap()),
            Duration::from_secs(2),
        )
        .unwrap();
        let wire = daemon::wait_for_text::Condition::from(text.condition());
        assert_eq!(ParseError::text_wait(Some(wire), 2000), Ok(text));
        assert!(ParseError::element_wait(None, 1000).is_err());
        assert!(
            ParseError::element_wait(
                Some(daemon::wait_for_element::Condition::Appears("x".into())),
                0
            )
            .is_err(),
            "a zero timeout is refused"
        );
    }
}
