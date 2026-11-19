//! How a checkpoint ended, as a client is told: the procedure's terminal
//! state without its plan, and with what the guest's screen showed after.

use doppel_desktop::outcome::Unconfirmed;

use super::{GuestAfterFailure, GuestSync, Step};
use crate::screen_probe::HostScreen;

/// How a checkpoint ended. `E` is the error as the reader carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointResult<E> {
    /// The checkpoint is kept and valid.
    Completed {
        memory_bytes: u64,
        /// Why the guest wasn't synced first, if it wasn't.
        sync_skipped: Option<String>,
        guest: GuestAfter<E>,
    },
    /// No checkpoint was kept.
    Failed {
        at: Step,
        error: E,
        guest: GuestAfterFailure,
    },
    /// Neither success nor failure can be claimed.
    Unknown { at: Step, error: E },
    /// The procedure ended before a terminal state (a bug): whether a
    /// checkpoint was kept is unknown.
    InProgress { awaiting: Step },
}

/// How a completed checkpoint left the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestAfter<E> {
    /// Running or stopped, as asked; whether screenshots show it as it is,
    /// when it was resumed.
    AsAsked { screen: Option<HostScreen> },
    /// It may still be paused: the resume or stop failed, or its outcome
    /// is unknown.
    LeftPaused(Unconfirmed<E>),
}

impl<E> CheckpointResult<E> {
    /// The same result, its errors turned into the reader's.
    #[must_use]
    pub fn map_error<F>(self, into: impl Fn(E) -> F) -> CheckpointResult<F> {
        match self {
            Self::Completed {
                memory_bytes,
                sync_skipped,
                guest,
            } => CheckpointResult::Completed {
                memory_bytes,
                sync_skipped,
                guest: match guest {
                    GuestAfter::AsAsked { screen } => GuestAfter::AsAsked { screen },
                    GuestAfter::LeftPaused(why) => GuestAfter::LeftPaused(why.map(&into)),
                },
            },
            Self::Failed { at, error, guest } => CheckpointResult::Failed {
                at,
                error: into(error),
                guest,
            },
            Self::Unknown { at, error } => CheckpointResult::Unknown {
                at,
                error: into(error),
            },
            Self::InProgress { awaiting } => CheckpointResult::InProgress { awaiting },
        }
    }

    /// What a report of a completed checkpoint adds about the sync:
    /// nothing when the guest was synced.
    #[must_use]
    pub fn sync_caveat(sync_skipped: Option<&str>) -> String {
        sync_skipped.map_or_else(String::new, |why| {
            GuestSync::<std::convert::Infallible>::skipped_caveat(&why)
        })
    }
}
