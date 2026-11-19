//! The console's [`Daemon`] port over doppel-client: the machines, sessions
//! and lifecycle the window shows and runs, its screens, and what agents
//! do. The one place that knows both the client's answers and the console's
//! words for them, and maps one to the other.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use futures::channel::{mpsc, oneshot};

use doppel_client::{ActivityStream, Client, ClientError, Listing, StopMode, Window};
use doppel_console::{
    Activity, ActivityEvent, Answer, AxSnapshot, CheckpointLabel, Daemon, DaemonError, Hold, Later,
    LocalTime, Machine, MachineReport, Order, Recording, RecordingId, RecordingSummary, Screen,
    Session, SessionId, SourceError, TextRead, Verdict, VideoFile, answers,
};
use doppel_desktop::outcome::{Certainty, Failure};
use doppel_host::capabilities::{Clock, SystemClock, WallClock};
use doppel_machine::checkpoint::AfterCheckpoint;
use doppel_machine::machine::MachineName;

use super::Frames;
use super::recordings::{self, Videos};
use crate::daemon_at::{LIFECYCLE, within};

/// How long a list or a hold may take.
const CALL: Duration = Duration::from_secs(10);
/// How long the activity stream waits before saying nothing happened.
const QUIET: Duration = Duration::from_secs(1);

/// The daemon on a socket, as the console's port: each call made on a
/// thread of its own, since the client's calls block and the window's
/// thread must not.
pub(crate) struct DaemonPort(Arc<Blocking>);

impl DaemonPort {
    pub(crate) fn new(
        socket: PathBuf,
        frames: Frames,
        videos: Videos,
        client: Client<SystemClock>,
    ) -> Self {
        Self(Arc::new(Blocking::new(socket, frames, videos, client)))
    }

    /// `work` on a thread of its own, answered later. No thread: the
    /// answer is lost, as a broken connection's is.
    fn later<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Blocking) -> Result<T, DaemonError> + Send + 'static,
    ) -> Answer<T> {
        let port = Arc::clone(&self.0);
        let answered = off_thread(move || work(&port));
        Box::pin(async move {
            answered
                .await
                .unwrap_or_else(|_| Err(DaemonError::Unreachable("no answer".into())))
        })
    }
}

/// Runs `work` on a thread of its own; its answer arrives on the receiver.
fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> oneshot::Receiver<T> {
    let (answer, answered) = oneshot::channel();
    // No thread: the receiver sees its sender dropped.
    let _ = std::thread::Builder::new()
        .name("doppel-console-daemon".into())
        .spawn(move || {
            let _ = answer.send(work());
        });
    answered
}

/// The daemon on a socket, called and waited for.
pub(crate) struct Blocking {
    socket: PathBuf,
    frames: Frames,
    videos: Videos,
    /// The connection lists and holds go on; made again once it breaks.
    client: Mutex<Option<Arc<Client<SystemClock>>>>,
}

impl Blocking {
    fn new(socket: PathBuf, frames: Frames, videos: Videos, client: Client<SystemClock>) -> Self {
        Self {
            socket,
            frames,
            videos,
            client: Mutex::new(Some(Arc::new(client))),
        }
    }

    fn connect(&self) -> Result<Client<SystemClock>, ClientError> {
        Client::connect(&self.socket, SystemClock, within(CALL))
    }

    /// The shared connection, made again if the last one broke.
    fn client(&self) -> Result<Arc<Client<SystemClock>>, DaemonError> {
        let mut held = self.client.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(client) = held.as_ref() {
            return Ok(Arc::clone(client));
        }
        let client = Arc::new(self.connect().map_err(|error| self.failed(error))?);
        *held = Some(Arc::clone(&client));
        Ok(client)
    }

    /// A failed call, for the window: a broken connection is dropped, so
    /// the next call makes another.
    fn failed(&self, error: ClientError) -> DaemonError {
        match Reach::of(&error) {
            Reach::Lost => {
                *self.client.lock().unwrap_or_else(PoisonError::into_inner) = None;
                DaemonError::Unreachable(error.to_string())
            }
            Reach::Answered => DaemonError::Refused(error.to_string()),
        }
    }

    fn call<T>(
        &self,
        call: impl FnOnce(&Client<SystemClock>) -> Result<T, ClientError>,
    ) -> Result<T, DaemonError> {
        let client = self.client()?;
        call(&client).map_err(|error| self.failed(error))
    }

