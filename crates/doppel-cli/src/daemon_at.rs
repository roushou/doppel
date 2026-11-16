//! Machine commands go to doppeld: the one that serves this store, which
//! launchd starts on the first connection once `doppel daemon install` ran,
//! or another Mac's, through a socket forwarded with `ssh -L` (`--host`).
//! Commands use its client directly; this says where it is, connects with
//! advice when nothing answers, and sets the client's deadlines.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use doppel_client::{Client, ClientError};
use doppel_host::capabilities::{Clock, SystemClock};
use doppel_host::machines::CALL_TIMEOUT;
use doppel_machine::deadline::Deadline;

/// Where the daemon that commands go to listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DaemonAt {
    /// The one serving this Mac's store, on its socket in the store.
    Store(PathBuf),
    /// Another Mac's, on a socket forwarded here (`ssh -L`): its machines,
    /// whose displays can only come encoded.
    Forwarded(PathBuf),
}

impl DaemonAt {
    pub(crate) fn socket(&self) -> &Path {
        match self {
            Self::Store(socket) | Self::Forwarded(socket) => socket,
        }
    }

    /// Connects to the daemon here.
    ///
    /// # Errors
    /// No daemon answers (with how to start one, or to forward one), or it
    /// can't be reached.
    pub(crate) fn connect(&self) -> Result<Client<SystemClock>> {
        match Client::connect(self.socket(), SystemClock, within(CALL_TIMEOUT)) {
            Ok(client) => Ok(client),
            Err(error @ ClientError::NoDaemon(_)) => match self {
                Self::Store(_) => bail!(
                    "{error}: run `doppel daemon install` once to have launchd start it when needed, or `doppel daemon run` to run it in the foreground"
                ),
                Self::Forwarded(socket) => bail!(
                    "{error}: nothing listens on {}; forward the other Mac's daemon there first, as in `ssh -N -L {}:<its data directory>/doppeld.socket <host>`",
                    socket.display(),
                    socket.display()
                ),
            },
            Err(error) => Err(anyhow::Error::new(error)
                .context("the daemon didn't answer (`doppel daemon logs` shows why)")),
        }
    }
}

/// How long the client waits for a lifecycle request. The daemon enforces
/// each operation's own, shorter budget (a checkpoint's save, a start's
/// wait for the desktop); this only bounds a daemon that hangs.
pub(crate) const LIFECYCLE: Duration = Duration::from_secs(15 * 60);

/// The deadline `budget` from now.
pub(crate) fn within(budget: Duration) -> Deadline {
    Deadline::after(SystemClock.now(), budget)
}
