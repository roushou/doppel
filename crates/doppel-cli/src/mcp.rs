//! `doppel mcp`: serves `doppel-mcp`'s rmcp server over stdio and relays
//! its tool calls to the daemon, as one session. Logs go to stderr: stdout
//! carries only protocol messages.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use doppel_client::{
    Acted, Call, CheckpointResult, Client, ClientError, Notes, Record, StopMode, StopOutcome,
    Window,
};
use doppel_host::capabilities::{Clock, SystemClock};
use doppel_machine::ax::refs::{Ref, RefTarget, SnapshotRefs};
use doppel_machine::checkpoint::AfterCheckpoint;
use doppel_machine::deadline::Deadline;
use doppel_machine::machine::MachineName;
use doppel_machine::outcome::{Certainty, Failure, Outcome, Unconfirmed};
use doppel_machine::vm::NextStart;
use doppel_protocol::transport::Cancel;

use doppel_host::machines::CALL_TIMEOUT;
use doppel_mcp::{
    Cancellation, CheckpointEnd, ClientName, Notice, Ran, Runner, Server, StartedFrom,
    StatusReport, ToolAction, ToolCall, ToolOutcome,
};
use rmcp::ServiceExt;
use tokio::sync::watch;

use crate::daemon_at::{DaemonAt, LIFECYCLE, within};

/// The [`Runner`] `doppel mcp` serves with: each call is a request on the
/// session's connection, sent from tokio's blocking pool, so calls run
/// concurrently and the daemon queues them per machine.
#[derive(Debug)]
pub struct Relay {
    client: Arc<Client<SystemClock>>,
    /// Whether the session it opens once the client introduces itself records.
    record: Record,
    /// The temporary machine made for this session, when it asked for one.
    made: Option<MadeMachine>,
}

/// How the session's temporary machine is coming up.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Startup {
    Starting,
    /// At an agent-ready desktop.
    Ready,
    /// It didn't start, or started without an agent-ready desktop: why.
    Failed(String),
}

/// The session's temporary machine: the daemon deletes it when the
/// session's connection closes.
#[derive(Debug)]
struct MadeMachine {
    name: MachineName,
    startup: watch::Receiver<Startup>,
}

impl MadeMachine {
    /// Waits until it has come up, or failed to.
    async fn ready(&self) -> Result<(), String> {
        let mut startup = self.startup.clone();
        let settled = startup
            .wait_for(|state| match state {
                Startup::Starting => false,
                Startup::Ready | Startup::Failed(_) => true,
            })
            .await;
        match settled.as_deref() {
            Ok(Startup::Ready) => Ok(()),
            Ok(Startup::Failed(why)) => Err(why.clone()),
            Ok(Startup::Starting) | Err(_) => Err("its start was lost".into()),
        }
    }
}

impl Relay {
    /// How long a call may take before the relay gives up on the daemon.
    /// The daemon enforces each operation's own, shorter budget; this only
    /// bounds a daemon that hangs.
    const CALL_BUDGET: Duration = Duration::from_secs(15 * 60);

    /// How long telling viewers a snapshot's refs may take: the daemon
    /// answers at once, and the model's result waits on it.
    const NAME_BUDGET: Duration = Duration::from_secs(2);

    /// Connects to the daemon at `at`. The session opens when the MCP
    /// client introduces itself, named after it ([`Runner::introduce`]).
    pub fn connect(at: &DaemonAt, record: Record) -> Result<Self> {
        let client = at.connect()?;
        Ok(Self {
            client: Arc::new(client),
            record,
            made: None,
        })
    }

