//! The desktop window for running machines: the browser viewer's design
//! and features (`web/`), drawn with GPUI.
//!
//! A sidebar lists the machines, running and stopped, and the agents
//! driving them; the stage shows one machine, two side by side, or every
//! running one. Each machine's panel has its lifecycle (start, shut down,
//! pause, checkpoints, forks, delete), its screen with what the agent does
//! drawn over it, and a strip of the agent's last minute of calls. Taking
//! control of a screen holds the agents' calls on it, and sends the
//! person's clicks, drags, scrolls and keys to it until they hand it back.
//!
//! Everything the window knows and does goes through the [`Daemon`] port,
//! which the shell implements. Each screen brings a [`FrameSource`]:
//! whatever produces its frames, or an [`EncodedSource`] of H.264 frames,
//! which the viewer decodes. The viewer reads each source on a thread of
//! its own, keeps only its latest frame, and draws it with GPUI's surface
//! element, which takes NV12 directly.

mod app;
mod ask;
mod decode;
mod field;
mod icons;
mod input;
mod inspect;
mod overlay;
mod pixels;
mod player;
mod reading;
mod replay;
mod screen;
mod srgb;
mod theme;
mod ui;

pub use doppel_machine::agent::AgentReport;
pub use doppel_machine::agent::AgentStatus;
pub use doppel_machine::ax::tree::AxSnapshot;
pub use doppel_machine::bundle::CheckpointLabel;
pub use doppel_machine::deadline::Clock;
pub use doppel_machine::geometry::Size;
pub use doppel_machine::machine::MachineName;
pub use doppel_machine::vm::MachineReport;
pub use doppel_scene::encoded::{EncodedFrame, FrameKind};
pub use doppel_scene::recordings::LocalTime;
use doppel_scene::{
    elements, model, panel, recordings, report, resync, stage, steering, timeline, transport,
};
pub use input::{Button, GuestInput, Key, Modifier, NamedKey, PointerPhase, Position};
pub use model::{
    Hold, Machine, MachineState, Order, Run, Session, SessionActivity, SessionId, Verdict,
};
pub use recordings::{
    Actor, Recording, RecordingId, RecordingSummary, TraceCall, TraceLine, TraceOutcome, Video,
    VideoName,
};
pub use timeline::{
    ActivityEvent, Call, CallKey, Callout, CalloutLasts, CalloutOn, CalloutShape, Ending, Gesture,
    ReadLine, RecentEnd, Target, TextRead,
};

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

/// A frame lent by a source: 4:2:0 bi-planar YCbCr (NV12), planes packed
/// without row padding. Checked once, when made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRef<'a> {
    width: u32,
    height: u32,
    luma: &'a [u8],
    chroma: &'a [u8],
}

/// Why planes don't make a frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("a {width}x{height} frame: NV12 needs even, non-zero sides")]
    Size { width: u32, height: u32 },
    #[error("the {plane} plane has {actual} bytes, not {expected}")]
    PlaneLength {
        plane: &'static str,
        expected: usize,
        actual: usize,
    },
}

impl<'a> FrameRef<'a> {
    /// `luma`: `width` bytes per row, `height` rows. `chroma`: Cb, Cr pairs,
    /// `width` bytes per row, `height / 2` rows.
    ///
    /// # Errors
    /// See [`FrameError`].
    pub fn new(
        width: u32,
        height: u32,
        luma: &'a [u8],
        chroma: &'a [u8],
    ) -> Result<Self, FrameError> {
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(FrameError::Size { width, height });
        }
        let pixels = width as usize * height as usize;
        for (plane, expected, actual) in [
            ("luma", pixels, luma.len()),
            ("chroma", pixels / 2, chroma.len()),
        ] {
            if actual != expected {
                return Err(FrameError::PlaneLength {
                    plane,
                    expected,
                    actual,
                });
            }
        }
        Ok(Self {
            width,
            height,
            luma,
            chroma,
        })
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The rows of a plane, top to bottom.
    fn rows(&self, plane: Plane) -> std::slice::ChunksExact<'a, u8> {
        match plane {
            Plane::Luma => self.luma.chunks_exact(self.width as usize),
            Plane::Chroma => self.chroma.chunks_exact(self.width as usize),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plane {
    Luma,
    Chroma,
}

/// What a source has next.
#[derive(Debug)]
pub enum Next<'a> {
    Frame(FrameRef<'a>),
    /// Nothing changed for a while: ask again.
    Unchanged,
    /// The source is done (the machine stopped).
    Ended,
}

/// Where a screen's frames come from. Called on a thread of its own, so it
/// may block, but should return [`Next::Unchanged`] within about a second.
pub trait FrameSource: Send + 'static {
    /// # Errors
    /// The source failed; its screen shows the error and reads no more.
    fn next_frame(&mut self) -> Result<Next<'_>, SourceError>;
}

/// What an encoded source has next.
#[derive(Debug)]
pub enum NextEncoded {
    Frame(EncodedFrame),
    /// Nothing changed for a while: ask again.
    Unchanged,
    /// The source is done (the machine stopped).
    Ended,
}

/// Where a screen's H.264 frames come from, in order. Called on a thread of
/// its own, as [`FrameSource`] is.
pub trait EncodedSource: Send + 'static {
    /// # Errors
    /// The source failed; its screen shows the error and reads no more.
    fn next_frame(&mut self) -> Result<NextEncoded, SourceError>;
}

