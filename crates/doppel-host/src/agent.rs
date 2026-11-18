//! Client for `doppel-agent` over a connected stream: the vsock socket the VMM
//! handed over, or (in host mode, later) a unix socket.
//!
//! Typed requests over a [`Channel`]; every reply is parsed into core types.
//! Every request carries a [`Deadline`].

use std::io;
use std::num::NonZeroUsize;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::agent_install::plan::AgentBuild;
use doppel_machine::agent::{AgentStatus, GuestEvent};
use doppel_machine::ax::tree::{ActOn, AxSnapshot, Retention, SnapshotRequest};
use doppel_machine::deadline::Deadline;
use doppel_machine::exec::{ExecOutput, ExecRequest};
use doppel_machine::files::{ReadRequest, WriteRequest};
use doppel_machine::input::Input;
use doppel_machine::outcome::{Certainty, Failure};
use doppel_machine::screen::Screenshot;
use doppel_machine::screen_probe::Probe;
use doppel_protocol::convert::{ParseError, RemoteError};
use doppel_protocol::exchange::{EventLimit, ExchangeConfig, ExchangeError};
use doppel_protocol::frame::{EncodeError, StreamError};
use doppel_protocol::pb::agent::v1 as agent;
use doppel_protocol::pb::common::v1 as common;
use doppel_protocol::transport::{Channel, ChannelError, Waited, Wakeup};

use crate::capabilities::Clock;
use doppel_protocol::transport::Cancel;

/// How long past an exec's own timeout to wait for the agent's reply
/// (the agent stops the program at its timeout, then needs a moment to reply).
pub const EXEC_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent speaks protocol v{theirs}; this build speaks v{ours}")]
    VersionMismatch { ours: u32, theirs: u32 },
    #[error("agent connection failed: {0}")]
    Io(#[source] io::Error),
    #[error("agent closed the connection")]
    Closed,
    #[error("timed out waiting for the agent")]
    Timeout,
    #[error(transparent)]
    Stream(#[from] StreamError),
    #[error(transparent)]
    Encode(#[from] EncodeError),
    #[error("agent sent an unexpected message: {0}")]
    Protocol(&'static str),
    #[error("agent sent an invalid reply: {0}")]
    Parse(#[from] ParseError),
    #[error("agent refused: {0}")]
    Remote(#[from] RemoteError),
    /// The caller cancelled; the agent stops the program when the channel closes.
    #[error("cancelled")]
    Cancelled,
    /// A look was asked of an agent that would keep it, pushing out the
    /// snapshots an AI agent acts on: nothing was sent.
    #[error(
        "the guest's agent is too old to look without disturbing the agents driving it; update it with `doppel agent install`"
    )]
    CannotObserve,
}

/// As for the VMM: once a request may have reached the agent, a broken or
/// unreadable exchange leaves its effect unknown (a click may have landed).
impl Failure for AgentError {
    fn certainty(&self) -> Certainty {
        match self {
            Self::Io(_)
            | Self::Closed
            | Self::Timeout
            | Self::Stream(_)
            | Self::Protocol(_)
            | Self::Parse(_)
            | Self::Cancelled => Certainty::MaybeDone,
            Self::VersionMismatch { .. } | Self::Encode(_) | Self::CannotObserve => {
                Certainty::NotDone
            }
            Self::Remote(error) => error.certainty(),
        }
    }
}

impl AgentError {
    /// A failure of the connection itself, which a later attempt may not repeat:
    /// the agent restarting, or a guest that has just resumed (its first vsock
    /// connections fail with `EINVAL`, seen live). A peer that answers wrongly
    /// is not transient.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Io(_) | Self::Closed | Self::Timeout => true,
            Self::VersionMismatch { .. }
            | Self::Stream(_)
            | Self::Encode(_)
            | Self::Protocol(_)
            | Self::Parse(_)
            | Self::Remote(_)
            | Self::Cancelled
            | Self::CannotObserve => false,
        }
    }
}

/// Each way a channel fails is one of the agent client's.
impl From<ChannelError> for AgentError {
    fn from(error: ChannelError) -> Self {
        match error {
            ChannelError::Io(error) => Self::Io(error),
            ChannelError::Closed => Self::Closed,
            ChannelError::Timeout => Self::Timeout,
            ChannelError::Encode(error) => Self::Encode(error),
            ChannelError::Exchange(ExchangeError::Stream(error)) => Self::Stream(error),
            ChannelError::Exchange(ExchangeError::VersionMismatch { ours, theirs }) => {
                Self::VersionMismatch { ours, theirs }
            }
            ChannelError::Exchange(ExchangeError::Unexpected(why)) => Self::Protocol(why),
            // Only a channel shared between callers is lost this way; this
            // client owns its own.
            ChannelError::Lost(why) => Self::Io(io::Error::other(why)),
            ChannelError::Cancelled => Self::Cancelled,
        }
    }
}

type Outcome = agent::agent_response::Outcome;
type Operation = agent::agent_request::Operation;

/// One agent connection. Short-lived ones borrow a clock (`AgentClient<&C>`);
/// a session kept between calls owns one.
#[derive(Debug)]
pub struct AgentClient<C> {
    channel: Channel<agent::AgentFrame, C>,
    /// From the agent's `Hello`; `None` for a bare binary.
    peer_build: Option<AgentBuild>,
    pushes: Pushes,
    /// Whether it answers a look without keeping it.
    observes: Observes,
    probes: Probes,
}

/// Whether the agent can push events (its `Hello` lists the capability).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushes {
    Supported,
    /// An older agent: waits fall back to polling alone.
    Unsupported,
}

