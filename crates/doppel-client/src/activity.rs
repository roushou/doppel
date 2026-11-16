//! What agents' sessions do to the machines, as they do it: the daemon
//! answers `WatchActivity` on a connection of its own, then sends an event
//! as each call on a machine gets its turn and as it ends. Parsed here,
//! once, into typed values; a person's own input is never streamed.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use doppel_machine::ax::refs::SnapshotRefs;
use doppel_machine::ax::tree::AxSnapshot;
use doppel_machine::deadline::Deadline;
use doppel_machine::geometry::{Point, Rect};
use doppel_machine::machine::MachineName;
use doppel_machine::screen_text::{ScreenText, TextQuery};
use doppel_protocol::pb::common::v1 as common;
use doppel_protocol::pb::daemon::v1 as daemon;

use crate::display::Stream;
use crate::{ClientError, Clock};

/// One call of a session, as it started or ended. A start and its end carry
/// the same session and request.
#[derive(Debug, Clone, PartialEq)]
pub struct Activity {
    pub session: u64,
    pub request: u64,
    pub machine: MachineName,
    /// The session's client, in its own words.
    pub client: String,
    /// What the call does, in the client's words; never typed text.
    pub caption: String,
    pub phase: Phase,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    /// It got its machine's turn.
    Started(OnScreen),
    Ended(Ended),
    /// A call from before the viewer began watching, told first: how long
    /// before it started, and, unless it is still under way, how it ended
    /// and how long before.
    Recent {
        acted: OnScreen,
        started_ago: Duration,
        ended: Option<(Ended, Duration)>,
    },
    /// The session's client named the elements of a snapshot one of its
    /// calls on the machine saw (`doppel mcp` calls them `s3e12`). Not a
    /// call: `request` is the naming's own.
    Named(SnapshotRefs),
}

/// What a call does on the screen.
#[derive(Debug, Clone, PartialEq)]
pub struct OnScreen {
    pub gesture: Gesture,
    /// Where on the display it acts, or what a mark is on, in points, when
    /// that is known.
    pub target: Option<Target>,
    /// What a mark call shows the people watching ([`Gesture::Mark`]).
    pub callout: Option<Box<Callout>>,
    /// Why the session's client made the call, in its words, when it said.
    pub why: Option<String>,
}

/// A mark an agent put on the screen for the people watching, drawn on the
/// call's target.
#[derive(Debug, Clone, PartialEq)]
pub struct Callout {
    pub shape: CalloutShape,
    /// One line, at most 200 characters.
    pub note: Option<String>,
    pub lasts: CalloutLasts,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CalloutShape {
    Box,
    Dot,
    /// From this point, when given, in display points.
    Arrow {
        from: Option<Point>,
    },
    /// One this build doesn't know, as sent.
    Unknown(i32),
}

/// How long a mark stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalloutLasts {
    For(Duration),
    /// Until the session's next call that acts on the screen, or this long
    /// at most.
    UntilActed(Duration),
}

/// How long a mark stays when the daemon doesn't say.
const CALLOUT_LASTS: Duration = Duration::from_secs(5);

/// How a call ended, and the accessibility snapshot it saw, if it took
/// one: as the agent got it, but with no password field's value. One the
/// client can't read is left out.
#[derive(Debug, Clone, PartialEq)]
pub struct Ended {
    pub ending: Ending,
    pub seen: Option<Box<AxSnapshot>>,
    /// What the session's client named the elements of `seen`: told only of
    /// a recent call; a live call's names come later, as [`Phase::Named`].
    pub refs: Option<SnapshotRefs>,
    /// What it read off the screen by text recognition. One the client
    /// can't read is left out.
    pub read: Option<Box<TextRead>>,
}

/// What a call read off the screen, and where what it looked for was found.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRead {
    pub text: ScreenText,
    /// What it looked for: a text read's query, or a text wait's text.
    pub query: Option<TextQuery>,
    /// Where that was found, in reading order.
    pub found: Vec<Rect>,
}

impl TextRead {
    /// `None` for one without its text, or with text, a query or a region
    /// this build can't read.
    pub(crate) fn from_wire(read: daemon::TextRead) -> Option<Self> {
        Some(Self {
            text: read.text?.try_into().ok()?,
            query: match read.query {
                Some(query) => Some(query.parse().ok()?),
                None => None,
            },
            found: read
                .found
                .into_iter()
                .map(Rect::try_from)
                .collect::<Result<_, _>>()
                .ok()?,
        })
    }
}

