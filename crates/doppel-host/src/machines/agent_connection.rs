//! A machine's connection to its guest agent, kept between calls: one
//! connection instead of a connect and handshake per call, and the agent's
//! pushed events, which wake a wait as soon as the guest changes instead of
//! at its next backoff.
//!
//! Exec keeps a channel of its own: a long program mustn't hold the
//! connection, and cancelling one closes its channel.

use std::time::Instant;

use doppel_desktop::deadline::Deadline;
use doppel_machine::machine::MachineName;
use doppel_protocol::transport::{Cancel, Slept, Waited, Wakeup};

use crate::agent::{AgentClient, AgentError, Pushes};
use crate::capabilities::Clock;
use crate::files::Filesystem;

use super::{MachineError, Machines};

/// A machine's connection to its guest agent: opened when first used, and
/// again after it broke (the guest stopped, the agent restarted). It holds
/// at most one open [`AgentClient`].
#[derive(Debug)]
pub struct AgentConnection<C> {
    client: Option<AgentClient<C>>,
}

/// How an [`AgentConnection::pause`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paused {
    /// The time came.
    Due,
    /// The guest reported a change first.
    Changed,
    Cancelled,
}

impl<C> Default for AgentConnection<C> {
    fn default() -> Self {
        Self { client: None }
    }
}

impl<C: Clock + Clone> AgentConnection<C> {
    /// The open connection, connecting (and subscribing) when there is none
    /// or the last one was closed.
    ///
    /// # Errors
    /// The machine isn't running, or its agent can't be reached.
    pub fn client<F: Filesystem>(
        &mut self,
        machines: &Machines<C, F>,
        machine: &MachineName,
        by: Deadline,
    ) -> Result<&mut AgentClient<C>, MachineError> {
        let client = match self.client.take() {
            Some(open) if open.is_open() => open,
            Some(_) | None => machines.machine(machine).running(by)?.connect_agent(by)?,
        };
        Ok(self.client.insert(client))
    }

    /// Runs one request on the connection. A broken exchange drops it, so
    /// the next call connects afresh; a refusal from the agent keeps it.
    ///
    /// # Errors
    /// See [`AgentConnectionError`].
    pub fn run<F: Filesystem, T>(
        &mut self,
        machines: &Machines<C, F>,
        machine: &MachineName,
        by: Deadline,
        request: impl FnOnce(&mut AgentClient<C>, Deadline) -> Result<T, AgentError>,
    ) -> Result<T, AgentConnectionError> {
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
            ) => self.close(),
        }
        Ok(result?)
    }

    /// Drops the connection: the guest is about to change under it (a
    /// stop, a restore), or it failed.
    pub fn close(&mut self) {
        self.client = None;
    }

    /// Forgets events received so far: they arrived before the reply to the
    /// last request, so that request already saw their effect.
    pub fn seen(&mut self) {
        if let Some(client) = &mut self.client {
            client.discard_events();
        }
    }

    /// Waits until `at`, ending early if the guest reports a change or the
    /// call is cancelled. Without a connection that receives events, it
    /// sleeps.
    pub fn pause(&mut self, clock: &impl Clock, at: Instant, cancel: &Cancel) -> Paused {
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
            // The connection broke: finish the pause asleep; the next
            // request reconnects.
            Err(_) => {
                self.close();
                asleep(cancel)
            }
        }
    }
}

/// Why a request on an [`AgentConnection`] failed.
#[derive(Debug, thiserror::Error)]
pub enum AgentConnectionError {
    /// It couldn't connect.
    #[error(transparent)]
    Connect(#[from] MachineError),
    /// The agent refused the request, or the exchange broke.
    #[error(transparent)]
    Request(#[from] AgentError),
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use crate::capabilities::SystemClock;
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

    /// A connection on a socketpair; the other end plays the agent.
    fn connection(pushes: bool) -> (AgentConnection<SystemClock>, UnixStream) {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let capabilities = if pushes {
            vec![common::Capability::Events as i32]
        } else {
            Vec::new()
        };
        theirs
            .write_all(&frame(agent::agent_frame::Kind::Hello(common::Hello {
                protocol_version: <doppel_protocol::pb::agent::v1::AgentFrame as doppel_protocol::exchange::Protocol>::VERSION,
                capabilities,
                ..Default::default()
            })))
            .unwrap();
        let by = Deadline::after(SystemClock.now(), Duration::from_secs(2));
        let client = AgentClient::connect(ours, SystemClock, by).unwrap();
        (
            AgentConnection {
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
        let (mut connection, mut agent) = connection(true);
        let pusher = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            agent.write_all(&ui_changed()).unwrap();
            agent
        });
        let started = SystemClock.now();
        let paused = connection.pause(&SystemClock, in_secs(5), &Cancel::never());
        assert_eq!(paused, Paused::Changed);
        assert!(SystemClock.now().duration_since(started) < Duration::from_secs(2));
        drop(pusher.join().unwrap());
    }

    #[test]
    fn a_cancel_ends_the_pause_while_listening() {
        let (mut connection, _agent) = connection(true);
        let cancel = Cancel::new();
        let canceller = cancel.clone();
        let later = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            canceller.cancel();
        });
        assert_eq!(
            connection.pause(&SystemClock, in_secs(5), &cancel),
            Paused::Cancelled
        );
        later.join().unwrap();
    }

    #[test]
    fn without_pushes_a_pause_waits_out_its_time() {
        let (mut connection, mut agent) = connection(false);
        // Not asked for, so never read as a change.
        agent.write_all(&ui_changed()).unwrap();
        let at = SystemClock.now() + Duration::from_millis(150);
        assert_eq!(
            connection.pause(&SystemClock, at, &Cancel::never()),
            Paused::Due
        );
        assert!(SystemClock.now() >= at);
    }

    #[test]
    fn a_closed_connection_falls_back_to_sleeping_and_is_dropped() {
        let (mut connection, agent) = connection(true);
        drop(agent);
        let at = SystemClock.now() + Duration::from_millis(100);
        assert_eq!(
            connection.pause(&SystemClock, at, &Cancel::never()),
            Paused::Due
        );
        assert!(connection.client.is_none(), "the next request reconnects");
    }
}
