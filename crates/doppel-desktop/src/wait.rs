//! `wait_for`: poll until a condition holds or time runs out.
//!
//! Input and the screen are asynchronous, so an agent waits
//! for evidence instead of sleeping. This is the pure part: given each poll's
//! result and the time, it decides whether to poll again (and when) or finish.
//! The shell takes the observations (AX snapshots or recognized screen text)
//! and sleeps. Time comes in as values.
//!
//! An unknown outcome is never rounded up: "disappeared" needs a complete
//! snapshot, because text missing from a partial one may simply not have been
//! reached.

use std::time::{Duration, Instant};

use crate::ax::tree::AxSnapshot;
use crate::deadline::Deadline;
use crate::screen_text::{ScreenText, TextQuery};

/// A condition, judged against one kind of observation. The engine is
/// generic over it rather than over an enum of observations, so a poll of
/// the wrong kind (text for an accessibility condition) can't be fed in.
pub trait Watch {
    type Seen;
    fn verdict(&self, seen: &Self::Seen) -> Verdict;
    /// # Errors
    /// [`WaitError`] when the condition can't be waited for.
    fn check(&self) -> Result<(), WaitError>;
}

/// Accessibility conditions, judged against snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// Some element's title, description, identifier or text value contains this.
    Appears(String),
    /// No element mentions this, in a complete snapshot.
    Disappears(String),
    /// The snapshot's app has this name or bundle id.
    Frontmost(String),
}

impl Watch for Condition {
    type Seen = AxSnapshot;

    fn check(&self) -> Result<(), WaitError> {
        let (Self::Appears(text) | Self::Disappears(text) | Self::Frontmost(text)) = self;
        if text.is_empty() {
            Err(WaitError::EmptyText)
        } else {
            Ok(())
        }
    }

    fn verdict(&self, snapshot: &AxSnapshot) -> Verdict {
        let mentioned = |needle: &str| snapshot.tree.iter().any(|(_, node)| node.mentions(needle));
        match self {
            Self::Appears(text) if mentioned(text) => Verdict::Holds,
            Self::Appears(_) => Verdict::Not,
            Self::Disappears(text) if mentioned(text) => Verdict::Not,
            Self::Disappears(_) if snapshot.truncation.is_complete() => Verdict::Holds,
            Self::Disappears(_) => Verdict::Unknown,
            Self::Frontmost(app) => {
                if snapshot.app.name == *app || snapshot.app.bundle_id.as_deref() == Some(app) {
                    Verdict::Holds
                } else {
                    Verdict::Not
                }
            }
        }
    }
}

/// Text conditions, judged against what recognition reads off the display.
/// A recognition covers the whole display, so a disappearance is never
/// unknown the way it is for a partial snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextCondition {
    Appears(TextQuery),
    Disappears(TextQuery),
}

impl TextCondition {
    #[must_use]
    pub const fn query(&self) -> &TextQuery {
        match self {
            Self::Appears(query) | Self::Disappears(query) => query,
        }
    }
}

impl Watch for TextCondition {
    type Seen = ScreenText;

    fn check(&self) -> Result<(), WaitError> {
        Ok(())
    }