/// What kind of thing a call does, for a viewer to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    Click,
    DoubleClick,
    SecondaryClick,
    Move,
    Drag,
    Scroll,
    Key,
    Type,
    /// An accessibility action on an element: press, set a value, focus.
    Act,
    /// Reads the screen.
    Look,
    Wait,
    /// Runs a program, or reads or writes a file, in the guest.
    Run,
    /// Starts, stops, pauses, restores, forks or deletes the machine.
    Lifecycle,
    /// Keeps the machine's state as a checkpoint, to return to.
    Checkpoint,
    /// Returns the machine to a checkpoint, discarding everything since.
    Rollback,
    /// Points something out to the people watching; changes nothing.
    Mark,
    Other,
    /// One this build doesn't know, as sent.
    Unknown(i32),
}

/// Where a call acts, in display points.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Point(Point),
    /// An accessibility element: its frame, with some area, and what a
    /// person would call it.
    Element {
        frame: Rect,
        label: String,
    },
    Drag {
        from: Point,
        to: Point,
    },
}

/// How a call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    Done,
    Failed(String),
    TimedOut(String),
    Refused(String),
    /// An ending this build can't read: it still ends the call.
    Unknown,
}

/// The activity stream, until the daemon closes it.
#[derive(Debug)]
pub struct ActivityStream<C> {
    channel: Stream<C>,
}

impl<C: Clock> ActivityStream<C> {
    /// Connects to the daemon on `socket` and starts watching.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn open(socket: &Path, clock: C, by: Deadline) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(socket).map_err(ClientError::from_connect)?;
        Self::over(stream, clock, by)
    }

    /// As [`ActivityStream::open`], on a connection already made.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn over(stream: UnixStream, clock: C, by: Deadline) -> Result<Self, ClientError> {
        let (channel, daemon::Done {}, _) =
            Stream::open(stream, clock, daemon::WatchActivity {}, by)?;
        Ok(Self { channel })
    }

    /// The next call started or ended. Events this build can't read (no
    /// phase, a machine name that isn't one) are skipped.
    ///
    /// # Errors
    /// [`ClientError::Timeout`] when none comes by `by`;
    /// [`ClientError::Closed`] once the daemon ends the stream.
    pub fn next(&mut self, by: Deadline) -> Result<Activity, ClientError> {
        loop {
            if let Some(daemon::daemon_event::Kind::Activity(activity)) = self.channel.event(by)?
                && let Some(read) = Activity::from_wire(activity)
            {
                return Ok(read);
            }
        }
    }
}

impl Activity {
    /// An event as the daemon sends it, read; `None` for one this build
    /// can't read (no phase, a machine name that isn't one).
    pub fn from_wire(activity: daemon::Activity) -> Option<Self> {
        let phase = match activity.phase? {
            daemon::activity::Phase::Started(started) => {
                Phase::Started(OnScreen::from_wire(started))
            }
            daemon::activity::Phase::Ended(ended) => Phase::Ended(ending(ended)),
            daemon::activity::Phase::Recent(recent) => Phase::Recent {
                acted: OnScreen::from_wire(recent.started?),
                started_ago: Duration::from_millis(recent.started_ms_ago),
                ended: recent
                    .ended
                    .zip(recent.ended_ms_ago)
                    .map(|(ended, ago)| (ending(ended), Duration::from_millis(ago))),
            },
            daemon::activity::Phase::Named(refs) => Phase::Named(refs.try_into().ok()?),
        };
        Some(Self {
            session: activity.session,
            request: activity.request,
            machine: activity.machine.parse().ok()?,
            client: activity.client,
            caption: activity.caption,
            phase,
        })
    }
}

/// How a call ended, and what it saw, as the daemon says it.
fn ending(ended: daemon::CallEnded) -> Ended {
    Ended {
        ending: match ended.outcome {
            Some(daemon::call_ended::Outcome::Done(_)) => Ending::Done,
            Some(daemon::call_ended::Outcome::Failed(why)) => Ending::Failed(why),
            Some(daemon::call_ended::Outcome::TimedOut(why)) => Ending::TimedOut(why),
            Some(daemon::call_ended::Outcome::Refused(why)) => Ending::Refused(why),
            None => Ending::Unknown,
        },
        seen: ended
            .seen
            .and_then(|seen| AxSnapshot::try_from(seen).ok())
            .map(Box::new),
        refs: ended.refs.and_then(|refs| refs.try_into().ok()),
        read: ended.read.and_then(TextRead::from_wire).map(Box::new),
    }
}