    /// Runs an order on a connection of its own: it may take minutes, and
    /// waits its machine's turn.
    fn order(&self, name: &MachineName, order: &Order) -> Result<Verdict, ClientError> {
        let client = self.connect()?;
        let by = || within(LIFECYCLE);
        Ok(match order {
            Order::Start => {
                client.start(name, Window::Hidden, by())?;
                answers::done()
            }
            Order::Stop => answers::stopped(client.stop(name, StopMode::Graceful, by())?.outcome),
            Order::ForceStop => answers::stopped(client.stop(name, StopMode::Force, by())?.outcome),
            Order::Pause => {
                client.pause(name, by())?;
                answers::done()
            }
            Order::Resume => {
                client.resume(name, by())?;
                answers::done()
            }
            Order::Checkpoint(label) => answers::checkpoint(
                client
                    .checkpoint(name, label, AfterCheckpoint::Resume, by())?
                    .result,
            ),
            Order::RollBack(label) => {
                client.rollback(name, label, by())?;
                answers::done()
            }
            Order::Restore(label) => {
                client.restore(name, label, by())?;
                answers::done()
            }
            Order::Fork { label, into } => {
                client.fork(name, label, into, by())?;
                answers::done()
            }
            Order::NewCopy(label) => {
                let forks = client.run_forks(name, label, 1, by())?;
                let failed: Vec<String> = forks
                    .iter()
                    .filter_map(|fork| {
                        fork.desktop
                            .as_ref()
                            .err()
                            .map(|failure| format!("{}: {failure}", fork.name))
                    })
                    .collect();
                if failed.is_empty() {
                    answers::done()
                } else {
                    Verdict::Refused(failed.join("; "))
                }
            }
            Order::Delete => {
                client.delete(name, by())?;
                answers::done()
            }
        })
    }
}

/// Whether a failed call reached the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// The connection is gone, or never was.
    Lost,
    /// The daemon answered: a refusal, or an answer this build can't read.
    Answered,
}

impl Reach {
    const fn of(error: &ClientError) -> Self {
        match error {
            ClientError::NoDaemon(_)
            | ClientError::Connect(_)
            | ClientError::VersionMismatch { .. }
            | ClientError::Io(_)
            | ClientError::Closed
            | ClientError::Lost(_)
            | ClientError::Timeout
            | ClientError::Stream(_) => Self::Lost,
            ClientError::Encode(_)
            | ClientError::Protocol(_)
            | ClientError::WrongAnswer(_)
            | ClientError::Parse(_)
            | ClientError::Remote(_)
            | ClientError::Machine(_) => Self::Answered,
        }
    }
}

/// How an order the client couldn't carry out ended: refused when nothing
/// was done; unknown when it may have been (the connection broke after the
/// order went out, or the answer can't be read).
fn failed(error: &ClientError) -> Verdict {
    if let ClientError::Machine(failure) = error {
        return answers::failed(failure);
    }
    let words = error.to_string();
    match error.certainty() {
        Certainty::NotDone => Verdict::Refused(words),
        Certainty::MaybeDone => Verdict::Unknown(words),
    }
}

/// A listed machine, in the console's words; a stray entry is left out.
fn machine(listing: Listing) -> Option<Machine> {
    match listing {
        Listing::Machine {
            name,
            status,
            lineage,
        } => Some(answers::machine(name, status, lineage)),
        // Not a machine's name: the daemon ignores it too.
        Listing::Unrecognized(_) => None,
    }
}

impl Blocking {
    fn machines(&self) -> Result<Vec<Machine>, DaemonError> {
        let listings = self.call(|client| client.machines(within(CALL)))?;
        Ok(listings.into_iter().filter_map(machine).collect())
    }

    fn sessions(&self) -> Result<Vec<Session>, DaemonError> {
        let sessions = self.call(|client| client.sessions(within(CALL)))?;
        Ok(sessions.into_iter().map(answers::session).collect())
    }

    fn hold(&self, session: SessionId, hold: Hold) -> Result<(), DaemonError> {
        self.call(|client| match hold {
            Hold::Pause => client.pause_session(session, within(CALL)),
            Hold::Resume => client.resume_session(session, within(CALL)),
        })
    }

    fn checkpoints(&self, name: &MachineName) -> Result<Vec<CheckpointLabel>, DaemonError> {
        Ok(answers::usable_checkpoints(
            self.call(|client| client.report(name, within(CALL)))?,
        ))
    }

    fn report(&self, name: &MachineName) -> Result<MachineReport, DaemonError> {
        self.call(|client| client.report(name, within(CALL)))
    }

    fn run(&self, name: &MachineName, order: &Order) -> Verdict {
        self.order(name, order)
            .unwrap_or_else(|error| failed(&error))
    }

