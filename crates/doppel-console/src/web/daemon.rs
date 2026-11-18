//! The console's [`Daemon`] port in the browser: every call over one
//! WebSocket to the gateway, made again once it breaks. The daemon's
//! answers are read with the protocol's conversions and mapped as the
//! desktop's port maps them (`crate::answers`).

use std::cell::RefCell;
use std::rc::Rc;

use doppel_desktop::ax::tree::AxSnapshot;
use doppel_desktop::geometry::Size;
use doppel_desktop::screen_text::ScreenText;
use doppel_machine::activity as told;
use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::checkpoint::AfterCheckpoint;
use doppel_machine::machine::MachineName;
use doppel_machine::recording as kept;
use doppel_machine::vm::{MachineReport, Status};
use doppel_protocol::convert::ParseError;
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_protocol::pb::vmm::v1 as vmm;
use doppel_protocol::report::{CheckpointReport, MachineFailure, SessionInfo, StopReport};
use futures::StreamExt as _;
use futures::future::{FutureExt as _, LocalBoxFuture, Shared};

use super::connection::{Connection, Refusal};
use super::feed::Screen;
use super::local;
use crate::{
    Activity, ActivityEvent, Answer, Daemon, DaemonError, Hold, Later, Machine, Order, Recording,
    RecordingId, RecordingSummary, Session, SessionId, SourceError, TextRead, Verdict, VideoAt,
    VideoName, answers,
};

type Opening = Shared<LocalBoxFuture<'static, Result<Rc<Connection>, String>>>;

/// The gateway's daemon, reached with the page's token.
pub(crate) struct WebDaemon(Rc<Reach>);

struct Reach {
    url: String,
    token: String,
    /// The connection calls go on, or the one being opened.
    current: RefCell<Option<Opening>>,
}

impl Reach {
    /// The open connection, or a new one once the last broke.
    async fn connection(&self) -> Result<Rc<Connection>, Refusal> {
        let opening = {
            let mut current = self.current.borrow_mut();
            let stale = current
                .as_ref()
                .is_some_and(|opening| match opening.peek() {
                    Some(Ok(connection)) => connection.is_closed(),
                    Some(Err(_)) => true,
                    None => false,
                });
            if stale || current.is_none() {
                let (url, token) = (self.url.clone(), self.token.clone());
                let opened: LocalBoxFuture<'static, _> =
                    Box::pin(async move { Connection::open(&url, &token).await });
                *current = Some(opened.shared());
            }
            current.clone()
        };
        match opening {
            Some(opening) => opening.await.map_err(Refusal::Lost),
            None => Err(Refusal::Lost("no connection".into())),
        }
    }

    async fn call<R: doppel_protocol::rpc::Rpc>(&self, request: R) -> Result<R::Answer, Refusal> {
        self.connection().await?.call(request).await
    }
}

impl WebDaemon {
    /// The daemon behind the gateway's WebSocket at `url`.
    pub(crate) fn new(url: String, token: String) -> Self {
        Self(Rc::new(Reach {
            url,
            token,
            current: RefCell::new(None),
        }))
    }

    /// `work` with the daemon, answered later.
    fn later<T: 'static, F: Future<Output = Result<T, Refusal>> + 'static>(
        &self,
        work: impl FnOnce(Rc<Reach>) -> F,
    ) -> Answer<T> {
        let answered = work(Rc::clone(&self.0));
        Box::pin(async move { answered.await.map_err(DaemonError::from) })
    }
}

impl From<Refusal> for DaemonError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Lost(why) => Self::Unreachable(why),
            Refusal::Refused(why) => Self::Refused(why),
            Refusal::Machine(failure) => Self::Refused(failure.to_string()),
        }
    }
}

/// An answer this build can't read.
fn unreadable(error: impl std::fmt::Display) -> Refusal {
    Refusal::Refused(format!("an answer this console can't read: {error}"))
}

fn machine(entry: daemon::MachineEntry) -> Result<Machine, ParseError> {
    let name: MachineName = entry.name.parse()?;
    let lineage = entry.recorded_lineage();
    let status = match entry.status {
        Some(daemon::machine_entry::Status::MachineStatus(status)) => Ok(Status::try_from(status)?),
        Some(daemon::machine_entry::Status::Failure(failure)) => {
            Err(MachineFailure::from_wire(failure))
        }
        None => return Err(ParseError::Missing("machine_entry.status")),
    };
    Ok(answers::machine(name, status, lineage))
}