    /// Has the daemon make a temporary machine, a fresh fork of `golden`'s
    /// template, for this session, and starts it in the background, hidden. Tool calls on it
    /// wait until its desktop is up. The daemon deletes it when this
    /// process's connection closes, however the process ends.
    ///
    /// # Errors
    /// The daemon refused or failed to make it: `golden` isn't a golden
    /// image, or one more machine wouldn't fit beside the running ones.
    pub fn make_machine(&mut self, golden: &MachineName) -> Result<MachineName> {
        let name = self
            .client
            .make_temporary_machine(golden, within(LIFECYCLE))
            .with_context(|| format!("making a machine from {golden}"))?;
        eprintln!("doppel mcp: {name} made from {golden}; starting it");
        let (tell, startup) = watch::channel(Startup::Starting);
        let client = Arc::clone(&self.client);
        let starting = name.clone();
        std::thread::spawn(move || {
            let started = Self::start(&client, &starting);
            match &started {
                Startup::Ready => eprintln!("doppel mcp: {starting} is ready"),
                Startup::Failed(why) => eprintln!("doppel mcp: {starting}: {why}"),
                Startup::Starting => {}
            }
            // No one waits any more: the server has ended.
            let _ = tell.send(started);
        });
        self.made = Some(MadeMachine {
            name: name.clone(),
            startup,
        });
        Ok(name)
    }

    /// Starts `machine`, hidden, and waits for an agent-ready desktop.
    fn start(client: &Client<SystemClock>, machine: &MachineName) -> Startup {
        if let Err(error) = client.start(machine, Window::Hidden, within(LIFECYCLE)) {
            return Startup::Failed(format!("{machine} didn't start: {error}"));
        }
        match client.await_desktop(machine, within(LIFECYCLE)) {
            Ok(outcome) if outcome.is_ready() => Startup::Ready,
            Ok(outcome) => Startup::Failed(format!(
                "{machine} started without an agent-ready desktop: {outcome}"
            )),
            Err(error) => Startup::Failed(format!(
                "{machine} started, but its desktop didn't come up: {error}"
            )),
        }
    }

    /// What the session is called when the client gives no name.
    fn unnamed() -> String {
        format!("doppel mcp (pid {})", std::process::id())
    }

    /// Serves MCP over stdio until the client disconnects. The session ends
    /// with the connection, which completes its recordings.
    pub fn serve(self, default_machine: Option<MachineName>) -> Result<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("starting the async runtime")?;
        runtime.block_on(async {
            let running = Server::new(self, default_machine)
                .serve(rmcp::transport::stdio())
                .await
                .context("starting the MCP session")?;
            running.waiting().await.context("serving MCP")?;
            Ok(())
        })
    }

    fn deadline(budget: Duration) -> Deadline {
        Deadline::after(SystemClock.now(), budget)
    }
}

/// Why a tool call failed: the daemon's answer, or what only a tool knows.
/// Each says whether the call may still have taken effect, which the model
/// is told.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error(transparent)]
    Daemon(#[from] ClientError),
    #[error(
        "{r} ({target}) is gone or has changed since that snapshot; nothing was done. Take a new snapshot."
    )]
    StaleRef { r: Ref, target: RefTarget },
    #[error("{0} didn't power off; check status")]
    StillRunning(MachineName),
    #[error("the call didn't finish: {0}")]
    Interrupted(#[source] tokio::task::JoinError),
    /// The session's temporary machine didn't come up, so the call wasn't
    /// sent.
    #[error("{0}; nothing was done")]
    NotStarted(String),
}

impl Failure for ToolError {
    fn certainty(&self) -> Certainty {
        match self {
            Self::Daemon(ClientError::Machine(failure)) => failure.certainty(),
            // Refused before it ran.
            Self::Daemon(
                ClientError::NoDaemon(_)
                | ClientError::Connect(_)
                | ClientError::VersionMismatch { .. }
                | ClientError::Encode(_)
                | ClientError::Remote(_),
            )
            | Self::StaleRef { .. }
            | Self::NotStarted(_) => Certainty::NotDone,
            // Sent, and no answer to trust.
            Self::Daemon(
                ClientError::Io(_)
                | ClientError::Closed
                | ClientError::Lost(_)
                | ClientError::Timeout
                | ClientError::Stream(_)
                | ClientError::Protocol(_)
                | ClientError::WrongAnswer(_)
                | ClientError::Parse(_),
            )
            | Self::StillRunning(_)
            | Self::Interrupted(_) => Certainty::MaybeDone,
        }
    }
}

