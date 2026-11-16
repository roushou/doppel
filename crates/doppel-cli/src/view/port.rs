//! The viewer's [`Daemon`] port over doppel-client: the machines, sessions
//! and lifecycle the window shows and runs, its screens, and what agents
//! do. The one place that knows both the client's answers and the viewer's
//! words for them, and maps one to the other.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use doppel_client::{
    Activity, ActivityStream, CheckpointResult, Client, ClientError, Listing, MachineFailure,
    SessionInfo, SessionStatus, StopMode, StopOutcome, Window,
};
use doppel_host::capabilities::{Clock, SystemClock, WallClock};
use doppel_machine::checkpoint::AfterCheckpoint;
use doppel_machine::machine::MachineName;
use doppel_machine::outcome::{Certainty, Failure};
use doppel_machine::vm::{RunState, Status};
use doppel_viewer::{
    ActivityEvent, ActivitySource, AxSnapshot, Call, CallKey, Callout, CalloutLasts, CalloutOn,
    CalloutShape, CheckpointLabel, Daemon, DaemonError, Ending, Gesture, Hold, LocalTime, Machine,
    MachineReport, MachineState, NextActivity, Order, ReadLine, RecentEnd, Recording, RecordingId,
    RecordingSummary, Run, Screen, Session, SessionActivity, SessionId, SourceError, Target,
    TextRead, Verdict, VideoName,
};

use super::Frames;
use super::recordings::{self, Videos};
use crate::daemon_at::{LIFECYCLE, within};

/// How long a list or a hold may take.
const CALL: Duration = Duration::from_secs(10);
/// How long the activity stream waits before saying nothing happened.
const QUIET: Duration = Duration::from_secs(1);

/// The daemon on a socket, as the viewer's port.
pub(crate) struct DaemonPort {
    socket: PathBuf,
    frames: Frames,
    videos: Videos,
    /// The connection lists and holds go on; made again once it breaks.
    client: Mutex<Option<Arc<Client<SystemClock>>>>,
}