/// How an order ended: refused when nothing was done; unknown when it may
/// have been (the connection broke after it went out, or the answer can't
/// be read).
fn failed(refusal: &Refusal) -> Verdict {
    match refusal {
        Refusal::Machine(failure) => answers::failed(failure),
        Refusal::Refused(why) => Verdict::Refused(why.clone()),
        Refusal::Lost(why) => Verdict::Unknown(why.clone()),
    }
}

/// Carries out `order` on `name`.
async fn order(reach: &Reach, name: &MachineName, order: Order) -> Result<Verdict, Refusal> {
    let connection = reach.connection().await.map_err(|refusal| match refusal {
        // Not sent: nothing was done.
        Refusal::Lost(why) => Refusal::Refused(why),
        other @ (Refusal::Refused(_) | Refusal::Machine(_)) => other,
    })?;
    let name_s = name.to_string();
    Ok(match order {
        Order::Start => {
            connection
                .call(daemon::StartMachine {
                    name: name_s,
                    window: daemon::Window::Hidden as i32,
                })
                .await?;
            answers::done()
        }
        Order::Stop => stop(&connection, name_s, vmm::StopMode::Graceful).await?,
        Order::ForceStop => stop(&connection, name_s, vmm::StopMode::Force).await?,
        Order::Pause => {
            connection
                .call(daemon::PauseMachine { name: name_s })
                .await?;
            answers::done()
        }
        Order::Resume => {
            connection
                .call(daemon::ResumeMachine { name: name_s })
                .await?;
            answers::done()
        }
        Order::Checkpoint(label) => {
            let report = connection
                .call(daemon::TakeCheckpoint {
                    name: name_s,
                    label: label.to_string(),
                    then: daemon::AfterCheckpoint::from(AfterCheckpoint::Resume) as i32,
                })
                .await?;
            match CheckpointReport::try_from(report) {
                Ok(report) => answers::checkpoint(report.result),
                Err(error) => Verdict::Unknown(error.to_string()),
            }
        }
        Order::RollBack(label) => {
            connection
                .call(daemon::RollBack {
                    name: name_s,
                    label: label.to_string(),
                })
                .await?;
            answers::done()
        }
        Order::Restore(label) => {
            connection
                .call(daemon::RestoreMachine {
                    name: name_s,
                    label: label.to_string(),
                })
                .await?;
            answers::done()
        }
        Order::Fork { label, into } => {
            connection
                .call(daemon::ForkMachine {
                    source: name_s,
                    label: label.to_string(),
                    into: into.to_string(),
                })
                .await?;
            Verdict::Done {
                note: None,
                made: Some(into),
            }
        }
        Order::NewCopy(label) => {
            let run = connection
                .call(daemon::RunForks {
                    name: name_s,
                    label: label.to_string(),
                    count: 1,
                })
                .await?;
            let mut made = None;
            let mut refused = Vec::new();
            for fork in run.forks {
                match fork.outcome {
                    Some(daemon::run_fork::Outcome::Failure(failure)) => {
                        refused.push(format!("{}: {}", fork.name, failure.message));
                    }
                    Some(daemon::run_fork::Outcome::Desktop(_)) | None => {
                        made = made.or_else(|| fork.name.parse().ok());
                    }
                }
            }
            if refused.is_empty() {
                Verdict::Done { note: None, made }
            } else {
                Verdict::Refused(refused.join("; "))
            }
        }
        Order::Delete => {
            connection
                .call(daemon::DeleteMachine { name: name_s })
                .await?;
            answers::done()
        }
    })
}

async fn stop(
    connection: &Connection,
    name: String,
    mode: vmm::StopMode,
) -> Result<Verdict, Refusal> {
    let stopped = connection
        .call(daemon::StopMachine {
            name,
            mode: mode as i32,
        })
        .await?;
    Ok(match StopReport::try_from(stopped) {
        Ok(report) => answers::stopped(report.outcome),
        Err(error) => Verdict::Unknown(error.to_string()),
    })
}

impl Daemon for WebDaemon {
    fn machines(&self) -> Answer<Vec<Machine>> {
        self.later(|reach| async move {
            let list = reach.call(daemon::ListMachines {}).await?;
            // Strays (`list.unrecognized`) aren't machines: left out.
            list.machines
                .into_iter()
                .map(machine)
                .collect::<Result<_, _>>()
                .map_err(unreadable)
        })
    }

    fn sessions(&self) -> Answer<Vec<Session>> {
        self.later(|reach| async move {
            let list = reach.call(daemon::ListSessions {}).await?;
            list.sessions
                .into_iter()
                .map(|entry| SessionInfo::try_from(entry).map(answers::session))
                .collect::<Result<_, _>>()
                .map_err(unreadable)
        })
    }

