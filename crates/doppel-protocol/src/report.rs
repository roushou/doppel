//! What the daemon answers, in its clients' terms: parsed once from the
//! wire, so callers never see a protobuf message.

use std::fmt;
use std::time::Duration;

use crate::pb::daemon::v1 as daemon;
use doppel_desktop::outcome::{Certainty, Failure};
pub use doppel_machine::activity::SessionId;

use doppel_machine::machine::MachineName;
pub use doppel_machine::stop::{StopMode, StopOutcome};
pub use doppel_machine::vm::{Resumed, Window};

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

    pub fn from_wire(failure: daemon::MachineFailure) -> Self {
        Self {
            certainty: daemon::Certainty::try_from(failure.certainty)
                .map_or(Certainty::MaybeDone, daemon::Certainty::or_maybe_done),
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

/// How the guest was asked to stop; when through the power button, why
/// the session couldn't be asked, as the daemon said.
pub type StopRequest = doppel_machine::stop::StopRequest<String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopReport {
    pub request: StopRequest,
    pub outcome: StopOutcome,
}

/// How a checkpoint ended, its errors as the daemon sent them.
pub type CheckpointResult = doppel_machine::checkpoint::CheckpointResult<MachineFailure>;

/// How a completed checkpoint left the guest.
pub type GuestAfter = doppel_machine::checkpoint::GuestAfter<MachineFailure>;

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
    pub id: SessionId,
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