    fn verdict(&self, text: &ScreenText) -> Verdict {
        let found = !text.find(self.query()).is_empty();
        match (self, found) {
            (Self::Appears(_), true) | (Self::Disappears(_), false) => Verdict::Holds,
            (Self::Appears(_), false) | (Self::Disappears(_), true) => Verdict::Not,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Holds,
    Not,
    /// The snapshot can't tell (it is partial).
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WaitError {
    #[error("the condition's text is empty")]
    EmptyText,
    #[error("timeout must be between 1 ms and 60 s (got {0:?})")]
    Timeout(Duration),
}

/// A validated wait: a condition and how long to try.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitSpec<C> {
    condition: C,
    timeout: Duration,
}

/// Limits shared by every kind of wait.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(60);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const FIRST_DELAY: Duration = Duration::from_millis(100);
/// How long after a reported change a wait looks again: enough for a burst
/// (a window opening, a list filling) to land in one observation.
pub const SETTLE: Duration = Duration::from_millis(50);
const MAX_DELAY: Duration = Duration::from_secs(1);
/// The last poll starts at the timeout; this is how long it may take to
/// answer. Covers the agent's own snapshot budget (3 s) with room to spare.
pub const POLL_GRACE: Duration = Duration::from_secs(5);

impl<C: Watch> WaitSpec<C> {
    /// # Errors
    /// [`WaitError`] for a condition that can't be waited for (empty text)
    /// or a timeout outside 1 ms..=60 s.
    pub fn new(condition: C, timeout: Duration) -> Result<Self, WaitError> {
        condition.check()?;
        if timeout.is_zero() || timeout > MAX_TIMEOUT {
            return Err(WaitError::Timeout(timeout));
        }
        Ok(Self { condition, timeout })
    }

    #[must_use]
    pub const fn condition(&self) -> &C {
        &self.condition
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}

/// What one poll produced, as the shell classifies it.
#[derive(Debug, Clone, PartialEq)]
pub enum Poll<S> {
    Seen(S),
    /// Worth trying again (the app had no window yet, a walk hit a moving tree).
    Retryable(String),
    /// Trying again won't help (a missing permission, an unreachable agent).
    Fatal(String),
}

#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub enum WaitOutcome<S> {
    Satisfied {
        seen: S,
        elapsed: Duration,
        polls: u32,
    },
    TimedOut {
        /// The last observation, if any poll produced one.
        last: Option<S>,
        /// The last retryable error, if the last poll failed.
        last_error: Option<String>,
        elapsed: Duration,
        polls: u32,
    },
    Failed {
        reason: String,
        polls: u32,
    },
}

/// A wait in progress. Each [`Self::observe`] consumes it, so a finished wait
/// can't be polled again.
#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub struct Waiting<C> {
    spec: WaitSpec<C>,
    started: Instant,
    deadline: Instant,
    /// When the next poll is due.
    next_poll: Instant,
    polls: u32,
    delay: Duration,
}

#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub enum Next<C: Watch> {
    /// Poll again at this instant.
    PollAt(Waiting<C>, Instant),
    Done(WaitOutcome<C::Seen>),
}

impl<C: Watch> Waiting<C> {
    /// Starts a wait at `now`. The shell polls immediately.
    pub fn start(spec: WaitSpec<C>, now: Instant) -> Self {
        Self {
            deadline: now + spec.timeout,
            spec,
            started: now,
            next_poll: now,
            polls: 0,
            delay: FIRST_DELAY,
        }
    }

    /// When any poll of this wait must have answered: the timeout, plus
    /// [`POLL_GRACE`] for the poll that starts at it. A stuck poll can't
    /// hold the wait up past this.
    pub fn poll_deadline(&self) -> Deadline {
        Deadline::after(self.started, self.spec.timeout + POLL_GRACE)
    }

    /// Folds in one poll's result, taken at `now`.
    pub fn observe(mut self, poll: Poll<C::Seen>, now: Instant) -> Next<C> {
        self.polls += 1;
        let elapsed = now.saturating_duration_since(self.started);
        let (last, last_error) = match poll {
            Poll::Fatal(reason) => {
                return Next::Done(WaitOutcome::Failed {
                    reason,
                    polls: self.polls,
                });
            }
            Poll::Seen(seen) => match self.spec.condition.verdict(&seen) {
                Verdict::Holds => {
                    return Next::Done(WaitOutcome::Satisfied {
                        seen,
                        elapsed,
                        polls: self.polls,
                    });
                }
                Verdict::Not | Verdict::Unknown => (Some(seen), None),
            },
            Poll::Retryable(error) => (None, Some(error)),
        };
        if now >= self.deadline {
            return Next::Done(WaitOutcome::TimedOut {
                last,
                last_error,
                elapsed,
                polls: self.polls,
            });
        }
        let at = (now + self.delay).min(self.deadline);
        self.delay = self.delay.mul_f64(1.5).min(MAX_DELAY);
        self.next_poll = at;
        Next::PollAt(self, at)
    }