    fn hold(&self, session: SessionId, hold: Hold) -> Answer<()> {
        let session = session.0;
        self.later(move |reach| async move {
            match hold {
                Hold::Pause => reach.call(daemon::PauseSession { session }).await,
                Hold::Resume => reach.call(daemon::ResumeSession { session }).await,
            }
            .map(drop)
        })
    }

    fn checkpoints(&self, name: &MachineName) -> Answer<Vec<CheckpointLabel>> {
        let report = self.report(name);
        Box::pin(async move { Ok(report.await?.checkpoints) })
    }

    fn report(&self, name: &MachineName) -> Answer<MachineReport> {
        let name = name.to_string();
        self.later(|reach| async move {
            let report = reach.call(daemon::GetMachineReport { name }).await?;
            MachineReport::try_from(report).map_err(unreadable)
        })
    }

    fn run(&self, name: &MachineName, order_: &Order) -> Later<Verdict> {
        let (reach, name, order_) = (Rc::clone(&self.0), name.clone(), order_.clone());
        Box::pin(async move {
            order(&reach, &name, order_)
                .await
                .unwrap_or_else(|refusal| failed(&refusal))
        })
    }

    fn open_screen(&self, name: &MachineName) -> Answer<Screen> {
        let name = name.clone();
        self.later(|reach| async move {
            let connection = reach.connection().await?;
            let (opened, frames) = connection
                .stream(daemon::OpenEncodedDisplay {
                    name: name.to_string(),
                })
                .await?;
            Ok(Screen {
                connection,
                name,
                frames,
                points: opened
                    .display_points
                    .and_then(|size| Size::try_from(size).ok()),
            })
        })
    }

    fn watch(&self) -> Answer<Activity> {
        self.later(|reach| async move {
            let connection = reach.connection().await?;
            let (_, events) = connection.stream(daemon::WatchActivity {}).await?;
            let activity: Activity = Box::pin(events.filter_map(|event| async move {
                match event {
                    daemon::daemon_event::Kind::Activity(activity) => {
                        // What this build can't read is passed over.
                        told::Activity::try_from(activity)
                            .ok()
                            .map(|activity| Ok::<_, SourceError>(ActivityEvent::of(activity)))
                    }
                    daemon::daemon_event::Kind::DisplayFrame(_)
                    | daemon::daemon_event::Kind::EncodedFrame(_) => None,
                }
            }));
            Ok(activity)
        })
    }

    fn recordings(&self) -> Answer<Vec<RecordingSummary>> {
        self.later(|reach| async move {
            let list = reach.call(daemon::ListRecordings {}).await?;
            Ok(list
                .recordings
                .into_iter()
                .filter_map(|summary| kept::RecordingSummary::try_from(summary).ok())
                .map(|summary| {
                    let started = local(summary.started_unix_seconds as f64 * 1000.0);
                    answers::summary(summary, started)
                })
                .collect())
        })
    }

    fn open_recording(&self, id: &RecordingId) -> Answer<Recording> {
        let id = id.as_str().to_owned();
        self.later(|reach| async move {
            let recording = reach.call(daemon::OpenRecording { id }).await?;
            kept::Recording::try_from(recording)
                .map(Recording::of)
                .map_err(|_| unreadable("a recording without its id"))
        })
    }

    fn seen_snapshot(&self, id: &RecordingId, line: u32) -> Answer<AxSnapshot> {
        let recording = id.as_str().to_owned();
        self.later(move |reach| async move {
            let snapshot = reach
                .call(daemon::OpenSeenSnapshot { recording, line })
                .await?;
            AxSnapshot::try_from(snapshot).map_err(unreadable)
        })
    }

    fn observe(&self, name: &MachineName) -> Answer<AxSnapshot> {
        let name = name.to_string();
        self.later(|reach| async move {
            let snapshot = reach.call(daemon::ObserveElements { name }).await?;
            AxSnapshot::try_from(snapshot).map_err(unreadable)
        })
    }

    fn observe_text(&self, name: &MachineName) -> Answer<TextRead> {
        let name = name.to_string();
        self.later(|reach| async move {
            let text = reach.call(daemon::ObserveText { name }).await?;
            let text = ScreenText::try_from(text).map_err(unreadable)?;
            Ok(TextRead::of(&told::TextRead {
                text,
                query: None,
                found: Vec::new(),
            }))
        })
    }

    fn video(&self, id: &RecordingId, file: &VideoName) -> Answer<VideoAt> {
        // Both names were parsed from the daemon's answer: path-safe.
        let url = format!("/recordings/{}/{}", id.as_str(), file.as_str());
        Box::pin(async move { Ok(url) })
    }
}

impl std::fmt::Debug for WebDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebDaemon")
            .field("url", &self.0.url)
            .finish_non_exhaustive()
    }
}
