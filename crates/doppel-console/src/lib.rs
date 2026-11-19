//! The window for running machines, drawn with GPUI: on the desktop, and
//! in a browser as WebAssembly.
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
//! which the shell implements on the desktop, and the console itself in a
//! browser. On the desktop each screen brings a source of its frames, raw
//! or H.264, which the console reads on a thread of its own, decodes, and
//! draws with GPUI's surface element, which takes NV12 directly. In a
//! browser the daemon's H.264 frames are decoded by the browser.

pub mod answers;
mod app;
mod input;
mod route;
mod screen;
mod ui;

// What differs between the desktop and the browser: where frames come
// from and how they're decoded and drawn, how a recording's video plays,
// and the window's colour space. Each target has one module, under one
// name, with the same items.
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "macos")]
pub use macos::ports::{
    EncodedSource, FrameError, FrameRef, FrameSource, InputSink, Next, NextEncoded, Screen,
    Takeover,
};
#[cfg(target_family = "wasm")]
mod web;
#[cfg(target_family = "wasm")]
use web as platform;
#[cfg(target_family = "wasm")]
pub use web::Screen;

pub use doppel_desktop::agent::AgentReport;
pub use doppel_desktop::agent::AgentStatus;
pub use doppel_desktop::ax::tree::AxSnapshot;
pub use doppel_desktop::geometry::Size;
pub use doppel_machine::bundle::CheckpointLabel;
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
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures::Stream;

/// A source's failure, as its screen shows it.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct SourceError(Box<dyn Error + Send + Sync>);

impl SourceError {
    pub fn new(error: impl Error + Send + Sync + 'static) -> Self {
        Self(Box::new(error))
    }
}

/// Where a recording's video plays from: a file on this Mac, for the
/// desktop's player; its address on the gateway, for the browser's.
#[cfg(target_os = "macos")]
pub type VideoAt = std::path::PathBuf;
#[cfg(target_family = "wasm")]
pub type VideoAt = String;

/// The time: milliseconds on a monotonic clock, from an origin of its
/// own, for the timelines, the overlay's motion and replay; and the local
/// date a checkpoint's suggested label carries.
pub trait LocalClock: Send + Sync + 'static {
    fn millis(&self) -> f64;
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

/// What the daemon will answer. The window awaits it on its own thread,
/// which it must never block: a port that blocks does its work elsewhere
/// (the desktop's on threads of its own) and hands the answer back.
pub type Later<T> = Pin<Box<dyn Future<Output = T>>>;

/// An answer from the daemon, or why there is none.
pub type Answer<T> = Later<Result<T, DaemonError>>;

/// What agents do, as it happens, until the daemon closes the stream or it
/// fails; the window opens another a little later.
pub type Activity = Pin<Box<dyn Stream<Item = Result<ActivityEvent, SourceError>>>>;

/// Everything the window asks of the daemon. Each call answers later: the
/// window awaits it without waiting for it.
pub trait Daemon: 'static {
    /// Every machine, with where it is.
    ///
    /// # Errors
    /// [`DaemonError::Unreachable`] shows the window as disconnected.
    fn machines(&self) -> Answer<Vec<Machine>>;

    /// The agents' sessions.
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn sessions(&self) -> Answer<Vec<Session>>;

    /// Holds a session's calls, or lets them go on.
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn hold(&self, session: SessionId, hold: Hold) -> Answer<()>;

    /// A machine's checkpoints that can be used, oldest first.
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn checkpoints(&self, name: &MachineName) -> Answer<Vec<CheckpointLabel>>;

    /// A machine's report: its guest agent's state and its checkpoints.
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn report(&self, name: &MachineName) -> Answer<MachineReport>;

    /// Carries out a lifecycle order on `name`, and says how it went. It
    /// may take minutes (a start waits for the desktop).
    fn run(&self, name: &MachineName, order: &Order) -> Later<Verdict>;

    /// Opens a running machine's screen, with input that waits for
    /// control ([`Screen::with_takeover`]).
    ///
    /// # Errors
    /// See [`DaemonError`]: the machine isn't running.
    fn open_screen(&self, name: &MachineName) -> Answer<Screen>;

    /// Starts watching what agents do.
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn watch(&self) -> Answer<Activity>;

    /// The recorded sessions, newest first.
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn recordings(&self) -> Answer<Vec<RecordingSummary>>;

    /// One recording: its videos and its trace.
    ///
    /// # Errors
    /// See [`DaemonError`]: it doesn't exist, or can't be read.
    fn open_recording(&self, id: &RecordingId) -> Answer<Recording>;

    /// The accessibility snapshot line `line` (from 0) of a recording's
    /// trace kept.
    ///
    /// # Errors
    /// See [`DaemonError`]: that line kept none.
    fn seen_snapshot(&self, id: &RecordingId, line: u32) -> Answer<AxSnapshot>;

    /// A person's look at a machine's elements (its frontmost app's focused
    /// window), which costs the agents driving it nothing.
    ///
    /// # Errors
    /// See [`DaemonError`]: the guest's agent is unreachable, or too old.
    fn observe(&self, name: &MachineName) -> Answer<AxSnapshot>;

    /// A person's own read of a machine's screen text, which costs the
    /// agents driving it nothing (it reads the host's view of the display).
    ///
    /// # Errors
    /// See [`DaemonError`].
    fn observe_text(&self, name: &MachineName) -> Answer<TextRead>;

    /// Where one of a recording's videos is on this Mac, to play.
    ///
    /// # Errors
    /// [`DaemonError::Refused`]: its file isn't here (the daemon is on
    /// another Mac), with why.
    fn video(&self, id: &RecordingId, file: &VideoName) -> Answer<VideoAt>;
}

/// Which theme the window shows: the system's appearance, or light or
/// dark whatever the system says.
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

/// The console window.
pub struct Console {
    daemon: Arc<dyn Daemon>,
    clock: Arc<dyn LocalClock>,
    settings: Arc<dyn Settings>,
    shown: Vec<MachineName>,
}

impl std::fmt::Debug for Console {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Console")
            .field("shown", &self.shown)
            .finish_non_exhaustive()
    }
}

impl Console {
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

    /// Opens the window. On the desktop it runs until it's closed, and
    /// takes over the main thread, as macOS requires of a GUI; in the
    /// browser it returns once the page draws it.
    pub fn run(self) {
        let Self {
            daemon,
            clock,
            settings,
            shown,
        } = self;
        platform::launch(move |cx| app::Root::open_window(daemon, clock, settings, shown, cx));
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
}