    /// The guest reported a change at `now`: poll [`SETTLE`] later, if that
    /// is sooner than planned, and start the backoff over, since the UI is
    /// moving. Without reports a wait polls on its backoff alone.
    pub fn on_change(mut self, now: Instant) -> (Self, Instant) {
        let at = (now + SETTLE).min(self.next_poll);
        self.next_poll = at;
        self.delay = FIRST_DELAY;
        (self, at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{BundleId, ProcessId, RunningApp};
    use crate::ax::tree::tests::node;
    use crate::ax::tree::{AxNode, AxTree, AxValue, NodeIndex, SnapshotId, Truncation};

    fn snapshot(texts: &[&str], truncation: Truncation) -> AxSnapshot {
        let mut nodes = vec![(None, node("AXWindow"))];
        nodes.extend(texts.iter().map(|t| {
            (
                Some(NodeIndex::ROOT),
                AxNode {
                    value: Some(AxValue::Text((*t).into())),
                    ..node("AXStaticText")
                },
            )
        }));
        AxSnapshot {
            id: SnapshotId::new(1),
            app: RunningApp {
                pid: ProcessId::new(1),
                bundle_id: BundleId::new("com.apple.TextEdit"),
                name: "TextEdit".into(),
            },
            tree: AxTree::new(nodes).unwrap(),
            truncation,
        }
    }

    fn spec<C: Watch>(condition: C) -> WaitSpec<C> {
        WaitSpec::new(condition, Duration::from_secs(1)).unwrap()
    }

    /// Runs a wait against scripted polls, returning the poll times and the outcome.
    #[expect(
        clippy::disallowed_methods,
        reason = "an arbitrary anchor: only differences from it are asserted"
    )]
    fn run<C: Watch>(
        spec: WaitSpec<C>,
        mut polls: impl FnMut(u32) -> Poll<C::Seen>,
    ) -> (Vec<Duration>, WaitOutcome<C::Seen>) {
        let t0 = Instant::now();
        let mut now = t0;
        let mut waiting = Waiting::start(spec, now);
        let mut times = Vec::new();
        for n in 0.. {
            times.push(now - t0);
            match waiting.observe(polls(n), now) {
                Next::PollAt(next, at) => {
                    assert!(at > now, "polls must move forward");
                    waiting = next;
                    now = at;
                }
                Next::Done(outcome) => return (times, outcome),
            }
        }
        unreachable!()
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "an arbitrary anchor: only differences from it are asserted"
    )]
    fn a_reported_change_brings_the_next_poll_forward_and_never_back() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let spec =
            WaitSpec::new(Condition::Appears("Save".into()), Duration::from_secs(10)).unwrap();
        let waiting = Waiting::start(spec, t0);
        // Backed off: after some polls the next one is a second away.
        let mut waiting = waiting;
        let mut now = t0;
        let mut next_at = t0;
        for _ in 0..8 {
            now = next_at;
            match waiting.observe(Poll::Retryable("no window".into()), now) {
                Next::PollAt(next, at) => {
                    waiting = next;
                    next_at = at;
                }
                Next::Done(outcome) => panic!("{outcome:?}"),
            }
        }
        assert_eq!(next_at - now, MAX_DELAY);
        let (waiting, at) = waiting.on_change(now + ms(10));
        assert_eq!(at, now + ms(10) + SETTLE, "soon after the change");
        // A later report in the same burst doesn't push it back.
        let (waiting, again) = waiting.on_change(now + ms(40));
        assert_eq!(again, at);
        // And the backoff starts over.
        let Next::PollAt(_, after) = waiting.observe(Poll::Retryable("x".into()), at) else {
            panic!()
        };
        assert_eq!(after, at + FIRST_DELAY);
    }

    #[test]
    fn satisfied_on_the_first_poll_that_shows_it() {
        let (times, outcome) = run(spec(Condition::Appears("Saved".into())), |n| {
            Poll::Seen(snapshot(
                if n < 2 { &["Saving…"] } else { &["Saved"] },
                Truncation::Complete,
            ))
        });
        assert_eq!(times.len(), 3);
        assert!(matches!(outcome, WaitOutcome::Satisfied { polls: 3, .. }));
    }

    #[test]
    fn times_out_at_the_deadline_with_backoff_and_keeps_the_last_snapshot() {
        let (times, outcome) = run(spec(Condition::Appears("never".into())), |_| {
            Poll::Seen(snapshot(&["something else"], Truncation::Complete))
        });
        let ms: Vec<u128> = times.iter().map(Duration::as_millis).collect();
        // 100 ms, ×1.5 each time, the last poll exactly at the 1 s deadline.
        assert_eq!(ms, [0, 100, 250, 475, 812, 1000]);
        let WaitOutcome::TimedOut {
            last,
            polls,
            elapsed,
            last_error,
        } = outcome
        else {
            panic!("expected a timeout, got {outcome:?}")
        };
        assert_eq!(polls, 6);
        assert_eq!(elapsed, Duration::from_secs(1));
        assert!(last.is_some());
        assert_eq!(last_error, None);
    }

    #[test]
    fn disappearance_needs_a_complete_snapshot() {
        // Text absent from a partial snapshot proves nothing: keep polling.
        let (_, outcome) = run(spec(Condition::Disappears("Loading".into())), |_| {
            Poll::Seen(snapshot(&[], Truncation::NodeBudget))
        });
        assert!(
            matches!(outcome, WaitOutcome::TimedOut { .. }),
            "{outcome:?}"
        );
        let (_, outcome) = run(spec(Condition::Disappears("Loading".into())), |n| {
            Poll::Seen(if n == 0 {
                snapshot(&["Loading"], Truncation::Complete)
            } else {
                snapshot(&[], Truncation::Complete)
            })
        });
        assert!(
            matches!(outcome, WaitOutcome::Satisfied { polls: 2, .. }),
            "{outcome:?}"
        );
    }

    #[test]
    fn retryable_errors_keep_polling_and_fatal_ones_stop() {
        let (_, outcome) = run(
            spec(Condition::Frontmost("com.apple.TextEdit".into())),
            |n| {
                if n == 0 {
                    Poll::Retryable("no window yet".into())
                } else {
                    Poll::Seen(snapshot(&[], Truncation::Complete))
                }
            },
        );
        assert!(
            matches!(outcome, WaitOutcome::Satisfied { polls: 2, .. }),
            "{outcome:?}"
        );
        let (times, outcome) = run(spec(Condition::Appears("x".into())), |_| {
            Poll::Fatal("Accessibility missing".into())
        });
        assert_eq!(times.len(), 1);
        assert_eq!(
            outcome,
            WaitOutcome::Failed {
                reason: "Accessibility missing".into(),
                polls: 1
            }
        );
        let (_, outcome) = run(spec(Condition::Appears("x".into())), |_| {
            Poll::Retryable("busy".into())
        });
        assert!(matches!(
            outcome,
            WaitOutcome::TimedOut { last: None, last_error: Some(ref e), .. } if e == "busy"
        ));
    }

    #[test]
    fn frontmost_matches_name_or_bundle_id_only() {
        let snap = snapshot(&[], Truncation::Complete);
        assert_eq!(
            Condition::Frontmost("TextEdit".into()).verdict(&snap),
            Verdict::Holds
        );
        assert_eq!(
            Condition::Frontmost("com.apple.TextEdit".into()).verdict(&snap),
            Verdict::Holds
        );
        assert_eq!(
            Condition::Frontmost("Text".into()).verdict(&snap),
            Verdict::Not
        );
    }

    #[test]
    fn specs_are_validated() {
        assert_eq!(
            WaitSpec::new(Condition::Appears(String::new()), Duration::from_secs(1)),
            Err(WaitError::EmptyText)
        );
        assert!(WaitSpec::new(Condition::Appears("x".into()), Duration::ZERO).is_err());
        assert!(WaitSpec::new(Condition::Appears("x".into()), Duration::from_secs(61)).is_err());
        assert!(WaitSpec::new(Condition::Appears("x".into()), MAX_TIMEOUT).is_ok());
    }

    fn screen(lines: &[&str]) -> ScreenText {
        use crate::geometry::Rect;
        use crate::geometry::Size;
        use crate::screen_text::{Confidence, TextLine};
        let lines = lines
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let region = Rect::new(0.0, 20.0 * i as f64, 100.0, 20.0).unwrap();
                TextLine::new(
                    (*text).into(),
                    region,
                    Confidence::new(1.0).unwrap(),
                    vec![],
                )
                .unwrap()
            })
            .collect();
        ScreenText::new(Size::new(1280.0, 800.0).unwrap(), lines)
    }

    #[test]
    fn text_waits_judge_recognized_lines() {
        let saved = || TextCondition::Appears("saved".parse().unwrap());
        let (_, outcome) = run(spec(saved()), |n| {
            Poll::Seen(screen(if n < 2 {
                &["Saving…"]
            } else {
                &["All changes Saved"]
            }))
        });
        assert!(
            matches!(outcome, WaitOutcome::Satisfied { polls: 3, .. }),
            "{outcome:?}"
        );
        let gone = TextCondition::Disappears("Loading".parse().unwrap());
        assert_eq!(gone.verdict(&screen(&["loading…"])), Verdict::Not);
        assert_eq!(
            gone.verdict(&screen(&[])),
            Verdict::Holds,
            "a recognition covers the display: absence is known"
        );
        let (_, outcome) = run(spec(saved()), |_| Poll::Seen(screen(&["Saving…"])));
        assert!(
            matches!(outcome, WaitOutcome::TimedOut { last: Some(_), .. }),
            "{outcome:?}"
        );
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "an arbitrary anchor: only differences from it are asserted"
    )]
    fn polls_must_answer_by_the_timeout_plus_a_grace_for_the_last_one() {
        use crate::deadline::Remaining;
        let t0 = Instant::now();
        let waiting = Waiting::start(spec(Condition::Appears("x".into())), t0);
        assert_eq!(
            waiting.poll_deadline().remaining(t0),
            Remaining::Left(Duration::from_secs(1) + POLL_GRACE)
        );
    }
}
