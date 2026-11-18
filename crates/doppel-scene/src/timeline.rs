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
use doppel_machine::activity as told;
use doppel_machine::machine::MachineName;

pub use crate::recordings::from_trace;

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

/// The moments after an event at `now`, the oldest dropped past [`KEPT`].
pub fn record(moments: &mut Vec<Moment>, event: ActivityEvent, now: f64) {
    match event {
        ActivityEvent::Started(call) => {
            moments.push(Moment {
                start_ms: now,
                end_ms: None,
                call,
                failed: None,
                person: false,
                seen: None,
                refs: None,
                read: None,
            });
            if moments.len() > KEPT {
                moments.drain(..moments.len() - KEPT);
            }
        }
        ActivityEvent::Ended {
            key,
            ending,
            seen,
            read,
            ..
        } => {
            for moment in moments
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
            if moments.iter().any(|moment| moment.call.key == call.key) {
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
            moments.push(Moment {
                start_ms: now - started_ms_ago,
                end_ms,
                call,
                failed,
                person: false,
                seen,
                refs,
                read,
            });
            moments.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms));
            if moments.len() > KEPT {
                moments.drain(..moments.len() - KEPT);
            }
        }
        // The latest call that saw the snapshot: the agent may reuse an id
        // once it restarts.
        ActivityEvent::Named { refs, .. } => {
            let saw = moments.iter_mut().rev().find(|moment| {
                matches!(&moment.seen, Some(Seen::Live(seen)) if seen.id == refs.snapshot())
            });
            if let Some(moment) = saw {
                moment.refs = Some(refs);
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

/// A mark a call left on the screen, showing from `since` until `until`:
/// a frame over an element or a path just after its call ends, or when its
/// agent's next call starts; a ripple for [`MARK_MS`].
#[derive(Debug, Clone, PartialEq)]
pub enum Mark {
    Ripple {
        at: Point,
        gesture: Gesture,
        since: f64,
        until: f64,
    },
    Box {
        frame: Rect,
        label: String,
        since: f64,
        until: f64,
    },
    Path {
        from: Point,
        to: Point,
        since: f64,
        until: f64,
    },
    Failed {
        at: Point,
        reason: String,
        since: f64,
        until: f64,
    },
}

impl Mark {
    /// When it showed.
    pub const fn since(&self) -> f64 {
        match self {
            Self::Ripple { since, .. }
            | Self::Box { since, .. }
            | Self::Path { since, .. }
            | Self::Failed { since, .. } => *since,
        }
    }

    /// When it goes.
    pub const fn until(&self) -> f64 {
        match self {
            Self::Ripple { until, .. }
            | Self::Box { until, .. }
            | Self::Path { until, .. }
            | Self::Failed { until, .. } => *until,
        }
    }

    /// How opaque it is at `at`: whole, then fading over [`MARK_FADE_MS`]
    /// to its end.
    pub fn opacity(&self, at: f64) -> f64 {
        ((self.until() - at) / MARK_FADE_MS).clamp(0.0, 1.0)
    }
}

/// When the mark of `moments[index]` goes: [`SETTLE_MS`] after its call
/// ends, at once if its agent's next call starts sooner, and after
/// [`MARK_MS`] at the latest.
fn mark_until(moments: &[Moment], index: usize) -> f64 {
    let Some(moment) = moments.get(index) else {
        return f64::NEG_INFINITY;
    };
    let session = moment.call.key.session;
    let next = moments
        .iter()
        .skip(index + 1)
        .find(|later| !later.person && later.call.key.session == session)
        .map_or(f64::INFINITY, |later| later.start_ms);
    let settled = moment
        .end_ms
        .map_or(f64::INFINITY, |end| end.max(moment.start_ms) + SETTLE_MS);
    settled.min(next).min(moment.start_ms + MARK_MS)
}

/// One machine's scene at an instant: each agent's, and the marks agents
/// put on the screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    /// Each session that has acted on the machine, by session id.
    pub agents: Vec<AgentScene>,
    /// The marks agents put on the screen that are showing.
    pub callouts: Vec<Shown>,
}

/// One session's part of a machine's scene: its pointer, its call under
/// way, and the marks its calls left.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentScene {
    pub session: u64,
    /// Its client, in its own words.
    pub client: String,
    /// Where its pointer is: the last place it acted.
    pub cursor: Option<Point>,
    /// Its call under way, and since when.
    pub current: Option<(Call, f64)>,
    /// When the look whose band is sweeping the screen began, if one is.
    pub scan: Option<f64>,
    pub marks: Vec<Mark>,
    /// When it last did anything: its pointer fades after a while.
    pub active: f64,
}

impl Scene {
    /// The session's part of the scene, if it has acted.
    pub fn of(&self, session: u64) -> Option<&AgentScene> {
        self.agents.iter().find(|agent| agent.session == session)
    }
}

/// A mark an agent put on the screen, showing from `since` until `until`.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub callout: Arc<Callout>,
    pub since: f64,
    pub until: f64,
}

/// How long an agent's mark takes to fade in and out.
pub const CALLOUT_FADE_MS: f64 = 250.0;

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