/// Whether the agent answers a snapshot with [`Retention::Observation`]
/// without keeping it (its `Hello` lists the capability). An older agent
/// would keep it, so it is never asked for one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observes {
    Supported,
    Unsupported,
}

/// Whether the agent draws a screen probe (its `Hello` lists the
/// capability). An older agent, as a checkpoint keeps it, doesn't.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probes {
    Supported,
    Unsupported,
}

/// Events are hints that the guest changed; past this many unread, the
/// oldest go. A reader looks at the guest again, so none is essential.
const MAX_QUEUED_EVENTS: NonZeroUsize = match NonZeroUsize::new(64) {
    Some(most) => most,
    None => NonZeroUsize::MIN,
};

impl<C: Clock> AgentClient<C> {
    /// Performs the `Hello` exchange (the agent speaks first).
    ///
    /// # Errors
    /// Timeout, closed stream, or protocol version mismatch.
    pub fn connect(stream: UnixStream, clock: C, by: Deadline) -> Result<Self, AgentError> {
        let config = ExchangeConfig {
            events: EventLimit::Newest(MAX_QUEUED_EVENTS),
            ..ExchangeConfig::STRICT
        };
        let channel = Channel::open(stream, clock, config, by)?;
        let hello = channel.peer();
        let offers =
            |capability: common::Capability| hello.capabilities.contains(&(capability as i32));
        let pushes = if offers(common::Capability::Events) {
            Pushes::Supported
        } else {
            Pushes::Unsupported
        };
        let observes = if offers(common::Capability::AxObserve) {
            Observes::Supported
        } else {
            Observes::Unsupported
        };
        let probes = if offers(common::Capability::ScreenProbe) {
            Probes::Supported
        } else {
            Probes::Unsupported
        };
        let peer_build = match hello.build_id.as_str() {
            "" => None,
            raw => Some(
                raw.parse()
                    .map_err(|_| AgentError::Protocol("invalid build_id in Hello"))?,
            ),
        };
        Ok(Self {
            channel,
            peer_build,
            pushes,
            observes,
            probes,
        })
    }

    /// The installed build this agent says it is; `None` for a bare binary.
    #[must_use]
    pub const fn peer_build(&self) -> Option<&AgentBuild> {
        self.peer_build.as_ref()
    }

    #[must_use]
    pub const fn pushes(&self) -> Pushes {
        self.pushes
    }

