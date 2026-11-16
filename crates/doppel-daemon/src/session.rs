//! A machine's agent session, kept between MCP calls: one connection instead
//! of a connect and handshake per call, and the agent's pushed events, which
//! wake a wait as soon as the guest changes instead of at its next backoff.
//!
//! Exec keeps a channel of its own: a long program mustn't hold the session,
//! and cancelling one closes its channel.

use std::time::Instant;

use doppel_host::agent::{AgentClient, AgentError, Pushes};
use doppel_host::capabilities::Clock;
use doppel_host::files::Filesystem;
use doppel_host::machines::{MachineError, Machines};
use doppel_machine::deadline::Deadline;
use doppel_machine::machine::MachineName;
use doppel_protocol::transport::{Cancel, Slept, Waited, Wakeup};

/// The session slot of one machine. It lives in the machine's queue, so
/// only the call whose turn it is uses it.
#[derive(Debug)]
pub(crate) struct Session<C> {
    client: Option<AgentClient<C>>,
}

/// How a [`Session::pause`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Paused {
    /// The time came.
    Due,
    /// The guest reported a change first.
    Changed,
    Cancelled,
}

impl<C> Default for Session<C> {
    fn default() -> Self {
        Self { client: None }
    }
}

impl<C: Clock + Clone> Session<C> {
    /// The open session, connecting (and subscribing) when there is none or
    /// the last one was closed: the guest stopped, the agent restarted.
    pub(crate) fn client<F: Filesystem>(
        &mut self,
        machines: &Machines<C, F>,
        machine: &MachineName,
        by: Deadline,
    ) -> Result<&mut AgentClient<C>, MachineError> {
        let client = match self.client.take() {
            Some(open) if open.is_open() => open,
            Some(_) | None => machines.machine(machine).running(by)?.session(by)?,
        };
        Ok(self.client.insert(client))
    }

    /// Runs one request on the session. A broken exchange ends the session,
    /// so the next call starts afresh; a refusal from the agent keeps it.
    pub(crate) fn run<F: Filesystem, T>(
        &mut self,
        machines: &Machines<C, F>,
        machine: &MachineName,
        by: Deadline,
        request: impl FnOnce(&mut AgentClient<C>, Deadline) -> Result<T, AgentError>,
    ) -> Result<T, SessionError> {
        let result = request(self.client(machines, machine, by)?, by);
        match &result {
            // Refused before it was sent: the connection is fine.
            Ok(_) | Err(AgentError::Remote(_) | AgentError::CannotObserve) => {}
            Err(
                AgentError::VersionMismatch { .. }
                | AgentError::Io(_)
                | AgentError::Closed
                | AgentError::Timeout
                | AgentError::Stream(_)
                | AgentError::Encode(_)
                | AgentError::Protocol(_)
                | AgentError::Parse(_)
                | AgentError::Cancelled,
            ) => self.end(),
        }
        Ok(result?)
    }

    /// Drops the session: the guest is about to change under it (a stop,
    /// a restore), or it failed.
    pub(crate) fn end(&mut self) {
        self.client = None;
    }

    /// Forgets events received so far: they arrived before the reply to the
    /// last request, so that request already saw their effect.
    pub(crate) fn seen(&mut self) {
        if let Some(client) = &mut self.client {
            client.discard_events();
        }
    }

    /// Waits until `at`, ending early if the guest reports a change or the
    /// call is cancelled. Without a session that receives events, it sleeps.
    pub(crate) fn pause(&mut self, clock: &impl Clock, at: Instant, cancel: &Cancel) -> Paused {
        let asleep = |cancel: &Cancel| match cancel.sleep(at.saturating_duration_since(clock.now()))
        {
            Slept::Elapsed => Paused::Due,
            Slept::Cancelled => Paused::Cancelled,
        };
        let client = match &mut self.client {
            Some(client) if client.pushes() == Pushes::Supported => client,
            Some(_) | None => return asleep(cancel),
        };
        // A cancel wakes the wait for events; without a wakeup, sleep.
        let Ok((wakeup, waker)) = Wakeup::new().and_then(|wakeup| {
            let waker = wakeup.waker()?;
            Ok((wakeup, waker))
        }) else {
            return asleep(cancel);
        };
        let _hook = cancel.on_cancel(move || waker.wake());
        let now = clock.now();
        match client.wait_event(
            Deadline::after(now, at.saturating_duration_since(now)),
            &wakeup,
        ) {
            Ok(Waited::Event(_)) => Paused::Changed,
            Ok(Waited::TimedOut) => Paused::Due,
            Ok(Waited::WokenUp) => Paused::Cancelled,
            // The session broke: finish the pause asleep; the next request
            // reconnects.
            Err(_) => {
                self.end();
                asleep(cancel)
            }
        }
    }
}