/// Where a screen's input goes. Called on a thread of its own, one input
/// at a time, in the order the person gave them.
pub trait InputSink: Send + 'static {
    /// # Errors
    /// The input wasn't delivered; the screen shows why, and later input is
    /// still sent.
    fn send(&mut self, input: GuestInput) -> Result<(), SourceError>;
}

/// A source's failure, as its screen shows it.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct SourceError(Box<dyn Error + Send + Sync>);

impl SourceError {
    pub fn new(error: impl Error + Send + Sync + 'static) -> Self {
        Self(Box::new(error))
    }
}

/// Taking control of a screen's machine from whoever drives it (an agent),
/// and handing it back. Called from the screen's input thread, in order
/// with its input.
pub trait Takeover: Send + 'static {
    /// # Errors
    /// Control wasn't given (someone else has it); the screen shows why.
    fn take(&mut self) -> Result<(), SourceError>;
    /// # Errors
    /// Control couldn't be handed back; the screen shows why.
    fn hand_back(&mut self) -> Result<(), SourceError>;
}

/// One machine's display: its frames, its size in points, and where its
/// input goes.
#[derive(Debug)]
pub struct Screen {
    source: Frames,
    points: Option<Size>,
    input: Option<Box<dyn InputSink>>,
    takeover: Option<Box<dyn Takeover>>,
}

impl std::fmt::Debug for dyn Takeover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Takeover")
    }
}

impl std::fmt::Debug for dyn InputSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InputSink")
    }
}

/// Where a screen's frames come from: a closed set, read by its view.
#[derive(Debug)]
enum Frames {
    Raw(Box<dyn FrameSource>),
    Encoded(Box<dyn EncodedSource>),
}

impl std::fmt::Debug for dyn EncodedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EncodedSource")
    }
}

impl std::fmt::Debug for dyn FrameSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FrameSource")
    }
}

impl Screen {
    pub fn new(source: impl FrameSource) -> Self {
        Self {
            source: Frames::Raw(Box::new(source)),
            points: None,
            input: None,
            takeover: None,
        }
    }

    /// A screen whose frames come encoded, and are decoded here.
    pub fn encoded(source: impl EncodedSource) -> Self {
        Self {
            source: Frames::Encoded(Box::new(source)),
            points: None,
            input: None,
            takeover: None,
        }
    }

    /// The display's size in points: where what agents do is drawn over it.
    /// Without it, the screen shows no overlay.
    #[must_use]
    pub const fn with_points(mut self, points: Size) -> Self {
        self.points = Some(points);
        self
    }

    /// Sends what the person does on this screen to `sink`. Without one,
    /// the screen is view-only.
    #[must_use]
    pub fn with_input(mut self, sink: impl InputSink) -> Self {
        self.input = Some(Box::new(sink));
        self
    }

    /// Makes input wait for control: the screen is view-only until the
    /// person presses "Take control", and sends input until they hand it
    /// back. Without one, input is always sent.
    #[must_use]
    pub fn with_takeover(mut self, takeover: impl Takeover) -> Self {
        self.takeover = Some(Box::new(takeover));
        self
    }
}

/// The time: monotonic, from the one [`Clock`] port, for the timelines and
/// the overlay's motion; and the local date a checkpoint's suggested label
/// carries.
pub trait LocalClock: Clock + Send + Sync + 'static {
    fn local(&self) -> LocalTime;
}