    #[must_use]
    pub const fn probes(&self) -> Probes {
        self.probes
    }

    /// Draws `probe` above every window, or moves the one shown. Check
    /// [`Self::probes`] first: an agent without them refuses.
    ///
    /// # Errors
    /// See [`AgentError`].
    pub fn show_probe(&mut self, probe: Probe, by: Deadline) -> Result<(), AgentError> {
        self.expect_done(Operation::ShowProbe(probe.into()), by)
    }

    /// Removes the probe, if one shows.
    ///
    /// # Errors
    /// See [`AgentError`].
    pub fn hide_probe(&mut self, by: Deadline) -> Result<(), AgentError> {
        self.expect_done(Operation::HideProbe(agent::HideProbe {}), by)
    }

    fn expect_done(&mut self, operation: Operation, by: Deadline) -> Result<(), AgentError> {
        match self.call(operation, by)? {
            Outcome::Done(_) => Ok(()),
            Outcome::Error(_)
            | Outcome::Status(_)
            | Outcome::Image(_)
            | Outcome::AxSnapshot(_)
            | Outcome::FileContents(_)
            | Outcome::ExecResult(_) => Err(AgentError::Protocol("expected Done")),
        }
    }

    /// Whether the connection still looks usable: the agent hasn't closed
    /// it (the guest stopped, the agent restarted). Doesn't wait or read.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.channel.is_open()
    }

    /// Asks the agent to push [`GuestEvent`]s on this connection, if it can.
    ///
    /// # Errors
    /// See [`AgentError`].
    pub fn subscribe(&mut self, by: Deadline) -> Result<Pushes, AgentError> {
        match self.pushes {
            Pushes::Unsupported => return Ok(Pushes::Unsupported),
            Pushes::Supported => {}
        }
        match self.call(Operation::Subscribe(agent::Subscribe {}), by)? {
            Outcome::Done(_) => Ok(Pushes::Supported),
            Outcome::Error(_)
            | Outcome::Status(_)
            | Outcome::Image(_)
            | Outcome::AxSnapshot(_)
            | Outcome::FileContents(_)
            | Outcome::ExecResult(_) => Err(AgentError::Protocol("expected Done")),
        }
    }

    /// The next pushed event: one already received, or the next to arrive
    /// before `until`. `None` once `until` passes with none.
    ///
    /// # Errors
    /// See [`AgentError`]; a response nobody asked for is a protocol error.
    pub fn next_event(&mut self, until: Deadline) -> Result<Option<GuestEvent>, AgentError> {
        Ok(self.channel.next_event(until)?.map(GuestEvent::from))
    }

    /// The next pushed event before `until`, unless `wakeup` is woken
    /// first (see [`Channel::wait_event`]).
    ///
    /// # Errors
    /// See [`AgentError`].
    pub fn wait_event(
        &mut self,
        until: Deadline,
        wakeup: &Wakeup,
    ) -> Result<Waited<GuestEvent>, AgentError> {
        Ok(match self.channel.wait_event(Some(until), wakeup)? {
            Waited::Event(event) => Waited::Event(GuestEvent::from(event)),
            Waited::TimedOut => Waited::TimedOut,
            Waited::WokenUp => Waited::WokenUp,
        })
    }

    /// Forgets events received so far: they predate whatever was just
    /// observed (they arrived before its reply).
    pub fn discard_events(&mut self) {
        self.channel.discard_events();
    }

    /// Sends a request and waits for its response. Events pushed meanwhile
    /// are queued for [`Self::next_event`].
    fn call(&mut self, operation: Operation, by: Deadline) -> Result<Outcome, AgentError> {
        match self.channel.call(operation, by)?.response.outcome {
            Some(Outcome::Error(error)) => Err(RemoteError::from(error).into()),
            Some(outcome) => Ok(outcome),
            None => Err(AgentError::Protocol("response without outcome")),
        }
    }

    /// # Errors
    /// See [`AgentError`].
    pub fn status(&mut self, by: Deadline) -> Result<AgentStatus, AgentError> {
        match self.call(Operation::GetStatus(agent::GetStatus {}), by)? {
            Outcome::Status(status) => Ok(status.try_into()?),
            Outcome::Error(_)
            | Outcome::Image(_)
            | Outcome::Done(_)
            | Outcome::AxSnapshot(_)
            | Outcome::FileContents(_)
            | Outcome::ExecResult(_) => Err(AgentError::Protocol("expected Status")),
        }
    }

    /// Captures the guest's main display as PNG, downscaled so the longest
    /// side is at most `max_dimension_pixels` (0 = native).
    ///
    /// # Errors
    /// See [`AgentError`]; a missing Screen Recording grant is `Remote`.
    pub fn capture(
        &mut self,
        max_dimension_pixels: u32,
        by: Deadline,
    ) -> Result<Screenshot, AgentError> {
        let request = agent::CaptureScreen {
            encoding: common::ImageEncoding::Png as i32,
            max_dimension_pixels,
        };
        match self.call(Operation::CaptureScreen(request), by)? {
            Outcome::Image(image) => Ok(image.try_into()?),
            Outcome::Error(_)
            | Outcome::Status(_)
            | Outcome::Done(_)
            | Outcome::AxSnapshot(_)
            | Outcome::FileContents(_)
            | Outcome::ExecResult(_) => Err(AgentError::Protocol("expected Image")),
        }
    }

    /// # Errors
    /// See [`AgentError`]; a missing Accessibility grant is `Remote`.
    pub fn input(&mut self, input: &Input, by: Deadline) -> Result<(), AgentError> {
        match self.call(Operation::Input(input.into()), by)? {
            Outcome::Done(_) => Ok(()),
            Outcome::Error(_)
            | Outcome::Status(_)
            | Outcome::Image(_)
            | Outcome::AxSnapshot(_)
            | Outcome::FileContents(_)
            | Outcome::ExecResult(_) => Err(AgentError::Protocol("expected Done")),
        }
    }
}

