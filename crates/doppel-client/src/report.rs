//! What the daemon answers, in the client's own terms: parsed once from the
//! wire, so callers never see a protobuf message.

use std::fmt;
use std::time::Duration;

use doppel_machine::checkpoint::{GuestAfterFailure, GuestSync, Step};
use doppel_machine::machine::MachineName;
use doppel_machine::outcome::{Certainty, Failure};
use doppel_protocol::convert::ParseError;
use doppel_protocol::pb::daemon::v1 as daemon;

/// Why a machine operation failed on the daemon's side: its description,
/// whether the operation may still have taken effect, and its causes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineFailure {
    message: String,
    certainty: Certainty,
    cause: Option<Box<Cause>>,
}

/// One cause of a [`MachineFailure`], as the daemon described it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cause {
    message: String,
    cause: Option<Box<Cause>>,
}

impl Cause {
    /// The chain, outermost first.
    fn chain(messages: Vec<String>) -> Option<Box<Self>> {
        messages.into_iter().rev().fold(None, |cause, message| {
            Some(Box::new(Self { message, cause }))
        })
    }
}

impl fmt::Display for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Cause {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause.as_deref().map(|cause| cause as _)
    }
}

impl MachineFailure {
    /// For callers that hold a failure of their own to report the same way.
    #[must_use]
    pub const fn new(message: String, certainty: Certainty) -> Self {
        Self {
            message,
            certainty,
            cause: None,
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn from_wire(failure: daemon::MachineFailure) -> Self {
        Self {
            certainty: daemon::Certainty::try_from(failure.certainty)
                .map_or(Certainty::MaybeDone, Certainty::from),
            message: failure.message,
            cause: Cause::chain(failure.causes),
        }
    }
}

impl fmt::Display for MachineFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MachineFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause.as_deref().map(|cause| cause as _)
    }
}

impl Failure for MachineFailure {
    fn certainty(&self) -> Certainty {
        self.certainty
    }
}

/// Whether the VMM shows the VM in a native window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Hidden,
    Shown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    /// Ask the guest to shut down.
    Graceful,
    /// Power off: unsaved guest work is lost.
    Force,
}