/// Why the daemon didn't do what the window asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DaemonError {
    /// No daemon answers: the window shows it's disconnected.
    #[error("{0}")]
    Unreachable(String),
    /// It answered, and refused, with why.
    #[error("{0}")]
    Refused(String),
}

/// What an activity stream has next.
#[derive(Debug)]
pub enum NextActivity {
    Event(ActivityEvent),
    /// Nothing happened for a while: ask again.
    Unchanged,
    /// The stream is done (the daemon closed it).
    Ended,
}

/// What agents do, as it happens. Called on a thread of its own, as
/// [`FrameSource`] is.
pub trait ActivitySource: Send + 'static {
    /// # Errors
    /// The stream failed; the window opens another a little later.
    fn next(&mut self) -> Result<NextActivity, SourceError>;
}

/// Everything the window asks of the daemon. Each call may block: the
/// window makes them from threads of its own, never its own.
pub trait Daemon: Send + Sync + 'static {
    /// Every machine, with where it is.
    ///
    /// # Errors
    /// [`DaemonError::Unreachable`] shows the window as disconnected.
    fn machines(&self) -> Result<Vec<Machine>, DaemonError>;

    /// The agents' sessions.
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn sessions(&self) -> Result<Vec<Session>, DaemonError>;

    /// Holds a session's calls, or lets them go on.
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn hold(&self, session: SessionId, hold: Hold) -> Result<(), DaemonError>;

    /// A machine's checkpoints, oldest first.
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn checkpoints(&self, name: &MachineName) -> Result<Vec<CheckpointLabel>, DaemonError>;

    /// A machine's report: its guest agent's state and its checkpoints.
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn report(&self, name: &MachineName) -> Result<MachineReport, DaemonError>;

    /// Carries out a lifecycle order on `name`, and says how it went. It
    /// may take minutes (a start waits for the desktop).
    fn run(&self, name: &MachineName, order: &Order) -> Verdict;

    /// Opens a running machine's screen, with input that waits for
    /// control ([`Screen::with_takeover`]).
    ///
    /// # Errors
    /// See [`DaemonError`]: the machine isn't running.
    fn open_screen(&self, name: &MachineName) -> Result<Screen, DaemonError>;

    /// Starts watching what agents do.
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn watch(&self) -> Result<Box<dyn ActivitySource>, DaemonError>;

    /// The recorded sessions, newest first.
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn recordings(&self) -> Result<Vec<RecordingSummary>, DaemonError>;

    /// One recording: its videos and its trace.
    ///
    /// # Errors
    /// See [`DaemonError`]: it doesn't exist, or can't be read.
    fn open_recording(&self, id: &RecordingId) -> Result<Recording, DaemonError>;

    /// The accessibility snapshot line `line` (from 0) of a recording's
    /// trace kept.
    ///
    /// # Errors
    /// See [`DaemonError`]: that line kept none.
    fn seen_snapshot(&self, id: &RecordingId, line: u32) -> Result<AxSnapshot, DaemonError>;

    /// A person's look at a machine's elements (its frontmost app's focused
    /// window), which costs the agents driving it nothing.
    ///
    /// # Errors
    /// See [`DaemonError`]: the guest's agent is unreachable, or too old.
    fn observe(&self, name: &MachineName) -> Result<AxSnapshot, DaemonError>;

    /// A person's own read of a machine's screen text, which costs the
    /// agents driving it nothing (it reads the host's view of the display).
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn observe_text(&self, name: &MachineName) -> Result<TextRead, DaemonError>;

    /// Where one of a recording's videos is on this Mac, to play.
    ///
    /// # Errors
    /// [`DaemonError::Refused`]: its file isn't here (the daemon is on
    /// another Mac), with why.
    fn video(&self, id: &RecordingId, file: &VideoName) -> Result<PathBuf, DaemonError>;
}

/// Which theme the window shows: the system's appearance, or light or
/// dark whatever the system says. The browser viewer has the same
/// (`web/src/lib/theme.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    /// The next choice round: system, light, dark, and back.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::System => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::System,
        }
    }

    /// How it's kept: what [`str::parse`] reads back.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// A kept theme that isn't one there is.
#[derive(Debug, thiserror::Error)]
#[error("no theme {0:?}: system, light or dark")]
pub struct UnknownTheme(String);