impl<C: Clock> AgentClient<C> {
    /// Takes an AX snapshot and returns it as the agent sent it, for recording fixtures.
    ///
    /// # Errors
    /// See [`AgentError`].
    pub fn ax_snapshot_wire(
        &mut self,
        request: &SnapshotRequest,
        by: Deadline,
    ) -> Result<agent::AxSnapshot, AgentError> {
        match (request.retention, self.observes) {
            (Retention::Observation, Observes::Unsupported) => {
                return Err(AgentError::CannotObserve);
            }
            (Retention::Observation, Observes::Supported)
            | (Retention::KeptForActions, Observes::Supported | Observes::Unsupported) => {}
        }
        match self.call(
            Operation::TakeAxSnapshot(agent::TakeAxSnapshot::in_millis(request)),
            by,
        )? {
            Outcome::AxSnapshot(snapshot) => Ok(snapshot),
            Outcome::Error(_)
            | Outcome::Status(_)
            | Outcome::Image(_)
            | Outcome::Done(_)
            | Outcome::FileContents(_)
            | Outcome::ExecResult(_) => Err(AgentError::Protocol("expected AxSnapshot")),
        }
    }

    /// # Errors
    /// See [`AgentError`]; a missing Accessibility grant is `Remote`.
    pub fn ax_snapshot(
        &mut self,
        request: &SnapshotRequest,
        by: Deadline,
    ) -> Result<AxSnapshot, AgentError> {
        Ok(self.ax_snapshot_wire(request, by)?.try_into()?)
    }

