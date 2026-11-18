//! One timeline per machine: the calls agents made on it as moments (when
//! each started and ended, what it did and where), and the scene the overlay
//! draws over its screen at any instant, worked out from them. Pure: events
//! and the window's clock in, moments and scenes out. Times are milliseconds
//! on the window's clock.
//!
//! Events come in as `doppel_machine::activity` parses the stream; taking
//! them in folds what this build doesn't know into `Other` or `Done`,
//! failures into one `Failed`, and times into milliseconds.

use std::sync::Arc;

use doppel_desktop::ax::refs::SnapshotRefs;
use doppel_desktop::ax::tree::AxSnapshot;
use doppel_desktop::geometry::{Point, Rect};
use doppel_machine::machine::MachineName;

use crate::recordings::{Actor, Recording, TraceOutcome};

mod activity;
mod scene;
mod seen;
#[cfg(test)]
mod tests;

pub(crate) use activity::on_screen;
pub use scene::{AgentScene, CALLOUT_FADE_MS, Lane, Mark, Scene, Shown, lanes};
pub use seen::{Look, Looked, ReadAt, Reading, SeenAt, Showing, TextLook, TextLooked};

/// What a call read off the screen by text recognition: each line, and
/// where what it looked for was found. In display points.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRead {
    pub lines: Vec<ReadLine>,
    /// What it looked for, if it looked for something.
    pub query: Option<String>,
    /// Where that was found: the matched words.
    pub found: Vec<Rect>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReadLine {
    pub text: String,
    pub frame: Rect,
}

/// What kind of thing a call does.
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
}

/// A mark an agent put on the screen for the people watching. It has its
/// own place: the agent's pointer doesn't go there.
#[derive(Debug, Clone, PartialEq)]
pub struct Callout {
    pub on: CalloutOn,
    pub shape: CalloutShape,
    /// One short line.
    pub note: Option<String>,
    pub lasts: CalloutLasts,
}

/// What a mark is on.
#[derive(Debug, Clone, PartialEq)]
pub enum CalloutOn {
    Point(Point),
    /// An area, or an element's frame and what a person would call it
    /// (empty for an area).
    Area {
        frame: Rect,
        label: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CalloutShape {
    Box,
    Dot,
    /// From this point, or from wherever there is room.
    Arrow {
        from: Option<Point>,
    },
}

/// How long a mark stays, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CalloutLasts {
    For(f64),
    /// Until the next call that may change the screen, or this long at most.
    UntilActed(f64),
}

impl CalloutLasts {
    const fn at_most(self) -> f64 {
        match self {
            Self::For(ms) | Self::UntilActed(ms) => ms,
        }
    }
}

/// Where a call acts.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Point(Point),
    /// An accessibility element: its frame and what a person would call it.
    Element {
        frame: Rect,
        label: String,
    },
    Drag {
        from: Point,
        to: Point,
    },
    Nowhere,
}

/// A call: its session and its request there. A start and its end share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallKey {
    pub session: u64,
    pub request: u64,
}

/// A call of an agent's session, as it started.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub key: CallKey,
    pub machine: MachineName,
    /// The session's client, in its own words.
    pub client: String,
    /// What it does, in the client's words; never typed text.
    pub caption: String,
    pub gesture: Gesture,
    pub target: Target,
    /// What a mark call shows ([`Gesture::Mark`]).
    pub callout: Option<Arc<Callout>>,
    /// Why the session's client made it, in its words, when it said.
    pub why: Option<String>,
}

/// How a call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    Done,
    Failed(String),
}

/// What agents did, as the daemon streams it.
#[derive(Debug, Clone, PartialEq)]
pub enum ActivityEvent {
    Started(Call),
    Ended {
        key: CallKey,
        machine: MachineName,
        ending: Ending,
        /// The accessibility snapshot it saw, if it took one.
        seen: Option<Arc<AxSnapshot>>,
        /// What it read off the screen, if it read it.
        read: Option<Arc<TextRead>>,
    },
    /// A call from before the console began watching: how many
    /// milliseconds before it started, and, unless it is still under way,
    /// how it ended.
    Recent {
        call: Call,
        started_ms_ago: f64,
        ended: Option<Box<RecentEnd>>,
    },
    /// The agent named the elements of a snapshot one of its calls saw,
    /// as its model was shown them (`s3e12`).
    Named {
        machine: MachineName,
        refs: Arc<SnapshotRefs>,
    },
}