impl Runner for Relay {
    type Error = ToolError;

    /// Opens the session, named as the client named itself.
    async fn introduce(&self, client: Option<ClientName>) -> Result<(), ToolError> {
        let connection = Arc::clone(&self.client);
        let record = self.record;
        let name = client.map_or_else(Relay::unnamed, |name| name.to_string());
        let session = tokio::task::spawn_blocking(move || {
            connection.open_session(&name, record, Relay::deadline(CALL_TIMEOUT))
        })
        .await
        .map_err(ToolError::Interrupted)??;
        match &session.recording {
            Some(dir) => eprintln!(
                "doppel mcp: session {}, recorded into {}",
                session.id,
                dir.display()
            ),
            None => eprintln!("doppel mcp: session {}", session.id),
        }
        Ok(())
    }

    /// A cancelled call is cancelled in the daemon: it gives up its place
    /// in the queue, waits end, an exec stops its program.
    async fn run(&self, call: ToolCall, cancellation: Cancellation) -> Ran<ToolError> {
        if let Some(made) = self.made.as_ref().filter(|made| made.name == call.machine) {
            let waited = tokio::select! {
                ready = made.ready() => ready,
                () = cancellation.cancelled() => Err(format!("{} was still starting", made.name)),
            };
            if let Err(why) = waited {
                return Ran {
                    outcome: ToolOutcome::Failed(ToolError::NotStarted(why).into()),
                    notices: Vec::new(),
                };
            }
        }
        let client = Arc::clone(&self.client);
        let machine = call.machine.clone();
        // The MCP client's cancellation, passed on to the daemon client.
        let cancel = Cancel::new();
        // The notes that come with this call's answers.
        let notes = Arc::new(Notes::new());
        let passing_on = tokio::spawn({
            let cancel = cancel.clone();
            async move {
                cancellation.cancelled().await;
                cancel.cancel();
            }
        });
        let outcome = tokio::task::spawn_blocking({
            let notes = Arc::clone(&notes);
            move || {
                let caption = call.caption();
                let why = call
                    .why
                    .as_ref()
                    .map_or("", doppel_mcp::Why::as_str)
                    .to_owned();
                let calls = Calls {
                    client: &client,
                    call: Call {
                        by: Relay::deadline(Relay::CALL_BUDGET),
                        caption: &caption,
                        why: &why,
                        cancel: &cancel,
                        notes: Some(&notes),
                    },
                };
                let machine = call.machine.clone();
                calls.outcome(call).unwrap_or_else(|error| {
                    // Plain Display: Doppel's error messages already embed their
                    // cause, so the alternate (chain) form would repeat it.
                    eprintln!("doppel mcp: {machine}: {error}");
                    ToolOutcome::Failed(error)
                })
            }
        })
        .await
        .unwrap_or_else(|error| {
            eprintln!("doppel mcp: {machine}: {error}");
            ToolOutcome::Failed(Unconfirmed::Unknown(ToolError::Interrupted(error)))
        });
        // The call is over: a cancel now has nothing to stop.
        passing_on.abort();
        let notices = match notes.take() {
            Ok(notes) => notes
                .into_iter()
                .map(|note| Notice::HandedBack {
                    machine: note.machine,
                    held: note.held,
                })
                .collect(),
            Err(error) => {
                eprintln!("doppel mcp: a note from the daemon: {error}");
                Vec::new()
            }
        };
        Ran { outcome, notices }
    }