/// How the guest was asked to stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopRequest {
    Forced,
    /// The logged-in session was asked to shut down.
    Session,
    /// The same, after typing the stored password on a locked screen.
    SessionAfterUnlock {
        unlocked_after: Duration,
    },
    /// The power button, because the session couldn't be asked: `why`.
    PowerButton {
        why: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    Stopped,
    /// Asked to shut down, and still running.
    ShutdownPending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopReport {
    pub request: StopRequest,
    pub outcome: StopOutcome,
}

impl StopReport {
    pub(crate) fn from_wire(stopped: daemon::MachineStopped) -> Result<Self, ParseError> {
        use daemon::machine_stopped::Request;
        let request = match stopped.request {
            None => return Err(ParseError::Missing("machine_stopped.request")),
            Some(Request::Forced(_)) => StopRequest::Forced,
            Some(Request::Session(_)) => StopRequest::Session,
            Some(Request::SessionAfterUnlockMicros(micros)) => StopRequest::SessionAfterUnlock {
                unlocked_after: Duration::from_micros(micros),
            },
            Some(Request::PowerButton(why)) => StopRequest::PowerButton { why },
        };
        let outcome = match daemon::StopOutcome::try_from(stopped.outcome) {
            Ok(daemon::StopOutcome::Stopped) => StopOutcome::Stopped,
            Ok(daemon::StopOutcome::ShutdownPending) => StopOutcome::ShutdownPending,
            Ok(daemon::StopOutcome::Unspecified) | Err(_) => {
                return Err(ParseError::UnknownEnum {
                    field: "machine_stopped.outcome",
                    value: stopped.outcome,
                });
            }
        };
        Ok(Self { request, outcome })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resumed {
    Resumed,
    /// It was running already: nothing to do.
    AlreadyRunning,
}

/// How a checkpoint ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointResult {
    Completed {
        memory_bytes: u64,
        /// Why the guest wasn't synced first, if it wasn't.
        sync_skipped: Option<String>,
        /// The checkpoint is valid, but the guest may be left paused.
        left_paused: Option<MachineFailure>,
    },
    Failed {
        at: Step,
        error: MachineFailure,
        guest: GuestAfterFailure,
    },
    /// Neither success nor failure can be claimed.
    Unknown { at: Step, error: MachineFailure },
    /// The procedure ended before a terminal state (a bug).
    InProgress { awaiting: Step },
}

impl CheckpointResult {
    /// What a report of a completed checkpoint should add about the sync:
    /// nothing when the guest was synced.
    #[must_use]
    pub fn sync_caveat(sync_skipped: Option<&str>) -> String {
        sync_skipped.map_or_else(String::new, |why| {
            GuestSync::<MachineFailure>::skipped_caveat(&why)
        })
    }
}

/// One cleanup step after a failed checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cleanup {
    /// What was done, described.
    pub action: String,
    pub result: Result<(), MachineFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointReport {
    pub result: CheckpointResult,
    /// How long the guest was paused, once it was.
    pub paused_for: Option<Duration>,
    pub cleanup: Vec<Cleanup>,
}

impl CheckpointReport {
    pub(crate) fn from_wire(report: daemon::CheckpointReport) -> Result<Self, ParseError> {
        use daemon::checkpoint_report::Result as Wire;
        let failure = |failure: Option<daemon::MachineFailure>, field| {
            failure
                .map(MachineFailure::from_wire)
                .ok_or(ParseError::Missing(field))
        };
        let result = match report.result {
            None => return Err(ParseError::Missing("checkpoint_report.result")),
            Some(Wire::Completed(completed)) => CheckpointResult::Completed {
                memory_bytes: completed.memory_bytes,
                sync_skipped: completed.sync_skipped,
                left_paused: completed.left_paused.map(MachineFailure::from_wire),
            },
            Some(Wire::Failed(failed)) => CheckpointResult::Failed {
                at: ParseError::checkpoint_step("checkpoint_failed.at", failed.at)?,
                error: failure(failed.error, "checkpoint_failed.error")?,
                guest: ParseError::guest_after_failure("checkpoint_failed.guest", failed.guest)?,
            },
            Some(Wire::Unknown(unknown)) => CheckpointResult::Unknown {
                at: ParseError::checkpoint_step("checkpoint_unknown.at", unknown.at)?,
                error: failure(unknown.error, "checkpoint_unknown.error")?,
            },
            Some(Wire::InProgress(step)) => CheckpointResult::InProgress {
                awaiting: ParseError::checkpoint_step("checkpoint_report.in_progress", step)?,
            },
        };
        Ok(Self {
            result,
            paused_for: report.paused_for_micros.map(Duration::from_micros),
            cleanup: report
                .cleanup
                .into_iter()
                .map(|cleanup| Cleanup {
                    action: cleanup.action,
                    result: cleanup
                        .failure
                        .map_or(Ok(()), |failure| Err(MachineFailure::from_wire(failure))),
                })
                .collect(),
        })
    }
}

/// How an action on an element ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acted {
    Done,
    /// The element is gone or changed since its snapshot; nothing was done.
    StaleReference,
}

/// A session open on the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: u64,
    pub client: String,
    /// The machines its requests named, in first-use order.
    pub machines: Vec<MachineName>,
    pub answered: u64,
    /// Captions of the requests running or waiting their turn now.
    pub running: Vec<String>,
    pub state: SessionStatus,
}

/// Whether a session's calls may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Active,
    /// Its next calls wait until it's resumed.
    Paused,
    /// A state this build doesn't know, as sent.
    Unknown(i32),
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => f.write_str("active"),
            Self::Paused => f.write_str("paused"),
            Self::Unknown(raw) => write!(f, "state {raw}"),
        }
    }
}

/// A person had control of a machine a session used, and handed it back:
/// what the session saw before may have changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandedBack {
    pub machine: MachineName,
    /// How long the person had it.
    pub held: std::time::Duration,
}

impl HandedBack {
    pub(crate) fn from_wire(note: &daemon::HandedBack) -> Result<Self, ParseError> {
        Ok(Self {
            machine: note
                .machine
                .parse()
                .map_err(|_| ParseError::Missing("handed_back.machine"))?,
            held: std::time::Duration::from_millis(note.held_millis),
        })
    }
}

impl SessionInfo {
    pub(crate) fn from_wire(entry: daemon::SessionEntry) -> Result<Self, ParseError> {
        Ok(Self {
            id: entry.id,
            machines: entry
                .machines
                .iter()
                .map(|name| name.parse())
                .collect::<Result<_, _>>()?,
            client: entry.client,
            answered: entry.answered,
            running: entry.running,
            state: match daemon::SessionState::try_from(entry.state) {
                Ok(daemon::SessionState::Active) => SessionStatus::Active,
                Ok(daemon::SessionState::Paused) => SessionStatus::Paused,
                Ok(daemon::SessionState::Unspecified) | Err(_) => {
                    SessionStatus::Unknown(entry.state)
                }
            },
        })
    }
}