/// How a call from before the console began watching ended.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentEnd {
    pub ending: Ending,
    pub seen: Option<Arc<AxSnapshot>>,
    /// What the agent named the elements of `seen`.
    pub refs: Option<Arc<SnapshotRefs>>,
    pub read: Option<Arc<TextRead>>,
    pub ms_ago: f64,
}

/// Where the snapshot a call saw is: with the call, or on a recording's
/// trace line, to fetch.
#[derive(Debug, Clone, PartialEq)]
pub enum Seen {
    Live(Arc<AxSnapshot>),
    Recorded(u32),
}

impl ActivityEvent {
    #[must_use]
    pub const fn machine(&self) -> &MachineName {
        match self {
            Self::Started(call) | Self::Recent { call, .. } => &call.machine,
            Self::Ended { machine, .. } | Self::Named { machine, .. } => machine,
        }
    }
}

/// A call on the timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Moment {
    pub start_ms: f64,
    /// `None` while it's under way.
    pub end_ms: Option<f64>,
    pub call: Call,
    /// Why it didn't succeed, if it didn't.
    pub failed: Option<String>,
    /// A person who had control, not the agent: it draws nothing over the
    /// screen (they saw their own pointer).
    pub person: bool,
    /// The accessibility snapshot it saw, if it took one.
    pub seen: Option<Seen>,
    /// What the agent named that snapshot's elements, once it has.
    pub refs: Option<Arc<SnapshotRefs>>,
    /// What it read off the screen, if it read it.
    pub read: Option<Arc<TextRead>>,
}

/// How many moments a timeline keeps for each machine.
const KEPT: usize = 300;

/// The longest a mark stays.
pub const MARK_MS: f64 = 1600.0;
/// How long a mark shows after its call ends: the screen changes then,
/// and a mark left over what changed is noise.
const SETTLE_MS: f64 = 150.0;
/// How long a mark takes to fade, at its end.
pub const MARK_FADE_MS: f64 = 150.0;

/// A call too quick to see is shown under way for at least this long.
const MIN_UNDER_WAY_MS: f64 = 600.0;

/// How long the reading band takes to sweep down the screen once.
pub(crate) const SWEEP_MS: f64 = 1300.0;

/// When the band of a look from `start` to `end` (None: still looking)
/// leaves the screen: at the end of the pass under way when the look ends,
/// so it never stops midway, and after one pass at the least.
pub(crate) fn sweep_ends(start: f64, end: Option<f64>) -> f64 {
    end.map_or(f64::INFINITY, |end| {
        start + ((end - start) / SWEEP_MS).ceil().max(1.0) * SWEEP_MS
    })
}

/// How far down the screen a band begun at `start` is at `now`, 0 to 1.
pub fn swept(start: f64, now: f64) -> f64 {
    (now - start).rem_euclid(SWEEP_MS) / SWEEP_MS
}

/// One machine's calls as moments, in the order they started (calls that
/// started together keep the order they came in). Recorded live, it keeps
/// the latest [`KEPT`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Timeline {
    moments: Vec<Moment>,
}

impl Timeline {
    /// Its moments, in start order.
    #[must_use]
    pub fn moments(&self) -> &[Moment] {
        &self.moments
    }