    /// Short and blocking, from tokio's blocking pool; a failure only costs
    /// viewers the names, so it is logged and dropped.
    async fn name(&self, machine: MachineName, refs: SnapshotRefs) {
        let client = Arc::clone(&self.client);
        let named = tokio::task::spawn_blocking(move || {
            client
                .name_elements(&machine, &refs, Relay::deadline(Relay::NAME_BUDGET))
                .map_err(|error| format!("{machine}: naming a snapshot's elements: {error}"))
        })
        .await;
        match named {
            Ok(Ok(())) => {}
            Ok(Err(why)) => eprintln!("doppel mcp: {why}"),
            Err(error) => eprintln!("doppel mcp: naming a snapshot's elements: {error}"),
        }
    }
}

/// Runs one tool call as requests to the daemon, blocking until it's done.
struct Calls<'a> {
    client: &'a Client<SystemClock>,
    call: Call<'a>,
}

impl Calls<'_> {
    fn outcome(&self, call: ToolCall) -> Outcome<ToolOutcome<ToolError>, ToolError> {
        let client = self.client;
        let at = self.call;
        let machine = call.machine;
        Ok(match call.action {
            ToolAction::Status => {
                let report = client.report(&machine, at).map_err(ToolError::from)?;
                ToolOutcome::Status(StatusReport {
                    vm: report.status.to_string(),
                    agent: report.agent,
                    checkpoints: report.checkpoints,
                    machine,
                })
            }
            ToolAction::Screenshot { via, size } => ToolOutcome::Screenshot {
                shot: client
                    .screenshot(&machine, via, size, at)
                    .map_err(ToolError::from)?,
                via,
            },
            ToolAction::FindText(query) => ToolOutcome::TextFound {
                text: client
                    .screen_text(&machine, query.as_ref(), at)
                    .map_err(ToolError::from)?,
                query,
            },
            ToolAction::Input { via, input } => {
                client
                    .input(&machine, via, &input, at)
                    .map_err(ToolError::from)?;
                ToolOutcome::Sent { input, via }
            }
            ToolAction::Snapshot(spec) => ToolOutcome::Snapshot {
                snapshot: client
                    .snapshot(&machine, spec.spec, at)
                    .map_err(ToolError::from)?,
                machine,
                policy: spec.policy,
            },
            ToolAction::Act { r, target, act } => {
                match client.act(&machine, &act, at).map_err(ToolError::from)? {
                    Acted::Done => ToolOutcome::Acted { r, target, act },
                    // Name the ref the model used, not the agent's internal ids.
                    Acted::StaleReference => {
                        return Err(ToolError::StaleRef { r, target }.into());
                    }
                }
            }
            ToolAction::WaitFor { wait, snapshot } => ToolOutcome::Waited {
                outcome: client
                    .wait_for(&machine, &wait, snapshot.spec, at)
                    .map_err(ToolError::from)?,
                machine,
                condition: wait.condition().clone(),
                policy: snapshot.policy,
            },
            ToolAction::WaitForText(wait) => ToolOutcome::WaitedForText {
                outcome: client
                    .wait_for_text(&machine, &wait, at)
                    .map_err(ToolError::from)?,
                condition: wait.condition().clone(),
            },
            ToolAction::Exec(request) => ToolOutcome::Executed(
                client
                    .exec(&machine, &request, at)
                    .map_err(ToolError::from)?,
            ),
            ToolAction::ReadFile(request) => ToolOutcome::FileRead {
                data: client
                    .read_file(&machine, &request, at)
                    .map_err(ToolError::from)?,
                path: request.path,
            },
            ToolAction::WriteFile(request) => {
                client
                    .write_file(&machine, &request, at)
                    .map_err(ToolError::from)?;
                ToolOutcome::FileWritten {
                    bytes: request.data().len(),
                    path: request.path,
                    policy: request.policy,
                }
            }
            ToolAction::Checkpoint(label) => {
                let report = client
                    .checkpoint(&machine, &label, AfterCheckpoint::Resume, at)
                    .map_err(ToolError::from)?;
                for cleanup in &report.cleanup {
                    eprintln!(
                        "doppel mcp: {machine}: checkpoint cleanup {}: {:?}",
                        cleanup.action, cleanup.result
                    );
                }
                ToolOutcome::Checkpointed {
                    machine,
                    label,
                    result: Self::checkpoint_end(report.result),
                    paused_for: report.paused_for,
                }
            }
            ToolAction::Rollback(checkpoint) => ToolOutcome::RolledBack {
                desktop: client
                    .rollback(&machine, &checkpoint, at)
                    .map_err(ToolError::from)?,
                machine,
                checkpoint,
            },
            ToolAction::Fork { checkpoint, into } => {
                client
                    .fork(&machine, &checkpoint, &into, at)
                    .map_err(ToolError::from)?;
                ToolOutcome::Forked {
                    source: machine,
                    checkpoint,
                    into,
                }
            }
            // Hidden: the model drives it. Then the desktop.
            ToolAction::Start => {
                let (_, next) = client
                    .start(&machine, Window::Hidden, at)
                    .map_err(ToolError::from)?;
                let from = match next {
                    NextStart::ColdBoot => StartedFrom::ColdBoot,
                    NextStart::ResumeSavedState => StartedFrom::SavedState,
                };
                ToolOutcome::Started {
                    desktop: client
                        .await_desktop(&machine, at)
                        .map_err(ToolError::from)?,
                    machine,
                    from,
                }
            }
            ToolAction::Mark { mark, on } => {
                client.mark(&machine, &mark, at).map_err(ToolError::from)?;
                ToolOutcome::Marked { mark, on }
            }
            // Forced: a graceful stop waits on the guest's shutdown dialog.
            ToolAction::Stop => {
                match client
                    .stop(&machine, StopMode::Force, at)
                    .map_err(ToolError::from)?
                    .outcome
                {
                    StopOutcome::Stopped => ToolOutcome::Stopped { machine },
                    StopOutcome::ShutdownPending => {
                        return Err(ToolError::StillRunning(machine).into());
                    }
                }
            }
        })
    }

    fn checkpoint_end(result: CheckpointResult) -> CheckpointEnd<ToolError> {
        let failure = |failure: doppel_client::MachineFailure| {
            ToolError::Daemon(ClientError::Machine(failure))
        };
        match result {
            CheckpointResult::Completed {
                memory_bytes,
                sync_skipped,
                left_paused,
            } => CheckpointEnd::Completed {
                memory_bytes,
                sync_skipped,
                left_paused: left_paused.map(|why| Unconfirmed::new(why.certainty(), failure(why))),
            },
            CheckpointResult::Failed { at, error, guest } => CheckpointEnd::Failed {
                at,
                error: failure(error),
                guest,
            },
            CheckpointResult::Unknown { at, error } => CheckpointEnd::Unknown {
                at,
                error: failure(error),
            },
            CheckpointResult::InProgress { .. } => CheckpointEnd::InProgress,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn made(startup: watch::Receiver<Startup>) -> MadeMachine {
        MadeMachine {
            name: "ready-clean-1".parse().unwrap(),
            startup,
        }
    }

    #[tokio::test]
    async fn calls_wait_for_the_machine_to_come_up() {
        let (tell, startup) = watch::channel(Startup::Starting);
        let machine = made(startup);
        let told = tokio::spawn(async move {
            tokio::task::yield_now().await;
            tell.send(Startup::Ready).unwrap();
            tell
        });
        assert_eq!(machine.ready().await, Ok(()));
        drop(told.await.unwrap());
    }

    #[tokio::test]
    async fn a_failed_start_is_every_call_s_answer() {
        let (tell, startup) = watch::channel(Startup::Starting);
        let machine = made(startup);
        tell.send(Startup::Failed("no VMM".into())).unwrap();
        assert_eq!(machine.ready().await, Err("no VMM".into()));
        assert_eq!(machine.ready().await, Err("no VMM".into()));
    }

    #[tokio::test]
    async fn a_start_that_never_reports_is_lost_not_ready() {
        let (tell, startup) = watch::channel(Startup::Starting);
        let machine = made(startup);
        drop(tell);
        assert_eq!(machine.ready().await, Err("its start was lost".into()));
    }
}
