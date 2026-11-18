//! What agents' sessions do to a machine, call by call, as a viewer is
//! told it: each call as it starts and ends, what it does on the screen
//! and where, how it ended, and what it saw and read. Kept as told: what
//! this build doesn't know stays (`Unknown(i32)`), times stay `Duration`s.

use std::time::Duration;

use crate::machine::MachineName;
use doppel_desktop::ax::refs::SnapshotRefs;
use doppel_desktop::ax::tree::AxSnapshot;
use doppel_desktop::geometry::{Point, Rect};
use doppel_desktop::screen_text::{ScreenText, TextQuery};

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
pub const CALLOUT_LASTS: Duration = Duration::from_secs(5);

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