    /// Takes in an event at `now`, the oldest moment dropped past [`KEPT`].
    pub fn record(&mut self, event: ActivityEvent, now: f64) {
        match event {
            ActivityEvent::Started(call) => self.keep(Moment {
                start_ms: now,
                end_ms: None,
                call,
                failed: None,
                person: false,
                seen: None,
                refs: None,
                read: None,
            }),
            ActivityEvent::Ended {
                key,
                ending,
                seen,
                read,
                ..
            } => {
                for moment in self
                    .moments
                    .iter_mut()
                    .filter(|moment| moment.call.key == key && moment.end_ms.is_none())
                {
                    moment.end_ms = Some(now);
                    moment.failed = failure(&ending);
                    moment.seen = seen.clone().map(Seen::Live);
                    moment.read = read.clone();
                }
            }
            ActivityEvent::Recent {
                call,
                started_ms_ago,
                ended,
            } => {
                // Told once, when watching begins; a call already here stays as it is.
                if self
                    .moments
                    .iter()
                    .any(|moment| moment.call.key == call.key)
                {
                    return;
                }
                let (end_ms, failed, seen, refs, read) = match ended {
                    Some(ended) => (
                        Some(now - ended.ms_ago),
                        failure(&ended.ending),
                        ended.seen.map(Seen::Live),
                        ended.refs,
                        ended.read,
                    ),
                    None => (None, None, None, None, None),
                };
                self.keep(Moment {
                    start_ms: now - started_ms_ago,
                    end_ms,
                    call,
                    failed,
                    person: false,
                    seen,
                    refs,
                    read,
                });
            }
            // The latest call that saw the snapshot: the agent may reuse an id
            // once it restarts.
            ActivityEvent::Named { refs, .. } => {
                let saw = self.moments.iter_mut().rev().find(|moment| {
                    matches!(&moment.seen, Some(Seen::Live(seen)) if seen.id == refs.snapshot())
                });
                if let Some(moment) = saw {
                    moment.refs = Some(refs);
                }
            }
        }
    }

    /// Places `moment` after every one that started no later, and drops
    /// the oldest past [`KEPT`].
    fn keep(&mut self, moment: Moment) {
        let at = self
            .moments
            .partition_point(|kept| kept.start_ms.total_cmp(&moment.start_ms).is_le());
        self.moments.insert(at, moment);
        if self.moments.len() > KEPT {
            self.moments.drain(..self.moments.len() - KEPT);
        }
    }

    /// Puts what `resolve` makes of each snapshot still on a recording's
    /// trace line in its place.
    pub fn fill_seen(&mut self, mut resolve: impl FnMut(u32) -> Option<Seen>) {
        for moment in &mut self.moments {
            if let Some(Seen::Recorded(line)) = moment.seen {
                moment.seen = resolve(line);
            }
        }
    }
}

fn failure(ending: &Ending) -> Option<String> {
    match ending {
        Ending::Done => None,
        Ending::Failed(reason) => Some(reason.clone()),
    }
}

impl Target {
    /// Where it puts the pointer: a point, an element's centre, a drag's end.
    pub(crate) fn aim(&self) -> Option<Point> {
        match self {
            Self::Point(at) => Some(*at),
            Self::Element { frame, .. } => Some(frame.center()),
            Self::Drag { to, .. } => Some(*to),
            Self::Nowhere => None,
        }
    }
}

impl Gesture {
    const fn marks_a_point(self) -> bool {
        match self {
            Self::Click | Self::DoubleClick | Self::SecondaryClick | Self::Scroll => true,
            Self::Move
            | Self::Drag
            | Self::Key
            | Self::Type
            | Self::Act
            | Self::Look
            | Self::Wait
            | Self::Run
            | Self::Lifecycle
            | Self::Checkpoint
            | Self::Rollback
            | Self::Mark
            | Self::Other => false,
        }
    }

    /// The colour family it's shown in on the timeline.
    pub const fn family(self) -> Family {
        match self {
            Self::Click
            | Self::DoubleClick
            | Self::SecondaryClick
            | Self::Move
            | Self::Drag
            | Self::Scroll
            | Self::Act => Family::Pointer,
            Self::Key | Self::Type => Family::Keys,
            Self::Look => Family::Look,
            Self::Wait => Family::Wait,
            Self::Run => Family::Run,
            Self::Lifecycle | Self::Checkpoint | Self::Rollback => Family::Machine,
            Self::Mark => Family::Mark,
            Self::Other => Family::Other,
        }
    }
}

/// The colour family a call is shown in on the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Pointer,
    Keys,
    Look,
    Wait,
    Run,
    Machine,
    Mark,
    Other,
}