impl std::str::FromStr for ThemeChoice {
    type Err = UnknownTheme;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim() {
            "system" => Ok(Self::System),
            "light" => Ok(Self::Light),
            "dark" => Ok(Self::Dark),
            other => Err(UnknownTheme(other.to_owned())),
        }
    }
}

/// What the person chose, kept across launches.
pub trait Settings: Send + Sync + 'static {
    /// The theme chosen last; [`ThemeChoice::System`] when none was.
    fn theme(&self) -> ThemeChoice;

    /// Keeps the theme chosen.
    ///
    /// # Errors
    /// It couldn't be kept: it holds until the window closes.
    fn keep_theme(&self, choice: ThemeChoice) -> Result<(), SettingsError>;
}

/// Settings that couldn't be kept.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct SettingsError(Box<dyn Error + Send + Sync>);

impl SettingsError {
    pub fn new(error: impl Error + Send + Sync + 'static) -> Self {
        Self(Box::new(error))
    }
}

/// Settings kept nowhere: each launch starts on the system's theme.
struct Unkept;

impl Settings for Unkept {
    fn theme(&self) -> ThemeChoice {
        ThemeChoice::System
    }

    fn keep_theme(&self, _: ThemeChoice) -> Result<(), SettingsError> {
        Ok(())
    }
}

/// The viewer window.
pub struct Viewer {
    daemon: Arc<dyn Daemon>,
    clock: Arc<dyn LocalClock>,
    settings: Arc<dyn Settings>,
    shown: Vec<MachineName>,
}

impl std::fmt::Debug for Viewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewer")
            .field("shown", &self.shown)
            .finish_non_exhaustive()
    }
}

impl Viewer {
    #[must_use]
    pub fn new(daemon: impl Daemon, clock: impl LocalClock) -> Self {
        Self {
            daemon: Arc::new(daemon),
            clock: Arc::new(clock),
            settings: Arc::new(Unkept),
            shown: Vec::new(),
        }
    }

    /// Keeps what the person chooses (the theme) in `settings`, across
    /// launches. Without it, each launch starts on the system's theme.
    #[must_use]
    pub fn keeping(mut self, settings: impl Settings) -> Self {
        self.settings = Arc::new(settings);
        self
    }

    /// Opens with these machines on stage: one in focus, two side by
    /// side, more in a grid. Without any, the first running machine.
    #[must_use]
    pub fn showing(mut self, names: Vec<MachineName>) -> Self {
        self.shown = names;
        self
    }

    /// Opens the window and runs until it's closed. Takes over the main
    /// thread, as macOS requires of a GUI.
    pub fn run(self) {
        app::Window::run(self.daemon, self.clock, self.settings, self.shown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_theme_cycles_and_reads_back_as_kept() {
        let mut choice = ThemeChoice::System;
        let mut seen = Vec::new();
        for _ in 0..3 {
            choice = choice.next();
            seen.push(choice);
            assert_eq!(choice.as_str().parse::<ThemeChoice>().ok(), Some(choice));
        }
        assert_eq!(
            seen,
            [ThemeChoice::Light, ThemeChoice::Dark, ThemeChoice::System]
        );
        assert_eq!(
            "dark\n".parse::<ThemeChoice>().ok(),
            Some(ThemeChoice::Dark)
        );
        assert!("sepia".parse::<ThemeChoice>().is_err());
    }

    #[test]
    fn planes_must_match_the_frame_size() {
        let luma = [0_u8; 8];
        let chroma = [0_u8; 4];
        let frame = FrameRef::new(4, 2, &luma, &chroma).unwrap();
        assert_eq!(frame.rows(Plane::Luma).count(), 2);
        assert_eq!(frame.rows(Plane::Chroma).count(), 1);
        assert_eq!(
            FrameRef::new(3, 2, &luma, &chroma),
            Err(FrameError::Size {
                width: 3,
                height: 2
            })
        );
        assert_eq!(
            FrameRef::new(4, 2, &luma[..7], &chroma),
            Err(FrameError::PlaneLength {
                plane: "luma",
                expected: 8,
                actual: 7
            })
        );
        assert_eq!(
            FrameRef::new(4, 2, &luma, &chroma[..2]),
            Err(FrameError::PlaneLength {
                plane: "chroma",
                expected: 4,
                actual: 2
            })
        );
    }
}