impl OnScreen {
    /// What a call did on the screen, and where, as the daemon says it. A
    /// mark is on its own place, not the call's target.
    pub(crate) fn from_wire(started: daemon::CallStarted) -> Self {
        let (target, callout) = match started.mark {
            Some(mark) => {
                let target = mark.at.clone().and_then(|at| match at {
                    daemon::screen_mark::At::Point(at) => {
                        Target::from_wire(daemon::call_started::Target::Point(at))
                    }
                    daemon::screen_mark::At::Area(area) => {
                        Target::from_wire(daemon::call_started::Target::Element(area))
                    }
                });
                (target, Some(Box::new(Callout::from_wire(&mark))))
            }
            None => (started.target.and_then(Target::from_wire), None),
        };
        let why = Some(started.why.trim())
            .filter(|why| !why.is_empty())
            .map(ToOwned::to_owned);
        Self {
            gesture: Gesture::from_wire(started.gesture),
            target,
            callout,
            why,
        }
    }
}

impl Callout {
    fn from_wire(mark: &daemon::ScreenMark) -> Self {
        use daemon::MarkShape as Wire;
        let from = Target::point(mark.from);
        Self {
            shape: match Wire::try_from(mark.shape) {
                Ok(Wire::Box) => CalloutShape::Box,
                Ok(Wire::Dot) => CalloutShape::Dot,
                Ok(Wire::Arrow) => CalloutShape::Arrow { from },
                Ok(Wire::Unspecified) | Err(_) => CalloutShape::Unknown(mark.shape),
            },
            note: Some(mark.note.trim())
                .filter(|note| !note.is_empty())
                .map(ToOwned::to_owned),
            lasts: match mark.lasts {
                Some(daemon::screen_mark::Lasts::ForMs(ms)) => {
                    CalloutLasts::For(Duration::from_millis(ms.into()))
                }
                Some(daemon::screen_mark::Lasts::UntilActedMs(ms)) => {
                    CalloutLasts::UntilActed(Duration::from_millis(ms.into()))
                }
                None => CalloutLasts::For(CALLOUT_LASTS),
            },
        }
    }
}

impl Gesture {
    fn from_wire(raw: i32) -> Self {
        use daemon::Gesture as Wire;
        match Wire::try_from(raw) {
            Ok(Wire::Click) => Self::Click,
            Ok(Wire::DoubleClick) => Self::DoubleClick,
            Ok(Wire::SecondaryClick) => Self::SecondaryClick,
            Ok(Wire::Move) => Self::Move,
            Ok(Wire::Drag) => Self::Drag,
            Ok(Wire::Scroll) => Self::Scroll,
            Ok(Wire::Key) => Self::Key,
            Ok(Wire::Type) => Self::Type,
            Ok(Wire::Act) => Self::Act,
            Ok(Wire::Look) => Self::Look,
            Ok(Wire::Wait) => Self::Wait,
            Ok(Wire::Run) => Self::Run,
            Ok(Wire::Lifecycle) => Self::Lifecycle,
            Ok(Wire::Checkpoint) => Self::Checkpoint,
            Ok(Wire::Rollback) => Self::Rollback,
            Ok(Wire::Mark) => Self::Mark,
            Ok(Wire::Other | Wire::Unspecified) => Self::Other,
            Err(_) => Self::Unknown(raw),
        }
    }
}

impl Target {
    /// `None` for a target without the points it needs, a point off the
    /// display, or an element's frame without area.
    fn from_wire(target: daemon::call_started::Target) -> Option<Self> {
        use daemon::call_started::Target as Wire;
        match target {
            Wire::Point(at) => Some(Self::Point(Self::point(Some(at))?)),
            Wire::Element(element) => {
                let frame = Rect::try_from(element.frame?).ok()?;
                frame.has_area().then_some(Self::Element {
                    frame,
                    label: element.label,
                })
            }
            Wire::Drag(drag) => Some(Self::Drag {
                from: Self::point(drag.from)?,
                to: Self::point(drag.to)?,
            }),
        }
    }