    /// Runs a program in the guest. The reply is due when the program has
    /// ended: by its own timeout, after which the agent stops it, plus
    /// [`EXEC_GRACE`] for the agent to answer.
    ///
    /// # Errors
    /// See [`AgentError`]; a missing program is `Remote(InvalidArgument)`.
    ///
    /// Cancelling closes this channel: the agent sees the hang-up and stops
    /// the program, and this returns [`AgentError::Cancelled`].
    pub fn exec(
        &mut self,
        request: &ExecRequest,
        cancel: &Cancel,
    ) -> Result<ExecOutput, AgentError> {
        let by = Deadline::after(self.channel.clock().now(), request.timeout() + EXEC_GRACE);
        let hang_up = self.channel.interrupter()?;
        let _hang_up = cancel.on_cancel(move || hang_up.interrupt());
        let reply = match self.call(Operation::Exec(agent::Exec::in_millis(request)), by) {
            Err(_) if cancel.is_cancelled() => return Err(AgentError::Cancelled),
            reply => reply?,
        };
        match reply {
            Outcome::ExecResult(result) => Ok(result.try_into()?),
            Outcome::Error(_)
            | Outcome::Status(_)
            | Outcome::Image(_)
            | Outcome::Done(_)
            | Outcome::FileContents(_)
            | Outcome::AxSnapshot(_) => Err(AgentError::Protocol("expected ExecResult")),
        }
    }

    /// # Errors
    /// See [`AgentError`]; a missing or oversized file is `Remote(InvalidArgument)`.
    pub fn read_file(
        &mut self,
        request: &ReadRequest,
        by: Deadline,
    ) -> Result<Vec<u8>, AgentError> {
        match self.call(Operation::ReadFile(request.into()), by)? {
            Outcome::FileContents(contents) => {
                let limit = usize::try_from(request.max_bytes()).unwrap_or(usize::MAX);
                if contents.data.len() > limit {
                    return Err(AgentError::Protocol("agent sent more than max_bytes"));
                }
                Ok(contents.data.to_vec())
            }
            Outcome::Error(_)
            | Outcome::Status(_)
            | Outcome::Image(_)
            | Outcome::Done(_)
            | Outcome::AxSnapshot(_)
            | Outcome::ExecResult(_) => Err(AgentError::Protocol("expected FileContents")),
        }
    }

    /// # Errors
    /// See [`AgentError`]; an existing file under `CreateNew` is `Remote(InvalidState)`.
    pub fn write_file(&mut self, request: &WriteRequest, by: Deadline) -> Result<(), AgentError> {
        match self.call(Operation::WriteFile(request.into()), by)? {
            Outcome::Done(_) => Ok(()),
            Outcome::Error(_)
            | Outcome::Status(_)
            | Outcome::Image(_)
            | Outcome::FileContents(_)
            | Outcome::AxSnapshot(_)
            | Outcome::ExecResult(_) => Err(AgentError::Protocol("expected Done")),
        }
    }

    /// # Errors
    /// See [`AgentError`]; an element that is gone is `Remote(StaleReference)`.
    pub fn act(&mut self, act: &ActOn, by: Deadline) -> Result<(), AgentError> {
        match self.call(Operation::PerformAxAction(act.into()), by)? {
            Outcome::Done(_) => Ok(()),
            Outcome::Error(_)
            | Outcome::Status(_)
            | Outcome::Image(_)
            | Outcome::AxSnapshot(_)
            | Outcome::FileContents(_)
            | Outcome::ExecResult(_) => Err(AgentError::Protocol("expected Done")),
        }
    }
}

#[cfg(test)]
mod tests {
    //! The client against a scripted fake agent on a socketpair.
    use std::thread;

    use doppel_machine::geometry::Point;
    use doppel_machine::input::Input;

    use std::io::{Read, Write};

    use doppel_protocol::PROTOCOL_VERSION;
    use doppel_protocol::frame::{FrameDecoder, FrameEncoder, MaxFrameLen, Received};

    use super::*;
    use crate::capabilities::SystemClock;

    fn soon() -> Deadline {
        Deadline::after(SystemClock.now(), Duration::from_secs(2))
    }

    fn frame(kind: agent::agent_frame::Kind) -> Vec<u8> {
        FrameEncoder::new(MaxFrameLen::DEFAULT)
            .encode(&agent::AgentFrame { kind: Some(kind) })
            .unwrap()
            .to_vec()
    }