/// The scene at `at`: the pointer where the latest call that had a place
/// acted, the call under way (the latest one started), and the marks of
/// calls that started or failed less than [`MARK_MS`] before.
pub fn scene_at(moments: &[Moment], at: f64) -> Scene {
    let mut agents: Vec<AgentScene> = Vec::new();
    for (at_index, moment) in moments
        .iter()
        .enumerate()
        .filter(|(_, moment)| !moment.person && moment.start_ms <= at)
    {
        let session = moment.call.key.session;
        let index = match agents.iter().position(|agent| agent.session == session) {
            Some(index) => index,
            None => {
                agents.push(AgentScene {
                    session,
                    client: moment.call.client.clone(),
                    cursor: None,
                    current: None,
                    scan: None,
                    marks: Vec::new(),
                    active: f64::NEG_INFINITY,
                });
                agents.len() - 1
            }
        };
        let Some(agent) = agents.get_mut(index) else {
            continue;
        };
        if let Some(placed) = moment.call.target.aim() {
            agent.cursor = Some(placed);
        }
        agent.active = agent.active.max(moment.start_ms);
        let shown_until = moment.end_ms.map_or(f64::INFINITY, |end| {
            end.max(moment.start_ms + MIN_UNDER_WAY_MS)
        });
        if at < shown_until {
            agent.current = Some((moment.call.clone(), moment.start_ms));
        }
        if moment.call.gesture == Gesture::Look && at < sweep_ends(moment.start_ms, moment.end_ms) {
            agent.scan = Some(moment.start_ms);
        }
        if let Some(mark) = moment
            .call
            .leaves(moment.start_ms, mark_until(moments, at_index))
            .filter(|mark| at < mark.until())
        {
            agent.marks.push(mark);
        }
        if let Some(end) = moment.end_ms {
            agent.active = agent.active.max(end.min(at));
            if let (Some(reason), Some(cursor)) = (&moment.failed, agent.cursor)
                && end <= at
                && at - end < MARK_MS
            {
                agent.marks.push(Mark::Failed {
                    at: cursor,
                    reason: reason.clone(),
                    since: end,
                    until: end + MARK_MS,
                });
            }
        }
    }
    agents.sort_by_key(|agent| agent.session);
    Scene {
        agents,
        callouts: callouts_at(moments, at),
    }
}

/// One row of a timeline strip: a session's calls, with who made them.
#[derive(Debug, Clone, PartialEq)]
pub struct Lane<'a> {
    pub session: u64,
    pub client: &'a str,
    pub moments: Vec<&'a Moment>,
}

/// The calls in a strip, a lane per session, by session id. A person's
/// stretches of control stay in the lane of the session they took over.
pub fn lanes<'a>(moments: impl IntoIterator<Item = &'a Moment>) -> Vec<Lane<'a>> {
    let mut lanes: Vec<Lane<'a>> = Vec::new();
    for moment in moments {
        let session = moment.call.key.session;
        match lanes.iter_mut().find(|lane| lane.session == session) {
            Some(lane) => {
                if lane.client.is_empty() {
                    lane.client = &moment.call.client;
                }
                lane.moments.push(moment);
            }
            None => lanes.push(Lane {
                session,
                client: &moment.call.client,
                moments: vec![moment],
            }),
        }
    }
    lanes.sort_by_key(|lane| lane.session);
    lanes
}

/// When a mark call's mark goes: after its time, or, for one that lasts
/// until the agent acts, when the next call that may change the screen
/// starts. `None` for a call that shows no mark (or failed).
fn callout_until(moments: &[Moment], index: usize) -> Option<(&Arc<Callout>, f64, f64)> {
    let moment = moments.get(index)?;
    let callout = moment.call.callout.as_ref()?;
    if moment.person || moment.failed.is_some() {
        return None;
    }
    let timed = moment.start_ms + callout.lasts.at_most();
    let until = match callout.lasts {
        CalloutLasts::For(_) => timed,
        CalloutLasts::UntilActed(_) => moments
            .iter()
            .skip(index + 1)
            .filter(|later| later.start_ms >= moment.start_ms && changes(later))
            .map(|later| later.start_ms)
            .fold(timed, f64::min),
    };
    Some((callout, moment.start_ms, until))
}

/// The agents' marks showing at `at`, oldest first.
pub(crate) fn callouts_at(moments: &[Moment], at: f64) -> Vec<Shown> {
    (0..moments.len())
        .filter_map(|index| {
            let (callout, since, until) = callout_until(moments, index)?;
            (since <= at && at < until).then(|| Shown {
                callout: callout.clone(),
                since,
                until,
            })
        })
        .collect()
}

/// The snapshot shown at an instant, which call took it, and whether the
/// machine may have changed since.
#[derive(Debug, Clone, PartialEq)]
pub struct SeenAt {
    pub seen: Seen,
    /// What the agent named its elements.
    pub refs: Option<Arc<SnapshotRefs>>,
    /// When it was taken: its call's end.
    pub since_ms: f64,
    /// A later call (or a person) may have changed the screen before `at`.
    pub acted_since: bool,
}

/// The latest snapshot a call had seen by `at`.
pub fn seen_at(moments: &[Moment], at: f64) -> Option<SeenAt> {
    let (taken, seen, since_ms, refs) = moments
        .iter()
        .enumerate()
        .filter_map(|(index, moment)| {
            let end = moment.end_ms.filter(|end| *end <= at)?;
            Some((index, moment.seen.as_ref()?, end, &moment.refs))
        })
        .max_by(|a, b| a.2.total_cmp(&b.2))?;
    let started = moments
        .get(taken)
        .map_or(f64::INFINITY, |moment| moment.start_ms);
    let acted_since = moments.iter().enumerate().any(|(index, moment)| {
        index != taken && moment.start_ms >= started && moment.start_ms <= at && changes(moment)
    });
    Some(SeenAt {
        seen: seen.clone(),
        refs: refs.clone(),
        since_ms,
        acted_since,
    })
}

/// The latest text a call had read by `at`, when it was read, and whether
/// the screen may have changed since.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadAt {
    pub read: Arc<TextRead>,
    pub since_ms: f64,
    pub acted_since: bool,
}

