//! A VM's state: as its VMM reports it while it runs, and a machine's
//! status, running or stopped.

use crate::agent::AgentReport;
use crate::bundle::CheckpointLabel;

/// A machine's state, its agent's, and its checkpoints.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineReport {
    pub status: Status,
    pub agent: AgentReport,
    pub checkpoints: Vec<CheckpointLabel>,
}

/// The VMM's view of the VM. Unrecognised values from a newer VMM are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Starting,
    Running,
    Pausing,
    Paused,
    Resuming,
    Saving,
    Restoring,
    Stopping,
    Stopped,
    Error,
    Unrecognized(i32),
}

impl RunState {
    /// States in which the guest is executing and will answer the agent.
    #[must_use]
    pub const fn is_running(self) -> bool {
        match self {
            Self::Running => true,
            Self::Starting
            | Self::Pausing
            | Self::Paused
            | Self::Resuming
            | Self::Saving
            | Self::Restoring
            | Self::Stopping
            | Self::Stopped
            | Self::Error
            | Self::Unrecognized(_) => false,
        }
    }
}

impl RunState {
    /// States the VM leaves on its own: a save or a shutdown in progress.
    /// Whoever finds one waits for it to end before changing the machine.
    #[must_use]
    pub const fn is_transient(self) -> bool {
        match self {
            Self::Starting
            | Self::Pausing
            | Self::Resuming
            | Self::Saving
            | Self::Restoring
            | Self::Stopping => true,
            Self::Running | Self::Paused | Self::Stopped | Self::Error | Self::Unrecognized(_) => {
                false
            }
        }
    }
}

impl std::fmt::Display for RunState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Pausing => "pausing",
            Self::Paused => "paused",
            Self::Resuming => "resuming",
            Self::Saving => "saving",
            Self::Restoring => "restoring",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Error => "error",
            Self::Unrecognized(n) => return write!(f, "unrecognized({n})"),
        };
        f.write_str(name)
    }
}

/// Whether a stopped machine will resume from saved memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextStart {
    ColdBoot,
    ResumeSavedState,
}

/// A machine's status: its VMM's run state, or how it would start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Stopped(NextStart),
    Live(RunState),
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Live(state) => write!(f, "{state}"),
            Self::Stopped(NextStart::ColdBoot) => f.write_str("stopped"),
            Self::Stopped(NextStart::ResumeSavedState) => {
                f.write_str("stopped (will resume saved state)")
            }
        }
    }
}