    fn point(at: Option<common::Point>) -> Option<Point> {
        let at = at?;
        Point::new(at.x, at.y).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(target: Option<daemon::call_started::Target>, gesture: i32) -> daemon::Activity {
        daemon::Activity {
            session: 3,
            request: 9,
            machine: "dev".into(),
            client: "claude".into(),
            caption: "click".into(),
            phase: Some(daemon::activity::Phase::Started(daemon::CallStarted {
                why: String::new(),
                gesture,
                target,
                mark: None,
            })),
        }
    }

    #[test]
    fn a_started_call_keeps_its_gesture_and_place() {
        let read = Activity::from_wire(started(
            Some(daemon::call_started::Target::Point(common::Point {
                x: 10.0,
                y: 20.0,
            })),
            daemon::Gesture::Click as i32,
        ));
        assert_eq!(
            read.map(|activity| activity.phase),
            Some(Phase::Started(OnScreen {
                why: None,
                gesture: Gesture::Click,
                target: Some(Target::Point(Point::new(10.0, 20.0).unwrap())),
                callout: None,
            }))
        );
    }

    #[test]
    fn unknown_gestures_are_kept_and_bad_places_dropped() {
        let read = Activity::from_wire(started(
            Some(daemon::call_started::Target::Element(
                daemon::ElementTarget {
                    frame: Some(common::Rect {
                        origin: Some(common::Point { x: 1.0, y: 1.0 }),
                        size: Some(common::Size {
                            width: 0.0,
                            height: 4.0,
                        }),
                    }),
                    label: "OK".into(),
                },
            )),
            99,
        ));
        assert_eq!(
            read.map(|activity| activity.phase),
            Some(Phase::Started(OnScreen {
                why: None,
                gesture: Gesture::Unknown(99),
                target: None,
                callout: None,
            }))
        );
    }

    #[test]
    fn a_mark_is_drawn_on_its_own_place() {
        let mut event = started(None, daemon::Gesture::Mark as i32);
        event.phase = Some(daemon::activity::Phase::Started(daemon::CallStarted {
            why: String::new(),
            gesture: daemon::Gesture::Mark as i32,
            target: None,
            mark: Some(daemon::ScreenMark {
                shape: daemon::MarkShape::Arrow as i32,
                at: Some(daemon::screen_mark::At::Point(common::Point {
                    x: 5.0,
                    y: 6.0,
                })),
                from: Some(common::Point { x: 1.0, y: 2.0 }),
                note: " here ".into(),
                lasts: Some(daemon::screen_mark::Lasts::UntilActedMs(3000)),
            }),
        }));
        assert_eq!(
            Activity::from_wire(event).map(|activity| activity.phase),
            Some(Phase::Started(OnScreen {
                why: None,
                gesture: Gesture::Mark,
                target: Some(Target::Point(Point::new(5.0, 6.0).unwrap())),
                callout: Some(Box::new(Callout {
                    shape: CalloutShape::Arrow {
                        from: Point::new(1.0, 2.0).ok()
                    },
                    note: Some("here".into()),
                    lasts: CalloutLasts::UntilActed(Duration::from_secs(3)),
                })),
            }))
        );
    }

    #[test]
    fn events_without_a_phase_or_a_valid_machine_are_skipped() {
        let mut no_phase = started(None, 1);
        no_phase.phase = None;
        assert_eq!(Activity::from_wire(no_phase), None);
        let mut bad_name = started(None, 1);
        bad_name.machine = "Not A Name".into();
        assert_eq!(Activity::from_wire(bad_name), None);
    }

    #[test]
    fn endings_say_why() {
        let mut ended = started(None, 1);
        ended.phase = Some(daemon::activity::Phase::Ended(daemon::CallEnded {
            outcome: Some(daemon::call_ended::Outcome::TimedOut("no window".into())),
            seen: None,
            refs: None,
            read: None,
        }));
        assert_eq!(
            Activity::from_wire(ended).map(|activity| activity.phase),
            Some(Phase::Ended(Ended {
                ending: Ending::TimedOut("no window".into()),
                seen: None,
                refs: None,
                read: None,
            }))
        );
    }

    #[test]
    fn names_are_read_and_a_generation_of_zero_is_skipped() {
        let named = |generation| {
            let mut event = started(None, 1);
            event.phase = Some(daemon::activity::Phase::Named(daemon::ElementRefs {
                snapshot: 7,
                generation,
                nodes: vec![3, 5],
            }));
            Activity::from_wire(event).map(|activity| activity.phase)
        };
        let Some(Phase::Named(refs)) = named(2) else {
            panic!("expected names");
        };
        let refs: Vec<_> = refs
            .refs()
            .map(|(node, r)| (node.get(), r.to_string()))
            .collect();
        assert_eq!(refs, [(3, "s2e1".to_owned()), (5, "s2e2".to_owned())]);
        assert_eq!(named(0), None);
    }
}