pub fn read_at(moments: &[Moment], at: f64) -> Option<ReadAt> {
    let (taken, read, since_ms) = moments
        .iter()
        .enumerate()
        .filter_map(|(index, moment)| {
            let end = moment.end_ms.filter(|end| *end <= at)?;
            Some((index, moment.read.as_ref()?, end))
        })
        .max_by(|a, b| a.2.total_cmp(&b.2))?;
    let started = moments
        .get(taken)
        .map_or(f64::INFINITY, |moment| moment.start_ms);
    Some(ReadAt {
        read: read.clone(),
        since_ms,
        acted_since: acted_between(moments, Some(taken), started, at),
    })
}

/// Whether a call other than `skip` that may change the screen started
/// between `from` and `to`.
fn acted_between(moments: &[Moment], skip: Option<usize>, from: f64, to: f64) -> bool {
    moments.iter().enumerate().any(|(index, moment)| {
        Some(index) != skip && moment.start_ms >= from && moment.start_ms <= to && changes(moment)
    })
}

/// A person's own read of a machine's screen text, asked at `at_ms`.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLook {
    pub at_ms: f64,
    pub outcome: TextLooked,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TextLooked {
    Loading,
    Read(Arc<TextRead>),
    Failed(String),
}

/// What the text layer shows at `at`: the newer of the agent's latest read
/// and the person's own.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading<'a> {
    Agent(ReadAt),
    Person {
        look: &'a TextLook,
        acted_since: bool,
    },
    Nothing,
}

pub fn reading<'a>(moments: &[Moment], look: Option<&'a TextLook>, at: f64) -> Reading<'a> {
    match (read_at(moments, at), look) {
        (Some(agent), Some(look)) if agent.since_ms > look.at_ms => Reading::Agent(agent),
        (Some(_) | None, Some(look)) => Reading::Person {
            look,
            acted_since: acted_between(moments, None, look.at_ms, at),
        },
        (Some(agent), None) => Reading::Agent(agent),
        (None, None) => Reading::Nothing,
    }
}

/// A person's own look at a machine's elements, asked at `at_ms`.
#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    pub at_ms: f64,
    pub outcome: Looked,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Looked {
    Loading,
    Seen(Arc<AxSnapshot>),
    Failed(String),
}

/// What the elements panel shows at `at`: the newer of the agent's latest
/// snapshot and the person's own look.
#[derive(Debug, Clone, PartialEq)]
pub enum Showing<'a> {
    Agent(SeenAt),
    Person {
        look: &'a Look,
        /// A call may have changed the screen since the look.
        acted_since: bool,
    },
    Nothing,
}

pub fn showing<'a>(moments: &[Moment], look: Option<&'a Look>, at: f64) -> Showing<'a> {
    let agent = seen_at(moments, at);
    match (agent, look) {
        (Some(agent), Some(look)) if agent.since_ms > look.at_ms => Showing::Agent(agent),
        (Some(_) | None, Some(look)) => Showing::Person {
            look,
            acted_since: moments.iter().any(|moment| {
                moment.start_ms >= look.at_ms && moment.start_ms <= at && changes(moment)
            }),
        },
        (Some(agent), None) => Showing::Agent(agent),
        (None, None) => Showing::Nothing,
    }
}

/// Whether a call may change what is on the screen.
fn changes(moment: &Moment) -> bool {
    if moment.person {
        return true;
    }
    match moment.call.gesture {
        Gesture::Click
        | Gesture::DoubleClick
        | Gesture::SecondaryClick
        | Gesture::Move
        | Gesture::Drag
        | Gesture::Scroll
        | Gesture::Key
        | Gesture::Type
        | Gesture::Act
        | Gesture::Run
        | Gesture::Lifecycle
        | Gesture::Rollback => true,
        Gesture::Look | Gesture::Wait | Gesture::Mark | Gesture::Checkpoint | Gesture::Other => {
            false
        }
    }
}

/// Whether anything on a timeline still moves at `now`: a call under way,
/// a mark fading, or an agent's mark showing.
pub fn moving(moments: &[Moment], now: f64) -> bool {
    moments.iter().any(|moment| {
        moment.end_ms.is_none() || now - moment.end_ms.unwrap_or(now).max(moment.start_ms) < MARK_MS
    }) || !callouts_at(moments, now).is_empty()
}

/// The latest time anything happened on a timeline.
pub fn latest(moments: &[Moment], now: f64) -> f64 {
    moments
        .iter()
        .map(|moment| moment.end_ms.unwrap_or(now).max(moment.start_ms))
        .fold(f64::NEG_INFINITY, f64::max)
}

impl ActivityEvent {
    /// An event of the activity stream, as the overlay takes it in.
    #[must_use]
    pub fn of(activity: told::Activity) -> Self {
        let key = CallKey {
            session: activity.session,
            request: activity.request,
        };
        let call = |acted: told::OnScreen| {
            let why = acted.why.clone();
            let (gesture, target, callout) = on_screen(acted);
            Call {
                key,
                machine: activity.machine.clone(),
                client: activity.client.clone(),
                caption: activity.caption.clone(),
                gesture,
                target,
                callout,
                why,
            }
        };
        match activity.phase {
            told::Phase::Started(acted) => Self::Started(call(acted)),
            told::Phase::Ended(ended) => Self::Ended {
                key,
                machine: activity.machine,
                ending: Ending::of(ended.ending),
                seen: ended.seen.map(|seen| Arc::new(*seen)),
                read: ended.read.map(|read| Arc::new(TextRead::of(&read))),
            },
            told::Phase::Named(refs) => Self::Named {
                machine: activity.machine,
                refs: Arc::new(refs),
            },
            told::Phase::Recent {
                acted,
                started_ago,
                ended,
            } => Self::Recent {
                call: call(acted),
                started_ms_ago: started_ago.as_secs_f64() * 1000.0,
                ended: ended.map(|(ended, ago)| {
                    Box::new(RecentEnd {
                        ending: Ending::of(ended.ending),
                        seen: ended.seen.map(|seen| Arc::new(*seen)),
                        refs: ended.refs.map(Arc::new),
                        read: ended.read.map(|read| Arc::new(TextRead::of(&read))),
                        ms_ago: ago.as_secs_f64() * 1000.0,
                    })
                }),
            },
        }
    }
}