impl Call {
    /// The mark it leaves where it acted, if it leaves one.
    fn leaves(&self, since: f64, until: f64) -> Option<Mark> {
        match &self.target {
            Target::Element { frame, label } => Some(Mark::Box {
                frame: *frame,
                label: label.clone(),
                since,
                until,
            }),
            Target::Drag { from, to } => Some(Mark::Path {
                from: *from,
                to: *to,
                since,
                until,
            }),
            // A pulse where it acted, over in a moment: not cut short.
            Target::Point(at) => self.gesture.marks_a_point().then_some(Mark::Ripple {
                at: *at,
                gesture: self.gesture,
                since,
                until: since + MARK_MS,
            }),
            Target::Nowhere => None,
        }
    }

    /// What it reads as, for a person.
    pub fn describe(&self) -> String {
        let named = match &self.target {
            Target::Element { label, .. } if !label.is_empty() => Some(format!("“{label}”")),
            Target::Element { .. } | Target::Point(_) | Target::Drag { .. } | Target::Nowhere => {
                None
            }
        };
        match self.gesture {
            Gesture::Click => named.unwrap_or_else(|| "Clicking".into()),
            Gesture::DoubleClick => "Double-clicking".into(),
            Gesture::SecondaryClick => "Right-clicking".into(),
            Gesture::Move => "Moving the pointer".into(),
            Gesture::Drag => "Dragging".into(),
            Gesture::Scroll => "Scrolling".into(),
            Gesture::Key => "Pressing a key".into(),
            Gesture::Type => "Typing".into(),
            // A press, a value set, a focus: the element is what matters.
            Gesture::Act => named.unwrap_or_else(|| "Using an element".into()),
            Gesture::Look => "Looking at the screen".into(),
            Gesture::Wait => "Waiting".into(),
            Gesture::Run => "Running a command".into(),
            Gesture::Mark => match self
                .callout
                .as_ref()
                .and_then(|callout| callout.note.as_ref())
            {
                Some(note) => format!("Pointing out: {note}"),
                None => "Pointing something out".into(),
            },
            Gesture::Checkpoint => "Keeping a checkpoint".into(),
            Gesture::Rollback => "Rolling back to a checkpoint".into(),
            Gesture::Lifecycle if self.caption.is_empty() => "Changing the machine".into(),
            Gesture::Lifecycle | Gesture::Other => self.caption.clone(),
        }
    }
}

impl Timeline {
    /// A recording's calls on `machine` (and those naming none) as moments,
    /// timed from the session's start, in the order they started whatever
    /// the trace's order.
    #[must_use]
    pub fn from_trace(recording: &Recording, machine: &MachineName) -> Self {
        let mut calls: Vec<_> = recording
            .calls()
            .filter(|call| call.machine.as_ref().is_none_or(|named| named == machine))
            .collect();
        calls.sort_by_key(|call| call.start_ms);
        let moments = calls
            .into_iter()
            .enumerate()
            .map(|(index, traced)| {
                let (gesture, target) = traced
                    .acted
                    .clone()
                    .unwrap_or((Gesture::Other, Target::Nowhere));
                let callout = traced.callout.clone();
                Moment {
                    start_ms: traced.start_ms as f64,
                    end_ms: Some(traced.end_ms as f64),
                    call: Call {
                        key: CallKey {
                            session: 0,
                            request: index as u64,
                        },
                        machine: machine.clone(),
                        client: String::new(),
                        caption: traced.call.clone(),
                        gesture,
                        target,
                        callout,
                        why: traced.why.clone(),
                    },
                    failed: match traced.outcome {
                        TraceOutcome::Done => None,
                        TraceOutcome::Failed
                        | TraceOutcome::TimedOut
                        | TraceOutcome::Refused
                        | TraceOutcome::Unknown(_) => Some(
                            traced
                                .error
                                .clone()
                                .or_else(|| traced.outcome.words())
                                .unwrap_or_default(),
                        ),
                    },
                    person: traced.by == Actor::Person,
                    seen: traced.seen.map(Seen::Recorded),
                    refs: traced.refs.clone(),
                    read: traced.read.clone(),
                }
            })
            .collect();
        Self { moments }
    }
}
