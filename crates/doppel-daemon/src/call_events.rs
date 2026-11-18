//! What happens to a session's calls, told once to everything that keeps
//! account of them: the consoles watching (`activity::Watchers`), the
//! session's recording (`Recordings`), and where recent snapshots'
//! elements are (`activity::Elements`).
//!
//! The request code publishes a [`CallEvent`] at each moment of a call;
//! [`Daemon::publish`] hands it to each of them, in a fixed order, on the
//! call's own thread. Each decides for itself what the event means to it.

use std::time::{Duration, Instant};

use doppel_host::capabilities::Clock;
use doppel_host::files::Filesystem;
use doppel_machine::machine::MachineName;
use doppel_protocol::pb::agent::v1 as agent;
use doppel_protocol::pb::daemon::v1 as daemon;

use crate::Daemon;
use crate::handle::Guest;
use crate::recording::Traced;
use crate::scheduler::SessionId;

/// One call of a session, as its events describe it: fixed when it is
/// cleared to run.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionCall {
    pub(crate) session: SessionId,
    /// The request's id on its connection.
    pub(crate) request: u64,
    /// Who drives the session, in its own words.
    pub(crate) client: String,
    pub(crate) caption: String,
    /// The machine it names.
    pub(crate) machine: Option<MachineName>,
    /// What it does on the machine's screen, and where; with its machine.
    pub(crate) acted: Option<daemon::CallStarted>,
    /// What it looks for in the screen's text, if it reads it.
    pub(crate) looked_for: Option<String>,
    /// What it may do to the machine's guest process.
    pub(crate) guest: Guest,
}

/// What a call's answer says, for those keeping account.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Answered<'a> {
    pub(crate) traced: Traced,
    /// The snapshot it saw, password values included: each consumer
    /// decides what to show.
    pub(crate) seen: Option<&'a agent::AxSnapshot>,
    /// What it read off the screen, and where what it looked for was.
    pub(crate) read: Option<daemon::TextRead>,
}

/// A moment in a session call's life.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CallEvent<'a> {
    /// It is cleared to run, and about to.
    Running { call: &'a SessionCall },
    /// It got its first machine turn, at `at`.
    TurnBegan { call: &'a SessionCall, at: Instant },
    /// It was answered.
    Ended {
        call: &'a SessionCall,
        /// When it began running.
        ran_from: Instant,
        /// When it got its first machine turn, if it took one.
        began: Option<Instant>,
        ended: Instant,
        /// How long its session or machine held it up before it ran.
        waited: Option<Duration>,
        answered: Answered<'a>,
    },
    /// The session named the elements of a snapshot one of its calls saw.
    Named {
        session: SessionId,
        request: u64,
        client: String,
        machine: MachineName,
        refs: &'a daemon::ElementRefs,
        at: Instant,
    },
}

impl CallEvent<'_> {
    pub(crate) const fn session(&self) -> SessionId {
        match self {
            Self::Running { call } | Self::TurnBegan { call, .. } | Self::Ended { call, .. } => {
                call.session
            }
            Self::Named { session, .. } => *session,
        }
    }
}

impl<C, F> Daemon<C, F>
where
    C: Clock + Clone,
    F: Filesystem,
{
    /// Tells `event` to the session's recording, the element cache, and
    /// the consoles, in that order.
    pub(crate) fn publish(&self, event: &CallEvent<'_>) {
        if let Some(recordings) = self.scheduler.recordings(event.session()) {
            recordings.on_call(event, &self.machines);
        }
        self.elements.on_call(event);
        self.watchers.on_call(event);
    }
}