    fn open_screen(&self, name: &MachineName) -> Result<Screen, DaemonError> {
        self.frames
            .screen(&self.socket, name)
            .map_err(|error| DaemonError::Refused(error.to_string()))
    }

    fn watch(&self) -> Result<ActivityStream<SystemClock>, DaemonError> {
        ActivityStream::open(&self.socket, SystemClock, within(CALL))
            .map_err(|error| DaemonError::Unreachable(error.to_string()))
    }

    fn recordings(&self) -> Result<Vec<RecordingSummary>, DaemonError> {
        let listed = self.call(|client| client.recordings(within(CALL)))?;
        Ok(listed.into_iter().map(recordings::summary).collect())
    }

    fn open_recording(&self, id: &RecordingId) -> Result<Recording, DaemonError> {
        let opened = self.call(|client| client.open_recording(id, within(CALL)))?;
        Ok(Recording::of(opened))
    }

    fn seen_snapshot(&self, id: &RecordingId, line: u32) -> Result<AxSnapshot, DaemonError> {
        self.call(|client| client.seen_snapshot(id, line, within(CALL)))
    }

    fn observe_text(&self, name: &MachineName) -> Result<TextRead, DaemonError> {
        let text = self.call(|client| client.observe_text(name, within(CALL)))?;
        Ok(TextRead::of(&doppel_client::TextRead {
            text,
            query: None,
            found: Vec::new(),
        }))
    }

    fn observe(&self, name: &MachineName) -> Result<AxSnapshot, DaemonError> {
        self.call(|client| client.observe(name, within(CALL)))
    }

    fn video(&self, id: &RecordingId, file: &VideoFile) -> Result<PathBuf, DaemonError> {
        self.videos.path(
            || self.call(|client| client.data_root(within(CALL))),
            id,
            file,
        )
    }
}

impl Daemon for DaemonPort {
    fn machines(&self) -> Answer<Vec<Machine>> {
        self.later(Blocking::machines)
    }

    fn sessions(&self) -> Answer<Vec<Session>> {
        self.later(Blocking::sessions)
    }

    fn hold(&self, session: SessionId, hold: Hold) -> Answer<()> {
        self.later(move |port| port.hold(session, hold))
    }

    fn checkpoints(&self, name: &MachineName) -> Answer<Vec<CheckpointLabel>> {
        let name = name.clone();
        self.later(move |port| port.checkpoints(&name))
    }

    fn report(&self, name: &MachineName) -> Answer<MachineReport> {
        let name = name.clone();
        self.later(move |port| port.report(&name))
    }

    fn run(&self, name: &MachineName, order: &Order) -> Later<Verdict> {
        let port = Arc::clone(&self.0);
        let (name, order) = (name.clone(), order.clone());
        let answered = off_thread(move || port.run(&name, &order));
        Box::pin(async move {
            answered
                .await
                .unwrap_or_else(|_| Verdict::Unknown("the daemon's answer was lost".into()))
        })
    }

    fn open_screen(&self, name: &MachineName) -> Answer<Screen> {
        let name = name.clone();
        self.later(move |port| port.open_screen(&name))
    }

    fn watch(&self) -> Answer<Activity> {
        let opened = self.later(Blocking::watch);
        Box::pin(async move {
            let stream = opened.await?;
            Ok(watching(stream))
        })
    }

    fn recordings(&self) -> Answer<Vec<RecordingSummary>> {
        self.later(Blocking::recordings)
    }

    fn open_recording(&self, id: &RecordingId) -> Answer<Recording> {
        let id = *id;
        self.later(move |port| port.open_recording(&id))
    }

    fn seen_snapshot(&self, id: &RecordingId, line: u32) -> Answer<AxSnapshot> {
        let id = *id;
        self.later(move |port| port.seen_snapshot(&id, line))
    }

    fn observe(&self, name: &MachineName) -> Answer<AxSnapshot> {
        let name = name.clone();
        self.later(move |port| port.observe(&name))
    }

    fn observe_text(&self, name: &MachineName) -> Answer<TextRead> {
        let name = name.clone();
        self.later(move |port| port.observe_text(&name))
    }

    fn video(&self, id: &RecordingId, file: &VideoFile) -> Answer<PathBuf> {
        let (id, file) = (*id, file.clone());
        self.later(move |port| port.video(&id, &file))
    }
}