impl TextRead {
    /// What a call read: lines and matches in display points.
    #[must_use]
    pub fn of(read: &told::TextRead) -> Self {
        Self {
            lines: read
                .text
                .lines()
                .iter()
                .map(|line| ReadLine {
                    text: line.text().to_owned(),
                    frame: line.region(),
                })
                .collect(),
            query: read.query.as_ref().map(|query| query.as_str().to_owned()),
            found: read.found.clone(),
        }
    }
}

impl Ending {
    /// An ending this build can't read still ends the call; a refusal or
    /// a time-out failed it.
    fn of(ending: told::Ending) -> Self {
        match ending {
            told::Ending::Done | told::Ending::Unknown => Self::Done,
            told::Ending::Failed(why) | told::Ending::Refused(why) => Self::Failed(why),
            told::Ending::TimedOut(why) if why.is_empty() => Self::Failed("timed out".into()),
            told::Ending::TimedOut(why) => Self::Failed(format!("timed out: {why}")),
        }
    }
}

/// What a call did on the screen. A mark's place is its own, not where
/// the agent's pointer goes: its call has no target.
pub(crate) fn on_screen(acted: told::OnScreen) -> (Gesture, Target, Option<Arc<Callout>>) {
    let gesture = Gesture::of(acted.gesture);
    match acted.callout {
        None => (gesture, Target::of(acted.target), None),
        Some(callout) => {
            let on = match Target::of(acted.target) {
                Target::Point(at) => Some(CalloutOn::Point(at)),
                Target::Element { frame, label } => Some(CalloutOn::Area { frame, label }),
                Target::Drag { .. } | Target::Nowhere => None,
            };
            let callout = on.map(|on| {
                Arc::new(Callout {
                    on,
                    shape: match callout.shape {
                        told::CalloutShape::Dot => CalloutShape::Dot,
                        told::CalloutShape::Arrow { from } => CalloutShape::Arrow { from },
                        // A shape a newer daemon added is drawn as a box.
                        told::CalloutShape::Box | told::CalloutShape::Unknown(_) => {
                            CalloutShape::Box
                        }
                    },
                    note: callout.note,
                    lasts: match callout.lasts {
                        told::CalloutLasts::For(time) => {
                            CalloutLasts::For(time.as_secs_f64() * 1000.0)
                        }
                        told::CalloutLasts::UntilActed(time) => {
                            CalloutLasts::UntilActed(time.as_secs_f64() * 1000.0)
                        }
                    },
                })
            });
            (gesture, Target::Nowhere, callout)
        }
    }
}

impl Gesture {
    /// A gesture a newer daemon added is drawn as any other call.
    const fn of(gesture: told::Gesture) -> Self {
        match gesture {
            told::Gesture::Click => Self::Click,
            told::Gesture::DoubleClick => Self::DoubleClick,
            told::Gesture::SecondaryClick => Self::SecondaryClick,
            told::Gesture::Move => Self::Move,
            told::Gesture::Drag => Self::Drag,
            told::Gesture::Scroll => Self::Scroll,
            told::Gesture::Key => Self::Key,
            told::Gesture::Type => Self::Type,
            told::Gesture::Act => Self::Act,
            told::Gesture::Look => Self::Look,
            told::Gesture::Wait => Self::Wait,
            told::Gesture::Run => Self::Run,
            told::Gesture::Lifecycle => Self::Lifecycle,
            told::Gesture::Checkpoint => Self::Checkpoint,
            told::Gesture::Rollback => Self::Rollback,
            told::Gesture::Mark => Self::Mark,
            told::Gesture::Other | told::Gesture::Unknown(_) => Self::Other,
        }
    }
}

