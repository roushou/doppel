//! A caller's request to stop. Long operations take a [`Cancel`] and check
//! it between steps, sleep on it instead of the thread, and hook it to
//! interrupt blocking I/O (closing a socket), so a cancelled call ends
//! promptly instead of running to its deadline.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Shared by the caller, who may cancel, and the work, which watches.
/// Clones watch the same request.
#[derive(Clone, Default)]
pub struct Cancel(Option<Arc<Shared>>);

/// How a [`Cancel::sleep`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Slept {
    Elapsed,
    Cancelled,
}

type HookFn = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    cancelled: bool,
    hooks: BTreeMap<u64, HookFn>,
    next: u64,
}

impl Cancel {
    /// A request nobody can cancel (the CLI's: Ctrl-C ends the process).
    #[must_use]
    pub const fn never() -> Self {
        Self(None)
    }

    #[must_use]
    pub fn new() -> Self {
        Self(Some(Arc::default()))
    }

    /// Cancels the request: wakes sleepers and runs every hook, once.
    pub fn cancel(&self) {
        let Some(shared) = &self.0 else { return };
        let hooks = {
            let mut state = shared.lock();
            if state.cancelled {
                return;
            }
            state.cancelled = true;
            std::mem::take(&mut state.hooks)
        };
        shared.changed.notify_all();
        for hook in hooks.into_values() {
            hook();
        }
    }

    /// Whether `other` watches the same request: it is a clone of this
    /// one. A request nobody can cancel is no one's in particular.
    #[must_use]
    pub fn same_request(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(ours), Some(theirs)) => Arc::ptr_eq(ours, theirs),
            (Some(_) | None, None) | (None, Some(_)) => false,
        }
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|shared| shared.lock().cancelled)
    }

    /// Sleeps for `duration`, or less if the request is cancelled.
    pub fn sleep(&self, duration: Duration) -> Slept {
        let Some(shared) = &self.0 else {
            std::thread::sleep(duration);
            return Slept::Elapsed;
        };
        let state = shared.lock();
        let (state, _) = shared
            .changed
            .wait_timeout_while(state, duration, |state| !state.cancelled)
            .unwrap_or_else(PoisonError::into_inner);
        if state.cancelled {
            Slept::Cancelled
        } else {
            Slept::Elapsed
        }
    }

    /// Runs `hook` if the request is cancelled while the returned [`Hook`]
    /// lives; at once if it already is.
    pub fn on_cancel(&self, hook: impl FnOnce() + Send + 'static) -> Hook {
        let Some(shared) = &self.0 else {
            return Hook(None);
        };
        let mut state = shared.lock();
        if state.cancelled {
            drop(state);
            hook();
            return Hook(None);
        }
        let id = state.next;
        state.next += 1;
        state.hooks.insert(id, Box::new(hook));
        Hook(Some((Arc::clone(shared), id)))
    }
}

impl Shared {
    /// The state is a flag and a hook list: valid after any panic.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for Cancel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            None => f.write_str("Cancel::never"),
            Some(shared) => f
                .debug_struct("Cancel")
                .field("cancelled", &shared.lock().cancelled)
                .finish(),
        }
    }
}

/// A registered [`Cancel::on_cancel`] hook; dropping it unregisters it.
#[must_use = "dropping the hook unregisters it at once"]
pub struct Hook(Option<(Arc<Shared>, u64)>);

impl Drop for Hook {
    fn drop(&mut self) {
        if let Some((shared, id)) = &self.0 {
            shared.lock().hooks.remove(id);
        }
    }
}

impl fmt::Debug for Hook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Hook")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn a_cancel_wakes_a_sleeper_early() {
        let cancel = Cancel::new();
        let other = cancel.clone();
        let sleeper = std::thread::spawn(move || other.sleep(Duration::from_secs(30)));
        std::thread::sleep(Duration::from_millis(50));
        cancel.cancel();
        assert_eq!(sleeper.join().unwrap(), Slept::Cancelled);
        assert_eq!(cancel.sleep(Duration::from_secs(30)), Slept::Cancelled);
    }

    #[test]
    fn hooks_run_once_and_only_while_held() {
        let runs = Arc::new(AtomicUsize::new(0));
        let count = |runs: &Arc<AtomicUsize>| {
            let runs = Arc::clone(runs);
            move || {
                runs.fetch_add(1, Ordering::SeqCst);
            }
        };
        let cancel = Cancel::new();
        drop(cancel.on_cancel(count(&runs)));
        let _held = cancel.on_cancel(count(&runs));
        cancel.cancel();
        cancel.cancel();
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the dropped hook never ran");
        // Registered after the cancel: runs at once.
        let _late = cancel.on_cancel(count(&runs));
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn never_is_never_cancelled() {
        let never = Cancel::never();
        never.cancel();
        assert!(!never.is_cancelled());
        assert_eq!(never.sleep(Duration::ZERO), Slept::Elapsed);
    }
}