/// Why a request on the session failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionError {
    #[error(transparent)]
    Connect(#[from] MachineError),
    #[error(transparent)]
    Request(#[from] AgentError),
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use doppel_host::capabilities::SystemClock;
    use doppel_protocol::PROTOCOL_VERSION;
    use doppel_protocol::frame::{FrameEncoder, MaxFrameLen};
    use doppel_protocol::pb::agent::v1 as agent;
    use doppel_protocol::pb::common::v1 as common;

    use super::*;

    fn frame(kind: agent::agent_frame::Kind) -> Vec<u8> {
        FrameEncoder::new(MaxFrameLen::DEFAULT)
            .encode(&agent::AgentFrame { kind: Some(kind) })
            .unwrap()
            .to_vec()
    }

    fn ui_changed() -> Vec<u8> {
        frame(agent::agent_frame::Kind::Event(agent::AgentEvent {
            kind: Some(agent::agent_event::Kind::UiChanged(agent::UiChanged {
                pid: 1,
            })),
        }))
    }

    /// A session on a socketpair; the other end plays the agent.
    fn session(pushes: bool) -> (Session<SystemClock>, UnixStream) {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let capabilities = if pushes {
            vec![common::Capability::Events as i32]
        } else {
            Vec::new()
        };
        theirs
            .write_all(&frame(agent::agent_frame::Kind::Hello(common::Hello {
                protocol_version: PROTOCOL_VERSION,
                capabilities,
                ..Default::default()
            })))
            .unwrap();
        let by = Deadline::after(SystemClock.now(), Duration::from_secs(2));
        let client = AgentClient::connect(ours, SystemClock, by).unwrap();
        (
            Session {
                client: Some(client),
            },
            theirs,
        )
    }

    fn in_secs(secs: u64) -> Instant {
        SystemClock.now() + Duration::from_secs(secs)
    }

    #[test]
    fn a_pushed_change_ends_the_pause_early() {
        let (mut session, mut agent) = session(true);
        let pusher = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            agent.write_all(&ui_changed()).unwrap();
            agent
        });
        let started = SystemClock.now();
        let paused = session.pause(&SystemClock, in_secs(5), &Cancel::never());
        assert_eq!(paused, Paused::Changed);
        assert!(SystemClock.now().duration_since(started) < Duration::from_secs(2));
        drop(pusher.join().unwrap());
    }

    #[test]
    fn a_cancel_ends_the_pause_while_listening() {
        let (mut session, _agent) = session(true);
        let cancel = Cancel::new();
        let canceller = cancel.clone();
        let later = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            canceller.cancel();
        });
        assert_eq!(
            session.pause(&SystemClock, in_secs(5), &cancel),
            Paused::Cancelled
        );
        later.join().unwrap();
    }

    #[test]
    fn without_pushes_a_pause_waits_out_its_time() {
        let (mut session, mut agent) = session(false);
        // Not asked for, so never read as a change.
        agent.write_all(&ui_changed()).unwrap();
        let at = SystemClock.now() + Duration::from_millis(150);
        assert_eq!(
            session.pause(&SystemClock, at, &Cancel::never()),
            Paused::Due
        );
        assert!(SystemClock.now() >= at);
    }

    #[test]
    fn a_closed_session_falls_back_to_sleeping_and_is_dropped() {
        let (mut session, agent) = session(true);
        drop(agent);
        let at = SystemClock.now() + Duration::from_millis(100);
        assert_eq!(
            session.pause(&SystemClock, at, &Cancel::never()),
            Paused::Due
        );
        assert!(session.client.is_none(), "the next request reconnects");
    }
}