impl Target {
    fn of(target: Option<told::Target>) -> Self {
        match target {
            Some(told::Target::Point(at)) => Self::Point(at),
            Some(told::Target::Element { frame, label }) => Self::Element { frame, label },
            Some(told::Target::Drag { from, to }) => Self::Drag { from, to },
            None => Self::Nowhere,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_keeps_its_call_and_ending() {
        let started = ActivityEvent::of(told::Activity {
            session: 2,
            request: 5,
            machine: "dev".parse().unwrap(),
            client: "claude".into(),
            caption: "click OK".into(),
            phase: told::Phase::Started(told::OnScreen {
                why: None,
                gesture: told::Gesture::Unknown(77),
                target: Some(told::Target::Point(Point::new(1.0, 2.0).unwrap())),
                callout: None,
            }),
        });
        let ActivityEvent::Started(call) = started else {
            panic!("a start");
        };
        assert_eq!(
            call.key,
            CallKey {
                session: 2,
                request: 5
            }
        );
        assert_eq!(call.gesture, Gesture::Other);
        assert_eq!(call.target, Target::Point(Point::new(1.0, 2.0).unwrap()));
        let ended = ActivityEvent::of(told::Activity {
            session: 2,
            request: 5,
            machine: "dev".parse().unwrap(),
            client: String::new(),
            caption: String::new(),
            phase: told::Phase::Ended(told::Ended {
                ending: told::Ending::TimedOut(String::new()),
                seen: None,
                refs: None,
                read: None,
            }),
        });
        assert!(matches!(
            ended,
            ActivityEvent::Ended { ending: Ending::Failed(why), .. } if why == "timed out"
        ));
    }

    fn call(request: u64, gesture: Gesture, target: Target) -> Call {
        Call {
            key: CallKey {
                session: 1,
                request,
            },
            machine: "dev".parse().unwrap(),
            client: "claude".into(),
            caption: "a call".into(),
            gesture,
            target,
            callout: None,
            why: None,
        }
    }

    fn at(x: f64, y: f64) -> Point {
        Point::new(x, y).unwrap()
    }

    fn ended(request: u64, ending: Ending) -> ActivityEvent {
        ActivityEvent::Ended {
            key: CallKey {
                session: 1,
                request,
            },
            machine: "dev".parse().unwrap(),
            ending,
            seen: None,
            read: None,
        }
    }

    #[test]
    fn the_snapshot_shown_is_the_latest_taken_and_says_when_the_agent_acted_since() {
        use doppel_desktop::agent::{ProcessId, RunningApp};
        use doppel_desktop::ax::tree::{AxNode, AxTree, Role, SnapshotId, States, Truncation};
        let snapshot = |id| {
            Arc::new(AxSnapshot {
                id: SnapshotId::new(id),
                app: RunningApp {
                    pid: ProcessId::new(1),
                    bundle_id: None,
                    name: "Calculator".into(),
                },
                tree: AxTree::new(vec![(
                    None,
                    AxNode {
                        role: Role::Window,
                        subrole: None,
                        title: None,
                        description: None,
                        identifier: None,
                        value: None,
                        frame: None,
                        states: States::default(),
                        actions: Vec::new(),
                    },
                )])
                .unwrap(),
                truncation: Truncation::Complete,
            })
        };
        let look = |request, id, start: f64, moments: &mut Vec<Moment>| {
            record(
                moments,
                ActivityEvent::Started(call(request, Gesture::Look, Target::Nowhere)),
                start,
            );
            record(
                moments,
                ActivityEvent::Ended {
                    key: CallKey {
                        session: 1,
                        request,
                    },
                    machine: "dev".parse().unwrap(),
                    ending: Ending::Done,
                    seen: Some(snapshot(id)),
                    read: None,
                },
                start + 100.0,
            );
        };
        let mut moments = Vec::new();
        look(1, 3, 0.0, &mut moments);
        record(
            &mut moments,
            ActivityEvent::Started(call(2, Gesture::Wait, Target::Nowhere)),
            200.0,
        );
        record(&mut moments, ended(2, Ending::Done), 300.0);
        record(
            &mut moments,
            ActivityEvent::Started(call(3, Gesture::Key, Target::Nowhere)),
            400.0,
        );
        record(&mut moments, ended(3, Ending::Done), 450.0);
        look(4, 4, 500.0, &mut moments);
        let id = |at| match seen_at(&moments, at).map(|shown| shown.seen) {
            Some(Seen::Live(seen)) => Some(seen.id.get()),
            Some(Seen::Recorded(_)) | None => None,
        };
        assert_eq!(id(50.0), None);
        assert_eq!(id(300.0), Some(3));
        assert_eq!(
            seen_at(&moments, 300.0).map(|shown| (shown.since_ms, shown.acted_since)),
            Some((100.0, false))
        );
        // A key press may change the screen; a wait doesn't.
        assert_eq!(
            seen_at(&moments, 420.0).map(|shown| shown.acted_since),
            Some(true)
        );
        assert_eq!(id(700.0), Some(4));
        assert_eq!(
            seen_at(&moments, 700.0).map(|shown| shown.acted_since),
            Some(false)
        );
        // Names reach the call that saw their snapshot, and come with it.
        use doppel_desktop::ax::refs::Generation;
        use doppel_desktop::ax::tree::NodeIndex;
        let named = |id| ActivityEvent::Named {
            machine: "dev".parse().unwrap(),
            refs: Arc::new(SnapshotRefs::new(
                SnapshotId::new(id),
                Generation::FIRST,
                vec![NodeIndex::ROOT],
            )),
        };
        record(&mut moments, named(3), 710.0);
        record(&mut moments, named(9), 720.0);
        let generation = |at| {
            seen_at(&moments, at)
                .and_then(|shown| shown.refs)
                .map(|refs| refs.generation())
        };
        assert_eq!(generation(300.0), Some(Generation::FIRST));
        assert_eq!(generation(700.0), None);
    }

    #[test]
    fn a_recent_call_is_placed_back_in_time_once() {
        let mut moments = Vec::new();
        let recent = |request| ActivityEvent::Recent {
            call: call(request, Gesture::Click, Target::Nowhere),
            started_ms_ago: 5000.0,
            ended: Some(Box::new(RecentEnd {
                ending: Ending::Failed("no window".into()),
                seen: None,
                read: None,
                refs: None,
                ms_ago: 4000.0,
            })),
        };
        record(&mut moments, recent(1), 10_000.0);
        record(
            &mut moments,
            ActivityEvent::Recent {
                call: call(2, Gesture::Wait, Target::Nowhere),
                started_ms_ago: 9000.0,
                ended: None,
            },
            10_000.0,
        );
        // In start order, the one under way still open.
        let placed: Vec<(f64, Option<f64>)> = moments
            .iter()
            .map(|moment| (moment.start_ms, moment.end_ms))
            .collect();
        assert_eq!(placed, vec![(1000.0, None), (5000.0, Some(6000.0))]);
        assert_eq!(moments[1].failed.as_deref(), Some("no window"));
        // Told again, it stays as it is.
        record(&mut moments, recent(1), 20_000.0);
        assert_eq!(moments.len(), 2);
    }

    #[test]
    fn the_reading_band_finishes_its_pass_after_a_look_ends() {
        let look = || ActivityEvent::Started(call(1, Gesture::Look, Target::Nowhere));
        let scan = |moments: &[Moment], at: f64| scene_at(moments, at).of(1).unwrap().scan;
        let mut quick = Vec::new();
        record(&mut quick, look(), 0.0);
        record(&mut quick, ended(1, Ending::Done), 300.0);
        assert_eq!(scan(&quick, 400.0), Some(0.0));
        assert_eq!(scan(&quick, SWEEP_MS - 1.0), Some(0.0));
        assert_eq!(scan(&quick, SWEEP_MS), None);
        // A longer look runs to the end of the pass it ends in.
        let mut long = Vec::new();
        record(&mut long, look(), 0.0);
        record(&mut long, ended(1, Ending::Done), SWEEP_MS + 100.0);
        assert_eq!(scan(&long, 2.0 * SWEEP_MS - 1.0), Some(0.0));
        assert_eq!(scan(&long, 2.0 * SWEEP_MS), None);
        // One still looking keeps sweeping, each pass from the top.
        let mut open = Vec::new();
        record(&mut open, look(), 0.0);
        assert_eq!(scan(&open, 10.0 * SWEEP_MS), Some(0.0));
        assert_eq!(swept(500.0, 500.0), 0.0);
        assert_eq!(swept(0.0, SWEEP_MS / 2.0), 0.5);
        assert_eq!(swept(0.0, SWEEP_MS), 0.0);
    }

    #[test]
    fn a_call_is_under_way_until_it_ends_and_a_little_after() {
        let mut moments = Vec::new();
        record(
            &mut moments,
            ActivityEvent::Started(call(1, Gesture::Click, Target::Point(at(10.0, 20.0)))),
            0.0,
        );
        let scene = scene_at(&moments, 100.0);
        assert_eq!(scene.of(1).unwrap().cursor, Some(at(10.0, 20.0)));
        assert!(scene.of(1).unwrap().current.is_some());
        assert!(matches!(
            scene.of(1).unwrap().marks.as_slice(),
            [Mark::Ripple { .. }]
        ));
        record(&mut moments, ended(1, Ending::Done), 200.0);
        assert!(
            scene_at(&moments, 500.0).of(1).unwrap().current.is_some(),
            "shown at least 600 ms"
        );
        assert!(scene_at(&moments, 700.0).of(1).unwrap().current.is_none());
        assert!(
            scene_at(&moments, 2000.0).of(1).unwrap().marks.is_empty(),
            "the ripple faded"
        );
    }

    #[test]
    fn each_session_has_its_own_pointer_call_and_lane() {
        let mut moments = Vec::new();
        let by = |session, request, gesture, target| Call {
            key: CallKey { session, request },
            client: format!("agent {session}"),
            ..call(request, gesture, target)
        };
        record(
            &mut moments,
            ActivityEvent::Started(by(4, 1, Gesture::Click, Target::Point(at(1.0, 1.0)))),
            0.0,
        );
        record(
            &mut moments,
            ActivityEvent::Started(by(2, 1, Gesture::Click, Target::Point(at(9.0, 9.0)))),
            10.0,
        );
        record(
            &mut moments,
            ActivityEvent::Started(by(4, 2, Gesture::Type, Target::Nowhere)),
            20.0,
        );
        let scene = scene_at(&moments, 30.0);
        let sessions: Vec<u64> = scene.agents.iter().map(|agent| agent.session).collect();
        assert_eq!(sessions, [2, 4], "by session id");
        let four = scene.of(4).unwrap();
        assert_eq!(four.cursor, Some(at(1.0, 1.0)));
        assert_eq!(four.client, "agent 4");
        assert_eq!(
            four.current.as_ref().map(|(call, _)| call.gesture),
            Some(Gesture::Type)
        );
        let two = scene.of(2).unwrap();
        assert_eq!(two.cursor, Some(at(9.0, 9.0)));
        assert_eq!(
            two.current.as_ref().map(|(call, _)| call.key.request),
            Some(1)
        );
        let lanes = lanes(&moments);
        let lanes: Vec<(u64, &str, usize)> = lanes
            .iter()
            .map(|lane| (lane.session, lane.client, lane.moments.len()))
            .collect();
        assert_eq!(lanes, [(2, "agent 2", 1), (4, "agent 4", 2)]);
    }

    #[test]
    fn a_mark_goes_just_after_its_call_ends_or_when_the_next_starts() {
        let press = || {
            call(
                1,
                Gesture::Act,
                Target::Element {
                    frame: Rect::new(10.0, 10.0, 80.0, 30.0).unwrap(),
                    label: "Save".into(),
                },
            )
        };
        let mut moments = Vec::new();
        record(&mut moments, ActivityEvent::Started(press()), 0.0);
        let marks = |moments: &[Moment], at| scene_at(moments, at).of(1).unwrap().marks.len();
        assert_eq!(marks(&moments, 900.0), 1, "under way, it shows");
        record(&mut moments, ended(1, Ending::Done), 30.0);
        let scene = scene_at(&moments, 100.0);
        let shown = &scene.of(1).unwrap().marks[0];
        assert_eq!(shown.until(), 30.0 + SETTLE_MS);
        assert_eq!(shown.opacity(30.0 + SETTLE_MS - MARK_FADE_MS), 1.0);
        assert_eq!(
            marks(&moments, 30.0 + SETTLE_MS),
            0,
            "gone once the screen changed"
        );

        let mut moments = Vec::new();
        record(&mut moments, ActivityEvent::Started(press()), 0.0);
        record(
            &mut moments,
            ActivityEvent::Started(call(2, Gesture::Look, Target::Nowhere)),
            80.0,
        );
        assert_eq!(marks(&moments, 90.0), 0, "the next call took over");
    }

    #[test]
    fn a_failure_marks_where_the_pointer_was() {
        let mut moments = Vec::new();
        record(
            &mut moments,
            ActivityEvent::Started(call(1, Gesture::Move, Target::Point(at(5.0, 5.0)))),
            0.0,
        );
        record(
            &mut moments,
            ActivityEvent::Started(call(2, Gesture::Type, Target::Nowhere)),
            10.0,
        );
        record(
            &mut moments,
            ended(2, Ending::Failed("no focus".into())),
            50.0,
        );
        let scene = scene_at(&moments, 60.0);
        assert!(scene.of(1).unwrap().marks.contains(&Mark::Failed {
            at: at(5.0, 5.0),
            reason: "no focus".into(),
            since: 50.0,
            until: 50.0 + MARK_MS,
        }));
        assert_eq!(
            scene
                .of(1)
                .unwrap()
                .current
                .as_ref()
                .map(|(call, _)| call.key.request),
            Some(2)
        );
    }

    #[test]
    fn calls_read_by_what_they_did() {
        let element = Target::Element {
            frame: Rect::new(0.0, 0.0, 10.0, 10.0).unwrap(),
            label: "OK".into(),
        };
        assert_eq!(call(1, Gesture::Act, element.clone()).describe(), "“OK”");
        assert_eq!(element.aim(), Some(at(5.0, 5.0)));
        assert_eq!(
            call(1, Gesture::Look, Target::Nowhere).describe(),
            "Looking at the screen"
        );
        assert_eq!(
            call(1, Gesture::Other, Target::Nowhere).describe(),
            "a call"
        );
    }

    #[test]
    fn a_timeline_keeps_its_latest_calls() {
        let mut moments = Vec::new();
        for request in 0..(KEPT as u64 + 5) {
            record(
                &mut moments,
                ActivityEvent::Started(call(request, Gesture::Wait, Target::Nowhere)),
                0.0,
            );
        }
        assert_eq!(moments.len(), KEPT);
        assert_eq!(
            moments.first().map(|moment| moment.call.key.request),
            Some(5)
        );
    }

    #[test]
    fn a_timeline_moves_while_a_call_runs_or_a_mark_fades() {
        let mut moments = Vec::new();
        record(
            &mut moments,
            ActivityEvent::Started(call(1, Gesture::Wait, Target::Nowhere)),
            0.0,
        );
        assert!(moving(&moments, 10_000.0));
        record(&mut moments, ended(1, Ending::Done), 100.0);
        assert!(moving(&moments, 1000.0));
        assert!(!moving(&moments, 5000.0));
        assert!((latest(&moments, 5000.0) - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_panel_shows_the_newer_of_the_agents_snapshot_and_a_persons_look() {
        let mut moments = Vec::new();
        record(
            &mut moments,
            ActivityEvent::Started(call(1, Gesture::Look, Target::Nowhere)),
            0.0,
        );
        record(
            &mut moments,
            ActivityEvent::Ended {
                key: CallKey {
                    session: 1,
                    request: 1,
                },
                machine: "dev".parse().unwrap(),
                ending: Ending::Done,
                seen: Some(Arc::new(snapshot_with_id(5))),
                read: None,
            },
            100.0,
        );
        let look = Look {
            at_ms: 200.0,
            outcome: Looked::Loading,
        };
        assert!(matches!(showing(&moments, None, 300.0), Showing::Agent(_)));
        assert!(matches!(
            showing(&moments, Some(&look), 300.0),
            Showing::Person {
                acted_since: false,
                ..
            }
        ));
        // A key press after the look may have changed the screen.
        record(
            &mut moments,
            ActivityEvent::Started(call(2, Gesture::Key, Target::Nowhere)),
            400.0,
        );
        record(&mut moments, ended(2, Ending::Done), 450.0);
        assert!(matches!(
            showing(&moments, Some(&look), 500.0),
            Showing::Person {
                acted_since: true,
                ..
            }
        ));
        // The agent's next snapshot is newer than the look.
        record(
            &mut moments,
            ActivityEvent::Started(call(3, Gesture::Look, Target::Nowhere)),
            600.0,
        );
        record(
            &mut moments,
            ActivityEvent::Ended {
                key: CallKey {
                    session: 1,
                    request: 3,
                },
                machine: "dev".parse().unwrap(),
                ending: Ending::Done,
                seen: Some(Arc::new(snapshot_with_id(6))),
                read: None,
            },
            700.0,
        );
        assert!(matches!(
            showing(&moments, Some(&look), 800.0),
            Showing::Agent(SeenAt {
                since_ms: 700.0,
                ..
            })
        ));
        assert_eq!(showing(&[], None, 0.0), Showing::Nothing);
    }

    #[test]
    fn the_text_layer_shows_the_newer_of_the_agents_read_and_a_persons() {
        let read = |text: &str| {
            Some(Arc::new(TextRead {
                lines: vec![ReadLine {
                    text: text.into(),
                    frame: Rect::new(0.0, 0.0, 10.0, 10.0).unwrap(),
                }],
                query: None,
                found: Vec::new(),
            }))
        };
        let mut moments = Vec::new();
        let find = |request, start: f64, text: &str, moments: &mut Vec<Moment>| {
            record(
                moments,
                ActivityEvent::Started(call(request, Gesture::Look, Target::Nowhere)),
                start,
            );
            record(
                moments,
                ActivityEvent::Ended {
                    key: CallKey {
                        session: 1,
                        request,
                    },
                    machine: "dev".parse().unwrap(),
                    ending: Ending::Done,
                    seen: None,
                    read: read(text),
                },
                start + 50.0,
            );
        };
        find(1, 0.0, "Save", &mut moments);
        let words = |shown: Reading<'_>| match shown {
            Reading::Agent(read) => Some(read.read.lines[0].text.clone()),
            Reading::Person { .. } | Reading::Nothing => None,
        };
        assert_eq!(words(reading(&moments, None, 10.0)), None);
        assert_eq!(words(reading(&moments, None, 100.0)), Some("Save".into()));
        let mine = TextLook {
            at_ms: 200.0,
            outcome: TextLooked::Loading,
        };
        assert!(matches!(
            reading(&moments, Some(&mine), 300.0),
            Reading::Person {
                acted_since: false,
                ..
            }
        ));
        find(2, 400.0, "Open", &mut moments);
        assert_eq!(
            words(reading(&moments, Some(&mine), 500.0)),
            Some("Open".into())
        );
        // A call that only read doesn't make the read stale.
        assert_eq!(
            read_at(&moments, 500.0).map(|read| read.acted_since),
            Some(false)
        );
    }

    fn snapshot_with_id(id: u64) -> AxSnapshot {
        use doppel_desktop::agent::{ProcessId, RunningApp};
        use doppel_desktop::ax::tree::{AxNode, AxTree, Role, SnapshotId, States, Truncation};
        AxSnapshot {
            id: SnapshotId::new(id),
            app: RunningApp {
                pid: ProcessId::new(1),
                bundle_id: None,
                name: "Calculator".into(),
            },
            tree: AxTree::new(vec![(
                None,
                AxNode {
                    role: Role::Window,
                    subrole: None,
                    title: None,
                    description: None,
                    identifier: None,
                    value: None,
                    frame: None,
                    states: States::default(),
                    actions: Vec::new(),
                },
            )])
            .unwrap(),
            truncation: Truncation::Complete,
        }
    }

    fn marked(request: u64, lasts: CalloutLasts) -> Call {
        Call {
            callout: Some(Arc::new(Callout {
                on: CalloutOn::Point(at(40.0, 50.0)),
                shape: CalloutShape::Dot,
                note: Some("here".into()),
                lasts,
            })),
            ..call(request, Gesture::Mark, Target::Nowhere)
        }
    }

    #[test]
    fn a_mark_shows_for_its_time_without_moving_the_pointer() {
        let mut moments = Vec::new();
        record(
            &mut moments,
            ActivityEvent::Started(call(1, Gesture::Click, Target::Point(at(1.0, 2.0)))),
            0.0,
        );
        record(&mut moments, ended(1, Ending::Done), 10.0);
        record(
            &mut moments,
            ActivityEvent::Started(marked(2, CalloutLasts::For(5000.0))),
            100.0,
        );
        record(&mut moments, ended(2, Ending::Done), 101.0);
        let scene = scene_at(&moments, 2000.0);
        assert_eq!(scene.of(1).unwrap().cursor, Some(at(1.0, 2.0)));
        assert_eq!(scene.callouts.len(), 1);
        assert_eq!(scene.callouts[0].until, 5100.0);
        assert!(moving(&moments, 4000.0));
        assert!(scene_at(&moments, 5100.0).callouts.is_empty());
        assert!(!moving(&moments, 5200.0));
        assert_eq!(moments[1].call.describe(), "Pointing out: here");
    }

    #[test]
    fn a_mark_until_acted_goes_when_the_agent_next_changes_the_screen() {
        let mut moments = Vec::new();
        record(
            &mut moments,
            ActivityEvent::Started(marked(1, CalloutLasts::UntilActed(5000.0))),
            0.0,
        );
        record(&mut moments, ended(1, Ending::Done), 1.0);
        // Looking changes nothing: the mark stays.
        record(
            &mut moments,
            ActivityEvent::Started(call(2, Gesture::Look, Target::Nowhere)),
            300.0,
        );
        record(
            &mut moments,
            ActivityEvent::Started(call(3, Gesture::Click, Target::Point(at(40.0, 50.0)))),
            900.0,
        );
        assert_eq!(scene_at(&moments, 800.0).callouts.len(), 1);
        assert!(scene_at(&moments, 900.0).callouts.is_empty());
    }

    #[test]
    fn a_refused_mark_shows_nothing() {
        let mut moments = Vec::new();
        record(
            &mut moments,
            ActivityEvent::Started(marked(1, CalloutLasts::For(5000.0))),
            0.0,
        );
        record(&mut moments, ended(1, Ending::Failed("no".into())), 1.0);
        assert!(scene_at(&moments, 500.0).callouts.is_empty());
    }

    #[test]
    fn a_trace_becomes_the_machines_moments_and_people_draw_nothing() {
        use crate::recordings::{
            Actor, Recording, RecordingId, TraceCall, TraceLine, TraceOutcome, from_trace,
        };
        let traced = |start_ms, machine: Option<&str>, by, outcome| {
            TraceLine::Call(Box::new(TraceCall {
                why: None,
                start_ms,
                end_ms: start_ms + 500,
                machine: machine.map(|raw| raw.parse().unwrap()),
                call: "click".into(),
                outcome,
                error: None,
                waited_ms: None,
                queued_ms: None,
                by,
                seen: None,
                refs: None,
                read: None,
                acted: Some((Gesture::Click, Target::Point(at(5.0, 6.0)))),
                callout: None,
            }))
        };
        let recording = Recording {
            id: RecordingId::new("r".into()),
            videos: Vec::new(),
            trace: vec![
                traced(100, Some("dev"), Actor::Session, TraceOutcome::Done),
                traced(200, Some("web"), Actor::Session, TraceOutcome::Done),
                traced(300, None, Actor::Session, TraceOutcome::TimedOut),
                traced(2000, Some("dev"), Actor::Person, TraceOutcome::Done),
                TraceLine::Unreadable(String::new()),
            ],
        };
        let dev: MachineName = "dev".parse().unwrap();
        let moments = from_trace(&recording, &dev);
        let starts: Vec<f64> = moments.iter().map(|moment| moment.start_ms).collect();
        assert_eq!(starts, [100.0, 300.0, 2000.0]);
        assert_eq!(moments[1].failed.as_deref(), Some("timed out"));
        assert!(moments[2].person);
        // The person's stretch draws nothing over the screen.
        let scene = scene_at(&moments, 2100.0);
        assert!(scene.agents.iter().all(|agent| agent.current.is_none()));
    }
}