impl DaemonPort {
    pub(crate) fn new(
        socket: PathBuf,
        frames: Frames,
        videos: Videos,
        client: Client<SystemClock>,
    ) -> Self {
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
                Verdict::done()
            }
            Order::Stop => Verdict::stopped(client.stop(name, StopMode::Graceful, by())?.outcome),
            Order::ForceStop => Verdict::stopped(client.stop(name, StopMode::Force, by())?.outcome),
            Order::Pause => {
                client.pause(name, by())?;
                Verdict::done()
            }
            Order::Resume => {
                client.resume(name, by())?;
                Verdict::done()
            }
            Order::Checkpoint(label) => Verdict::checkpoint(
                client
                    .checkpoint(name, label, AfterCheckpoint::Resume, by())?
                    .result,
            ),
            Order::RollBack(label) => {
                client.rollback(name, label, by())?;
                Verdict::done()
            }
            Order::Restore(label) => {
                client.restore(name, label, by())?;
                Verdict::done()
            }
            Order::Fork { label, into } => {
                client.fork(name, label, into, by())?;
                Verdict::Done {
                    note: None,
                    made: Some(into.clone()),
                }
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
                    Verdict::Done {
                        note: None,
                        made: forks.into_iter().next().map(|fork| fork.name),
                    }
                } else {
                    Verdict::Refused(failed.join("; "))
                }
            }
            Order::Delete => {
                client.delete(name, by())?;
                Verdict::done()
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

/// How a lifecycle order ended, from the daemon's answers.
trait Verdicts {
    fn done() -> Self;
    fn stopped(outcome: StopOutcome) -> Self;
    fn checkpoint(result: CheckpointResult) -> Self;
    fn failed(error: &ClientError) -> Self;
}

impl Verdicts for Verdict {
    fn done() -> Self {
        Self::Done {
            note: None,
            made: None,
        }
    }

    fn stopped(outcome: StopOutcome) -> Self {
        match outcome {
            StopOutcome::Stopped => Self::done(),
            StopOutcome::ShutdownPending => Self::Done {
                note: Some("asked to shut down; it hasn't yet".into()),
                made: None,
            },
        }
    }

    fn checkpoint(result: CheckpointResult) -> Self {
        match result {
            CheckpointResult::Completed {
                left_paused: Some(failure),
                ..
            } => Self::Done {
                note: Some(format!("saved, but it may be left paused: {failure}")),
                made: None,
            },
            CheckpointResult::Completed {
                left_paused: None, ..
            } => Self::done(),
            CheckpointResult::Failed { error, .. } => Self::Refused(error.to_string()),
            CheckpointResult::Unknown { error, .. } => Self::Unknown(error.to_string()),
            CheckpointResult::InProgress { .. } => {
                Self::Unknown("the checkpoint ended before it finished".into())
            }
        }
    }

    /// A refusal when nothing was done; unknown when it may have been: the
    /// connection broke after the order went out, or the answer can't be read.
    fn failed(error: &ClientError) -> Self {
        let words = error.to_string();
        match error {
            ClientError::Machine(failure) => match failure.certainty() {
                Certainty::NotDone => Self::Refused(words),
                Certainty::MaybeDone => Self::Unknown(words),
            },
            ClientError::NoDaemon(_)
            | ClientError::Connect(_)
            | ClientError::VersionMismatch { .. }
            | ClientError::Encode(_)
            | ClientError::Remote(_) => Self::Refused(words),
            ClientError::Io(_)
            | ClientError::Closed
            | ClientError::Lost(_)
            | ClientError::Timeout
            | ClientError::Stream(_)
            | ClientError::Protocol(_)
            | ClientError::WrongAnswer(_)
            | ClientError::Parse(_) => Self::Unknown(words),
        }
    }
}

/// The viewer's words for the client's answers.
fn machine(listing: Listing) -> Option<Machine> {
    match listing {
        Listing::Machine {
            name,
            status,
            lineage,
        } => Some(Machine {
            name,
            state: match status {
                Ok(Status::Stopped(_)) => MachineState::Stopped,
                Ok(Status::Live(state)) => MachineState::Live(run(state)),
                Err(failure) => MachineState::Unreadable(MachineFailure::message(&failure).into()),
            },
            lineage,
        }),
        // Not a machine's name: the daemon ignores it too.
        Listing::Unrecognized(_) => None,
    }
}

const fn run(state: RunState) -> Run {
    match state {
        RunState::Starting => Run::Starting,
        RunState::Running => Run::Running,
        RunState::Paused => Run::Paused,
        RunState::Pausing | RunState::Resuming | RunState::Saving | RunState::Restoring => {
            Run::Busy
        }
        RunState::Stopping | RunState::Stopped => Run::Stopping,
        RunState::Error => Run::Error,
        RunState::Unrecognized(_) => Run::Unknown,
    }
}

fn session(info: SessionInfo) -> Session {
    Session {
        id: SessionId(info.id),
        client: info.client,
        machines: info.machines,
        running: info.running,
        answered: info.answered,
        activity: match info.state {
            SessionStatus::Active => SessionActivity::Active,
            SessionStatus::Paused => SessionActivity::Paused,
            SessionStatus::Unknown(raw) => SessionActivity::Unknown(raw),
        },
    }
}

pub(super) fn activity(activity: Activity) -> ActivityEvent {
    let key = CallKey {
        session: activity.session,
        request: activity.request,
    };
    match activity.phase {
        doppel_client::Phase::Started(acted) => {
            let why = acted.why.clone();
            let (gesture, target, callout) = on_screen(acted);
            ActivityEvent::Started(Call {
                key,
                machine: activity.machine,
                client: activity.client,
                caption: activity.caption,
                gesture,
                target,
                callout,
                why,
            })
        }
        doppel_client::Phase::Ended(ended) => ActivityEvent::Ended {
            key,
            machine: activity.machine,
            ending: ending_of(ended.ending),
            seen: ended.seen.map(|seen| Arc::new(*seen)),
            read: ended.read.map(|read| Arc::new(text_read(&read))),
        },
        doppel_client::Phase::Named(refs) => ActivityEvent::Named {
            machine: activity.machine,
            refs: Arc::new(refs),
        },
        doppel_client::Phase::Recent {
            acted,
            started_ago,
            ended,
        } => ActivityEvent::Recent {
            call: {
                let why = acted.why.clone();
                let (gesture, target, callout) = on_screen(acted);
                Call {
                    key,
                    machine: activity.machine,
                    client: activity.client,
                    caption: activity.caption,
                    gesture,
                    target,
                    callout,
                    why,
                }
            },
            started_ms_ago: started_ago.as_secs_f64() * 1000.0,
            ended: ended.map(|(ended, ago)| {
                Box::new(RecentEnd {
                    ending: ending_of(ended.ending),
                    seen: ended.seen.map(|seen| Arc::new(*seen)),
                    refs: ended.refs.map(Arc::new),
                    read: ended.read.map(|read| Arc::new(text_read(&read))),
                    ms_ago: ago.as_secs_f64() * 1000.0,
                })
            }),
        },
    }
}

/// What a call read, in the viewer's words: lines and matches in display points.
pub(super) fn text_read(read: &doppel_client::TextRead) -> TextRead {
    TextRead {
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

fn ending_of(ending: doppel_client::Ending) -> Ending {
    match ending {
        // An ending this build can't read still ends the call.
        doppel_client::Ending::Done | doppel_client::Ending::Unknown => Ending::Done,
        doppel_client::Ending::Failed(why) | doppel_client::Ending::Refused(why) => {
            Ending::Failed(why)
        }
        doppel_client::Ending::TimedOut(why) if why.is_empty() => {
            Ending::Failed("timed out".into())
        }
        doppel_client::Ending::TimedOut(why) => Ending::Failed(format!("timed out: {why}")),
    }
}

/// What a call did on the screen, in the viewer's words. A mark's place is
/// its own, not where the agent's pointer goes: its call has no target.
pub(super) fn on_screen(acted: doppel_client::OnScreen) -> (Gesture, Target, Option<Arc<Callout>>) {
    let gesture = gesture_of(acted.gesture);
    match acted.callout {
        None => (gesture, target_of(acted.target), None),
        Some(callout) => {
            let on = match target_of(acted.target) {
                Target::Point(at) => Some(CalloutOn::Point(at)),
                Target::Element { frame, label } => Some(CalloutOn::Area { frame, label }),
                Target::Drag { .. } | Target::Nowhere => None,
            };
            let callout = on.map(|on| {
                Arc::new(Callout {
                    on,
                    shape: match callout.shape {
                        doppel_client::CalloutShape::Dot => CalloutShape::Dot,
                        doppel_client::CalloutShape::Arrow { from } => CalloutShape::Arrow { from },
                        // A shape a newer daemon added is drawn as a box.
                        doppel_client::CalloutShape::Box
                        | doppel_client::CalloutShape::Unknown(_) => CalloutShape::Box,
                    },
                    note: callout.note,
                    lasts: match callout.lasts {
                        doppel_client::CalloutLasts::For(time) => {
                            CalloutLasts::For(time.as_secs_f64() * 1000.0)
                        }
                        doppel_client::CalloutLasts::UntilActed(time) => {
                            CalloutLasts::UntilActed(time.as_secs_f64() * 1000.0)
                        }
                    },
                })
            });
            (gesture, Target::Nowhere, callout)
        }
    }
}

/// The viewer's word for what a call did on the screen.
pub(super) fn gesture_of(gesture: doppel_client::Gesture) -> Gesture {
    match gesture {
        doppel_client::Gesture::Click => Gesture::Click,
        doppel_client::Gesture::DoubleClick => Gesture::DoubleClick,
        doppel_client::Gesture::SecondaryClick => Gesture::SecondaryClick,
        doppel_client::Gesture::Move => Gesture::Move,
        doppel_client::Gesture::Drag => Gesture::Drag,
        doppel_client::Gesture::Scroll => Gesture::Scroll,
        doppel_client::Gesture::Key => Gesture::Key,
        doppel_client::Gesture::Type => Gesture::Type,
        doppel_client::Gesture::Act => Gesture::Act,
        doppel_client::Gesture::Look => Gesture::Look,
        doppel_client::Gesture::Wait => Gesture::Wait,
        doppel_client::Gesture::Run => Gesture::Run,
        doppel_client::Gesture::Lifecycle => Gesture::Lifecycle,
        doppel_client::Gesture::Checkpoint => Gesture::Checkpoint,
        doppel_client::Gesture::Rollback => Gesture::Rollback,
        doppel_client::Gesture::Mark => Gesture::Mark,
        // A gesture a newer daemon added is drawn as any other call.
        doppel_client::Gesture::Other | doppel_client::Gesture::Unknown(_) => Gesture::Other,
    }
}

/// The viewer's word for where a call acted.
pub(super) fn target_of(target: Option<doppel_client::Target>) -> Target {
    match target {
        Some(doppel_client::Target::Point(at)) => Target::Point(at),
        Some(doppel_client::Target::Element { frame, label }) => Target::Element { frame, label },
        Some(doppel_client::Target::Drag { from, to }) => Target::Drag { from, to },
        None => Target::Nowhere,
    }
}

impl Daemon for DaemonPort {
    fn machines(&self) -> Result<Vec<Machine>, DaemonError> {
        let listings = self.call(|client| client.machines(within(CALL)))?;
        Ok(listings.into_iter().filter_map(machine).collect())
    }

    fn sessions(&self) -> Result<Vec<Session>, DaemonError> {
        let sessions = self.call(|client| client.sessions(within(CALL)))?;
        Ok(sessions.into_iter().map(session).collect())
    }

    fn hold(&self, session: SessionId, hold: Hold) -> Result<(), DaemonError> {
        self.call(|client| match hold {
            Hold::Pause => client.pause_session(session.0, within(CALL)),
            Hold::Resume => client.resume_session(session.0, within(CALL)),
        })
    }

    fn checkpoints(&self, name: &MachineName) -> Result<Vec<CheckpointLabel>, DaemonError> {
        Ok(self
            .call(|client| client.report(name, within(CALL)))?
            .checkpoints)
    }

    fn report(&self, name: &MachineName) -> Result<MachineReport, DaemonError> {
        self.call(|client| client.report(name, within(CALL)))
    }

    fn run(&self, name: &MachineName, order: &Order) -> Verdict {
        self.order(name, order)
            .unwrap_or_else(|error| Verdict::failed(&error))
    }

    fn open_screen(&self, name: &MachineName) -> Result<Screen, DaemonError> {
        self.frames
            .screen(&self.socket, name)
            .map_err(|error| DaemonError::Refused(error.to_string()))
    }

    fn watch(&self) -> Result<Box<dyn ActivitySource>, DaemonError> {
        ActivityStream::open(&self.socket, SystemClock, within(CALL))
            .map(|stream| Box::new(Watching(stream)) as Box<dyn ActivitySource>)
            .map_err(|error| DaemonError::Unreachable(error.to_string()))
    }

    fn recordings(&self) -> Result<Vec<RecordingSummary>, DaemonError> {
        let listed = self.call(|client| client.recordings(within(CALL)))?;
        Ok(listed.into_iter().map(recordings::summary).collect())
    }

    fn open_recording(&self, id: &RecordingId) -> Result<Recording, DaemonError> {
        // The viewer's name, parsed again on the way in.
        let id: doppel_client::RecordingId =
            id.as_str()
                .parse()
                .map_err(|error: doppel_client::NotARecording| {
                    DaemonError::Refused(error.to_string())
                })?;
        let opened = self.call(|client| client.open_recording(&id, within(CALL)))?;
        Ok(recordings::recording(opened))
    }

    fn seen_snapshot(&self, id: &RecordingId, line: u32) -> Result<AxSnapshot, DaemonError> {
        let id: doppel_client::RecordingId =
            id.as_str()
                .parse()
                .map_err(|error: doppel_client::NotARecording| {
                    DaemonError::Refused(error.to_string())
                })?;
        self.call(|client| client.seen_snapshot(&id, line, within(CALL)))
    }

    fn observe_text(&self, name: &MachineName) -> Result<TextRead, DaemonError> {
        let text = self.call(|client| client.observe_text(name, within(CALL)))?;
        Ok(text_read(&doppel_client::TextRead {
            text,
            query: None,
            found: Vec::new(),
        }))
    }

    fn observe(&self, name: &MachineName) -> Result<AxSnapshot, DaemonError> {
        self.call(|client| client.observe(name, within(CALL)))
    }

    fn video(&self, id: &RecordingId, file: &VideoName) -> Result<PathBuf, DaemonError> {
        self.videos.path(
            || self.call(|client| client.data_root(within(CALL))),
            id,
            file,
        )
    }
}

/// The daemon's activity stream, as the viewer's source.
struct Watching(ActivityStream<SystemClock>);

impl ActivitySource for Watching {
    fn next(&mut self) -> Result<NextActivity, SourceError> {
        match self.0.next(within(QUIET)) {
            Ok(event) => Ok(NextActivity::Event(activity(event))),
            Err(ClientError::Timeout) => Ok(NextActivity::Unchanged),
            Err(ClientError::Closed) => Ok(NextActivity::Ended),
            Err(error) => Err(SourceError::new(error)),
        }
    }
}

/// The time, for the viewer: monotonic, and local to name checkpoints by.
pub(crate) struct ViewerClock;

impl Clock for ViewerClock {
    fn now(&self) -> Instant {
        SystemClock.now()
    }
}

impl doppel_viewer::LocalClock for ViewerClock {
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
    use doppel_machine::geometry::Point;
    use doppel_machine::lineage::{Lineage, RecordedLineage};

    #[test]
    fn listings_become_machines_and_strays_are_left_out() {
        let dev: MachineName = "dev".parse().unwrap();
        assert_eq!(
            machine(Listing::Machine {
                name: dev.clone(),
                status: Ok(Status::Live(RunState::Saving)),
                lineage: RecordedLineage::Unreadable("bad".into()),
            }),
            Some(Machine {
                name: dev.clone(),
                state: MachineState::Live(Run::Busy),
                lineage: RecordedLineage::Unreadable("bad".into()),
            })
        );
        assert_eq!(
            machine(Listing::Machine {
                name: dev.clone(),
                status: Err(MachineFailure::new("no spec".into(), Certainty::NotDone)),
                lineage: RecordedLineage::Known(Lineage::default()),
            })
            .map(|machine| machine.state),
            Some(MachineState::Unreadable("no spec".into()))
        );
        assert_eq!(machine(Listing::Unrecognized("Not A Name".into())), None);
    }

    #[test]
    fn a_failure_reads_as_refused_only_when_nothing_was_done() {
        let refused = ClientError::Machine(MachineFailure::new("busy".into(), Certainty::NotDone));
        assert_eq!(Verdict::failed(&refused), Verdict::Refused("busy".into()));
        let maybe = ClientError::Machine(MachineFailure::new("lost".into(), Certainty::MaybeDone));
        assert_eq!(Verdict::failed(&maybe), Verdict::Unknown("lost".into()));
        assert!(matches!(
            Verdict::failed(&ClientError::Closed),
            Verdict::Unknown(_)
        ));
        assert_eq!(Reach::of(&ClientError::Closed), Reach::Lost);
        assert_eq!(Reach::of(&ClientError::Protocol("x")), Reach::Answered);
    }

    #[test]
    fn a_shutdown_still_pending_is_done_with_a_note() {
        assert_eq!(
            Verdict::stopped(StopOutcome::ShutdownPending),
            Verdict::Done {
                note: Some("asked to shut down; it hasn't yet".into()),
                made: None,
            }
        );
        assert_eq!(
            Verdict::checkpoint(CheckpointResult::InProgress {
                awaiting: doppel_machine::checkpoint::Step::SyncingGuest,
            }),
            Verdict::Unknown("the checkpoint ended before it finished".into())
        );
    }

    #[test]
    fn activity_keeps_its_call_and_ending() {
        let started = activity(Activity {
            session: 2,
            request: 5,
            machine: "dev".parse().unwrap(),
            client: "claude".into(),
            caption: "click OK".into(),
            phase: doppel_client::Phase::Started(doppel_client::OnScreen {
                why: None,
                gesture: doppel_client::Gesture::Unknown(77),
                target: Some(doppel_client::Target::Point(Point::new(1.0, 2.0).unwrap())),
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
        let ended = activity(Activity {
            session: 2,
            request: 5,
            machine: "dev".parse().unwrap(),
            client: String::new(),
            caption: String::new(),
            phase: doppel_client::Phase::Ended(doppel_client::Ended {
                ending: doppel_client::Ending::TimedOut(String::new()),
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

    #[test]
    fn local_time_reads_a_date() {
        let at = local(SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000));
        // 2027-01-15 08:00 UTC: the same day or the next, in any zone.
        assert_eq!((at.year, at.month), (2027, 1));
        assert!((14..=16).contains(&at.day));
    }
}

/// What the person chose in the viewer, kept in a file of the store's.
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

impl doppel_viewer::Settings for KeptSettings {
    fn theme(&self) -> doppel_viewer::ThemeChoice {
        // None kept, or one unreadable: the system's.
        std::fs::read_to_string(&self.theme)
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or_default()
    }

    fn keep_theme(
        &self,
        choice: doppel_viewer::ThemeChoice,
    ) -> Result<(), doppel_viewer::SettingsError> {
        std::fs::write(&self.theme, choice.as_str()).map_err(doppel_viewer::SettingsError::new)
    }
}