/// The daemon's activity stream, read on a thread of its own until it
/// ends or fails, or the window stops listening.
fn watching(mut stream: ActivityStream<SystemClock>) -> Activity {
    let (events, arrived) = mpsc::unbounded();
    let _ = std::thread::Builder::new()
        .name("doppel-console-activity".into())
        .spawn(move || {
            while !events.is_closed() {
                let event = match stream.next(within(QUIET)) {
                    Ok(event) => Ok(ActivityEvent::of(event)),
                    Err(ClientError::Timeout) => continue,
                    Err(ClientError::Closed) => return,
                    Err(error) => Err(SourceError::new(error)),
                };
                let failed = event.is_err();
                if events.unbounded_send(event).is_err() || failed {
                    return;
                }
            }
        });
    Box::pin(arrived)
}

/// The time, for the console: monotonic, from when it was made, and local
/// to name checkpoints by.
pub(crate) struct ConsoleClock {
    origin: Instant,
}

impl ConsoleClock {
    pub(crate) fn new() -> Self {
        Self {
            origin: SystemClock.now(),
        }
    }
}

impl doppel_console::LocalClock for ConsoleClock {
    fn millis(&self) -> f64 {
        SystemClock
            .now()
            .saturating_duration_since(self.origin)
            .as_secs_f64()
            * 1000.0
    }

    fn local(&self) -> LocalTime {
        local(SystemClock.wall())
    }
}

/// `at` in this Mac's time zone; the epoch's date if it can't be read.
pub(super) fn local(at: SystemTime) -> LocalTime {
    let seconds = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let seconds = nix::libc::time_t::try_from(seconds).unwrap_or(0);
    let mut parts = std::mem::MaybeUninit::<nix::libc::tm>::zeroed();
    #[expect(
        unsafe_code,
        reason = "reading the local time zone, which only libc knows"
    )]
    // SAFETY: `localtime_r` reads `seconds` and writes one `tm` into
    // `parts`, which is valid for writes; it returns null on failure, and
    // a zeroed `tm` is a valid value either way.
    let parts = unsafe {
        nix::libc::localtime_r(&seconds, parts.as_mut_ptr());
        parts.assume_init()
    };
    let small = |value: i32| u8::try_from(value).unwrap_or(0);
    LocalTime {
        year: parts.tm_year + 1900,
        month: small(parts.tm_mon + 1),
        day: small(parts.tm_mday),
        hour: small(parts.tm_hour),
        minute: small(parts.tm_min),
        second: small(parts.tm_sec),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doppel_client::MachineFailure;
    use doppel_desktop::outcome::Certainty;

    #[test]
    fn strays_are_left_out() {
        assert_eq!(machine(Listing::Unrecognized("Not A Name".into())), None);
    }

    #[test]
    fn a_failure_reads_as_refused_only_when_nothing_was_done() {
        let refused = ClientError::Machine(MachineFailure::new("busy".into(), Certainty::NotDone));
        assert_eq!(failed(&refused), Verdict::Refused("busy".into()));
        let maybe = ClientError::Machine(MachineFailure::new("lost".into(), Certainty::MaybeDone));
        assert_eq!(failed(&maybe), Verdict::Unknown("lost".into()));
        assert!(matches!(failed(&ClientError::Closed), Verdict::Unknown(_)));
        assert_eq!(Reach::of(&ClientError::Closed), Reach::Lost);
        assert_eq!(Reach::of(&ClientError::Protocol("x")), Reach::Answered);
    }

    #[test]
    fn local_time_reads_a_date() {
        let at = local(SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000));
        // 2027-01-15 08:00 UTC: the same day or the next, in any zone.
        assert_eq!((at.year, at.month), (2027, 1));
        assert!((14..=16).contains(&at.day));
    }
}

/// What the person chose in the console, kept in a file of the store's.
#[derive(Debug)]
pub(crate) struct KeptSettings {
    /// The theme chosen: `system`, `light` or `dark`.
    theme: PathBuf,
}

impl KeptSettings {
    /// Settings kept in `home`, the store's folder.
    pub(crate) fn in_home(home: &std::path::Path) -> Self {
        Self {
            theme: home.join("viewer-theme"),
        }
    }
}

impl doppel_console::Settings for KeptSettings {
    fn theme(&self) -> doppel_console::ThemeChoice {
        // None kept, or one unreadable: the system's.
        std::fs::read_to_string(&self.theme)
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or_default()
    }

    fn keep_theme(
        &self,
        choice: doppel_console::ThemeChoice,
    ) -> Result<(), doppel_console::SettingsError> {
        std::fs::write(&self.theme, choice.as_str()).map_err(doppel_console::SettingsError::new)
    }
}
