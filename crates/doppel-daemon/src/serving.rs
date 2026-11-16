//! Whether the daemon serves or is stopping, and the connections it has
//! open: a [`Stopper`] stops accepting and closes each one, so every
//! session ends as [`SessionEnd::DaemonStopped`] and its recordings are
//! completed before [`crate::Daemon::serve`] returns.

use std::collections::HashMap;
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::scheduler::session::SessionEnd;

use crate::scheduler::ConnectionId;

/// The daemon's side of serving: shared with every [`Stopper`].
#[derive(Debug, Default)]
pub(crate) struct Serving {
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    phase: Phase,
    /// A second handle on each open connection's socket, to close it by.
    open: HashMap<ConnectionId, UnixStream>,
    /// Written once on stop: wakes the accept loop while it waits.
    wake: Option<UnixStream>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Phase {
    #[default]
    Serving,
    Stopping,
}

/// A connection the daemon serves: closed with it when it stops.
#[derive(Debug)]
#[must_use = "dropping it forgets the connection"]
pub(crate) struct Admitted<'a> {
    serving: &'a Serving,
    id: ConnectionId,
}

impl Serving {
    /// Asks the daemon to stop, from any thread.
    pub(crate) fn stopper(&self) -> Stopper {
        Stopper {
            state: Arc::clone(&self.state),
        }
    }

    /// Lets connection `id` on `socket` in, to be closed when the daemon
    /// stops; `None` when it already is stopping.
    pub(crate) fn admit(&self, id: ConnectionId, socket: UnixStream) -> Option<Admitted<'_>> {
        let mut state = self.lock();
        match state.phase {
            Phase::Serving => {
                state.open.insert(id, socket);
                Some(Admitted { serving: self, id })
            }
            Phase::Stopping => None,
        }
    }

    /// Where the accept loop's wake-up is written; `false` when the daemon
    /// is stopping already, and the loop shouldn't start.
    pub(crate) fn wake_with(&self, wake: UnixStream) -> bool {
        let mut state = self.lock();
        match state.phase {
            Phase::Serving => {
                state.wake = Some(wake);
                true
            }
            Phase::Stopping => false,
        }
    }

    /// Whether the daemon is stopping.
    pub(crate) fn stopping(&self) -> bool {
        match self.lock().phase {
            Phase::Serving => false,
            Phase::Stopping => true,
        }
    }

    /// The state is a phase and sockets: usable after any panic.
    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }
}

impl Admitted<'_> {
    /// Why its session ends, now that its connection has.
    pub(crate) fn end(&self) -> SessionEnd {
        match self.serving.lock().phase {
            Phase::Serving => SessionEnd::Disconnected,
            Phase::Stopping => SessionEnd::DaemonStopped,
        }
    }
}

impl Drop for Admitted<'_> {
    fn drop(&mut self) {
        self.serving.lock().open.remove(&self.id);
    }
}

/// Stops a serving daemon: it accepts no more connections and closes
/// those it has, ending their sessions; [`crate::Daemon::serve`] returns
/// once their requests have finished. Stopping twice is stopping once.
#[derive(Debug, Clone)]
pub struct Stopper {
    state: Arc<Mutex<State>>,
}

impl Stopper {
    pub fn stop(&self) {
        let mut state = lock(&self.state);
        state.phase = Phase::Stopping;
        if let Some(wake) = &state.wake {
            // Full or closed: the loop has been woken, or has ended.
            let _ = (&*wake).write_all(&[0]);
        }
        for socket in state.open.values() {
            // Already closed: then it's ending anyway.
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn id() -> ConnectionId {
        crate::scheduler::Waiting::default().connection()
    }

    #[test]
    fn stopping_closes_what_is_open_and_admits_nothing_more() {
        let serving = Serving::default();
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let admitted = serving.admit(id(), ours.try_clone().unwrap()).unwrap();
        assert_eq!(admitted.end(), SessionEnd::Disconnected);
        serving.stopper().stop();
        let mut byte = [0];
        assert_eq!(theirs.read(&mut byte).unwrap(), 0, "closed by the stop");
        assert_eq!(admitted.end(), SessionEnd::DaemonStopped);
        assert!(serving.stopping());
        let (late, _) = UnixStream::pair().unwrap();
        assert!(serving.admit(id(), late).is_none());
    }

    #[test]
    fn a_stop_wakes_the_accept_loop_and_one_before_it_keeps_it_from_starting() {
        let serving = Serving::default();
        let (wake, mut woken) = UnixStream::pair().unwrap();
        assert!(serving.wake_with(wake));
        serving.stopper().stop();
        let mut byte = [9];
        woken.read_exact(&mut byte).unwrap();
        let (wake, _) = UnixStream::pair().unwrap();
        assert!(!serving.wake_with(wake));
    }

    #[test]
    fn a_connection_that_ends_is_forgotten() {
        let serving = Serving::default();
        let (ours, _theirs) = UnixStream::pair().unwrap();
        drop(serving.admit(id(), ours));
        assert!(serving.lock().open.is_empty());
    }
}