    fn hello(version: u32) -> Vec<u8> {
        frame(agent::agent_frame::Kind::Hello(common::Hello {
            protocol_version: version,
            ..Default::default()
        }))
    }

    /// Runs `script` as the agent: it gets each decoded request and returns the reply.
    fn fake_agent(
        greeting: Vec<u8>,
        script: impl Fn(agent::AgentRequest) -> Vec<u8> + Send + 'static,
    ) -> UnixStream {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        thread::spawn(move || {
            theirs.write_all(&greeting).unwrap();
            let mut decoder = FrameDecoder::<agent::AgentFrame>::new(MaxFrameLen::DEFAULT);
            let mut buffer = [0u8; 4096];
            loop {
                while let Some(Received::Recognized { message, .. }) = decoder.next_frame().unwrap()
                {
                    if let Some(agent::agent_frame::Kind::Request(request)) = message.kind {
                        theirs.write_all(&script(request)).unwrap();
                    }
                }
                match theirs.read(&mut buffer) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => decoder.push(&buffer[..n]),
                }
            }
        });
        ours
    }

    fn respond(id: u64, outcome: Outcome) -> Vec<u8> {
        frame(agent::agent_frame::Kind::Response(agent::AgentResponse {
            id,
            outcome: Some(outcome),
        }))
    }

    fn hello_pushing() -> Vec<u8> {
        frame(agent::agent_frame::Kind::Hello(common::Hello {
            protocol_version: PROTOCOL_VERSION,
            capabilities: vec![common::Capability::Events as i32],
            ..Default::default()
        }))
    }

    fn ui_changed(pid: i32) -> Vec<u8> {
        frame(agent::agent_frame::Kind::Event(agent::AgentEvent {
            kind: Some(agent::agent_event::Kind::UiChanged(agent::UiChanged {
                pid,
            })),
        }))
    }

    #[test]
    fn a_look_reaches_only_an_agent_that_keeps_nothing_of_it() {
        use doppel_machine::ax::tree::{Menus, Scope};
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent = |capabilities: Vec<i32>| {
            let asked = asked.clone();
            fake_agent(
                frame(agent::agent_frame::Kind::Hello(common::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    capabilities,
                    ..Default::default()
                })),
                move |request| {
                    if let Some(Operation::TakeAxSnapshot(take)) = &request.operation {
                        asked.lock().unwrap().push(take.retention);
                    }
                    respond(
                        request.id,
                        Outcome::AxSnapshot(agent::AxSnapshot::default()),
                    )
                },
            )
        };
        let look = SnapshotRequest::new(Scope::FocusedWindow, Menus::Exclude).observation();
        let mut old = AgentClient::connect(agent(Vec::new()), SystemClock, soon()).unwrap();
        assert!(matches!(
            old.ax_snapshot_wire(&look, soon()),
            Err(AgentError::CannotObserve)
        ));
        let mut new = AgentClient::connect(
            agent(vec![common::Capability::AxObserve as i32]),
            SystemClock,
            soon(),
        )
        .unwrap();
        new.ax_snapshot_wire(&look, soon()).unwrap();
        // The old agent was never asked; the new one was asked not to keep it.
        assert_eq!(
            *asked.lock().unwrap(),
            vec![agent::Retention::Observation as i32]
        );
    }

    #[test]
    fn events_pushed_during_a_call_wait_for_the_reader_and_later_ones_arrive() {
        let stream = fake_agent(hello_pushing(), |request| match request.operation {
            // Subscribed: an event right behind the answer.
            Some(Operation::Subscribe(_)) => [
                respond(request.id, Outcome::Done(agent::Done {})),
                ui_changed(7),
            ]
            .concat(),
            // An event pushed before the answer: queued, not mistaken for it.
            _ => [
                ui_changed(8),
                respond(request.id, Outcome::Done(agent::Done {})),
            ]
            .concat(),
        });
        let mut client = AgentClient::connect(stream, SystemClock, soon()).unwrap();
        assert_eq!(client.subscribe(soon()).unwrap(), Pushes::Supported);
        client.input(&Input::Type("x".into()), soon()).unwrap();
        let ui = |pid| {
            Some(GuestEvent::UiChanged(
                doppel_machine::agent::ProcessId::new(pid),
            ))
        };
        assert_eq!(client.next_event(soon()).unwrap(), ui(7));
        assert_eq!(client.next_event(soon()).unwrap(), ui(8));
        let shortly = Deadline::after(SystemClock.now(), Duration::from_millis(50));
        assert_eq!(client.next_event(shortly).unwrap(), None, "none left");
    }

    #[test]
    fn an_older_agent_is_never_asked_to_push() {
        let stream = fake_agent(hello(PROTOCOL_VERSION), |_| panic!("no request expected"));
        let mut client = AgentClient::connect(stream, SystemClock, soon()).unwrap();
        assert_eq!(client.subscribe(soon()).unwrap(), Pushes::Unsupported);
    }

    #[test]
    fn a_closed_session_is_seen_without_reading() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        theirs.write_all(&hello(PROTOCOL_VERSION)).unwrap();
        let client = AgentClient::connect(ours, SystemClock, soon()).unwrap();
        assert!(client.is_open());
        drop(theirs);
        assert!(!client.is_open());
    }

    /// An exec the agent never answers, until the caller cancels.
    #[test]
    fn cancelling_an_exec_hangs_up_and_says_so_at_once() {
        // The fake agent never replies: it waits for the hang-up.
        let stream = fake_agent(hello(PROTOCOL_VERSION), |_| Vec::new());
        let mut client = AgentClient::connect(stream, SystemClock, soon()).unwrap();
        let request = ExecRequest::new(
            "/bin/sleep".parse().unwrap(),
            vec!["60".into()],
            Duration::from_secs(60),
            1024,
        )
        .unwrap();
        let cancel = Cancel::new();
        let canceller = cancel.clone();
        let started = SystemClock.now();
        let later = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            canceller.cancel();
        });
        assert!(matches!(
            client.exec(&request, &cancel),
            Err(AgentError::Cancelled)
        ));
        later.join().unwrap();
        assert!(SystemClock.now().duration_since(started) < Duration::from_secs(2));
    }

    #[test]
    fn rejects_another_protocol_version() {
        let stream = fake_agent(hello(2), |_| Vec::new());
        assert!(matches!(
            AgentClient::connect(stream, SystemClock, soon()),
            Err(AgentError::VersionMismatch { theirs: 2, .. })
        ));
    }

    #[test]
    fn input_round_trip_and_remote_errors() {
        let stream = fake_agent(hello(PROTOCOL_VERSION), |request| match request.operation {
            Some(Operation::Input(_)) if request.id == 1 => {
                respond(1, Outcome::Done(agent::Done {}))
            }
            _ => respond(
                request.id,
                Outcome::Error(common::Error {
                    kind: Some(common::error::Kind::PermissionMissing(
                        common::PermissionMissing {
                            permission: common::Permission::ScreenRecording as i32,
                        },
                    )),
                }),
            ),
        });
        let mut client = AgentClient::connect(stream, SystemClock, soon()).unwrap();
        client
            .input(&Input::MoveTo(Point::new(1.0, 2.0).unwrap()), soon())
            .unwrap();
        assert!(matches!(
            client.capture(0, soon()),
            Err(AgentError::Remote(RemoteError::PermissionMissing(
                doppel_machine::agent::Permission::ScreenRecording
            )))
        ));
    }

    #[test]
    fn mismatched_response_id_is_a_protocol_error() {
        let stream = fake_agent(hello(PROTOCOL_VERSION), |request| {
            respond(request.id + 7, Outcome::Done(agent::Done {}))
        });
        let mut client = AgentClient::connect(stream, SystemClock, soon()).unwrap();
        assert!(matches!(
            client.input(&Input::Type("x".into()), soon()),
            Err(AgentError::Protocol(_))
        ));
    }

    #[test]
    fn snapshots_parse_and_stale_actions_are_typed() {
        use doppel_machine::ax::tree::{
            ActionName, ElementAction, ElementTarget, Menus, NodeIndex, Scope, SnapshotId,
        };
        let stream = fake_agent(hello(PROTOCOL_VERSION), |request| match request.operation {
            Some(Operation::TakeAxSnapshot(_)) => respond(
                request.id,
                Outcome::AxSnapshot(agent::AxSnapshot {
                    snapshot_id: 11,
                    app: Some(agent::RunningApp::default()),
                    nodes: vec![agent::AxNode {
                        role: "AXWindow".into(),
                        ..Default::default()
                    }],
                    truncation: agent::Truncation::Complete as i32,
                }),
            ),
            Some(Operation::PerformAxAction(act)) => respond(
                request.id,
                Outcome::Error(common::Error {
                    kind: Some(common::error::Kind::StaleReference(
                        common::StaleReference {
                            snapshot_id: act.snapshot_id,
                            node_index: act.node_index,
                        },
                    )),
                }),
            ),
            _ => respond(request.id, Outcome::Done(agent::Done {})),
        });
        let mut client = AgentClient::connect(stream, SystemClock, soon()).unwrap();
        let snap = client
            .ax_snapshot(
                &SnapshotRequest::new(Scope::FocusedWindow, Menus::Exclude),
                soon(),
            )
            .unwrap();
        assert_eq!(snap.id, SnapshotId::new(11));
        let act = ActOn {
            target: ElementTarget {
                snapshot: snap.id,
                node: NodeIndex::ROOT,
            },
            action: ElementAction::Perform(ActionName::Press),
        };
        assert!(matches!(
            client.act(&act, soon()),
            Err(AgentError::Remote(RemoteError::StaleReference {
                snapshot_id: 11,
                node_index: 0
            }))
        ));
    }

    #[test]
    fn invalid_status_is_rejected_at_the_boundary() {
        let stream = fake_agent(hello(PROTOCOL_VERSION), |request| {
            respond(request.id, Outcome::Status(agent::AgentStatus::default()))
        });
        let mut client = AgentClient::connect(stream, SystemClock, soon()).unwrap();
        assert!(matches!(client.status(soon()), Err(AgentError::Parse(_))));
    }

    /// A reply that trickles in a byte at a time keeps every single read
    /// short, so a per-read timeout never fires; the call's deadline does.
    #[test]
    fn a_trickling_reply_ends_at_the_call_deadline() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        thread::spawn(move || {
            theirs.write_all(&hello(PROTOCOL_VERSION)).unwrap();
            let mut buffer = [0u8; 4096];
            // Wait for the request, then answer it very slowly.
            if theirs.read(&mut buffer).is_err() {
                return;
            }
            let reply = respond(2, Outcome::Done(agent::Done {}));
            for byte in reply {
                thread::sleep(Duration::from_millis(50));
                if theirs.write_all(&[byte]).is_err() {
                    return;
                }
            }
        });
        let clock = SystemClock;
        let mut client = AgentClient::connect(ours, &clock, soon()).unwrap();
        let started = clock.now();
        let by = Deadline::after(started, Duration::from_millis(300));
        assert!(matches!(
            client.input(&Input::Type("x".into()), by),
            Err(AgentError::Timeout)
        ));
        let took = clock.now().saturating_duration_since(started);
        assert!(took < Duration::from_millis(600), "took {took:?}");
    }

    #[test]
    fn a_passed_deadline_fails_without_waiting() {
        let stream = fake_agent(hello(PROTOCOL_VERSION), |request| {
            respond(request.id, Outcome::Done(agent::Done {}))
        });
        let clock = SystemClock;
        let mut client = AgentClient::connect(stream, &clock, soon()).unwrap();
        let passed = Deadline::after(clock.now(), Duration::ZERO);
        assert!(matches!(
            client.input(&Input::Type("x".into()), passed),
            Err(AgentError::Timeout)
        ));
    }
}
