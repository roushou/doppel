//! One client's side of a conversation over frames, sans-IO.
//!
//! An [`Exchange`] is fed the bytes its transport read, with whatever the
//! transport received beside them (file descriptors, for a unix socket), and
//! hands back bytes to write. Between the two it keeps the conversation's
//! rules: the peer greets first and is answered, each request gets an id
//! and its response is filed under it, events pushed meanwhile queue, and
//! anything else ends the conversation with a typed error.
//!
//! Attachments are tied to frames by position in the stream, not by the
//! order they arrived: one received with a chunk belongs to the first frame
//! that starts in or after that chunk. A sender attaches a descriptor to the
//! first byte of the frame it goes with, and the kernel never returns it
//! with bytes sent before it, so a response gets exactly its own
//! descriptors, and a stray one can never be taken for another's.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::num::NonZeroUsize;

use bytes::{Buf, Bytes, BytesMut};

use crate::frame::{
    EncodeError, Envelope, FrameDecoder, FrameEncoder, MaxFrameLen, Received, StreamError,
};
use crate::pb::{agent::v1 as agent, common::v1 as common, daemon::v1 as daemon, vmm::v1 as vmm};

/// Orphans kept at most; see [`Exchange::orphans`].
const MOST_ORPHANS: usize = 16;

/// A request's id on its connection, which its response carries back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(u64);

impl RequestId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A conversation's frame type, seen from the client: what it sends, and
/// what each frame it receives is.
pub trait Protocol: Envelope + Sized {
    /// What a request carries besides its id.
    type Request: fmt::Debug;
    /// A response, its id included.
    type Response: fmt::Debug;
    /// Something the peer pushes without being asked.
    type Event: fmt::Debug;

    /// This channel's wire protocol version, carried in `Hello.protocol_version`.
    /// Peers refuse a different one. Each channel has its own, so a change
    /// to one never strands the peers of another; see each implementation
    /// for when its version may change.
    const VERSION: u32;

    fn hello(hello: common::Hello) -> Self;
    fn request(id: RequestId, request: Self::Request) -> Self;
    fn inbound(self) -> Inbound<Self>;

    /// Asks the peer to stop request `id`, which still answers it; `None`
    /// when the protocol can't ask.
    fn cancel(_id: RequestId) -> Option<Self> {
        None
    }
}

/// What a received frame is to a client.
#[derive(Debug)]
pub enum Inbound<P: Protocol> {
    Hello(common::Hello),
    Response(RequestId, P::Response),
    Event(P::Event),
    /// A frame a client is never sent: why it isn't expected.
    Unexpected(&'static str),
}

/// How many pushed events to keep while no one takes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventLimit {
    /// None: a conversation that asks for no events passes over any.
    Ignored,
    /// Every one: a reader takes them all, in order.
    Unbounded,
    /// The newest this many; older ones go. For events that only hint that
    /// something changed, where a reader looks again anyway.
    Newest(NonZeroUsize),
}

/// What to do with a frame of a kind this build doesn't know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unrecognized {
    /// End the conversation: the peer is built with this one, so it's wrong.
    Fail,
    /// Pass over it: a newer peer may add kinds an older client ignores.
    Skip,
}

/// How an [`Exchange`] keeps its conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExchangeConfig {
    pub limit: MaxFrameLen,
    pub events: EventLimit,
    pub unrecognized: Unrecognized,
}

impl ExchangeConfig {
    /// The default frame limit, every event kept, unknown frames refused.
    pub const STRICT: Self = Self {
        limit: MaxFrameLen::DEFAULT,
        events: EventLimit::Unbounded,
        unrecognized: Unrecognized::Fail,
    };
}

/// Why a conversation can't go on. Once one is returned, the exchange is
/// over, and returns it again for everything asked of it.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ExchangeError {
    #[error(transparent)]
    Stream(#[from] StreamError),
    #[error("the peer speaks protocol v{theirs}; this build speaks v{ours}")]
    VersionMismatch { ours: u32, theirs: u32 },
    /// The peer sent what the conversation doesn't allow.
    #[error("{0}")]
    Unexpected(&'static str),
}

/// A response, with the attachments that came with it.
#[derive(Debug)]
#[must_use]
pub struct Answer<R, A> {
    pub response: R,
    pub attached: Vec<A>,
}

#[derive(Debug)]
enum State {
    /// Waiting for the peer's `Hello`, to answer with ours.
    Greeting,
    Open {
        peer: common::Hello,
    },
    Over(ExchangeError),
}

/// A client's side of one conversation. `A` is what the transport receives
/// beside the bytes; `()` when nothing can be.
#[derive(Debug)]
pub struct Exchange<P: Protocol, A = ()> {
    state: State,
    config: ExchangeConfig,
    decoder: FrameDecoder<P>,
    encoder: FrameEncoder,
    /// Encoded, not yet written.
    outgoing: BytesMut,
    /// Stream bytes received so far.
    received: u64,
    /// Attachments not yet tied to a frame, with the stream offset of the
    /// chunk each came with, in order.
    unclaimed: VecDeque<(u64, A)>,
    next_id: u64,
    /// Sent and not yet answered.
    awaited: HashSet<RequestId>,
    /// Sent, but whose callers gave up: their responses become orphans.
    abandoned: HashSet<RequestId>,
    /// Responses to abandoned requests, newest last, for whoever wants what
    /// they say (a note about the machine, say).
    orphans: VecDeque<P::Response>,
    answers: HashMap<RequestId, Answer<P::Response, A>>,
    events: VecDeque<P::Event>,
}

impl<P: Protocol, A> Exchange<P, A> {
    /// A conversation whose peer speaks first, answered with this build's
    /// protocol and version.
    pub fn new(config: ExchangeConfig) -> Self {
        Self {
            state: State::Greeting,
            config,
            decoder: FrameDecoder::new(config.limit),
            encoder: FrameEncoder::new(config.limit),
            outgoing: BytesMut::new(),
            received: 0,
            unclaimed: VecDeque::new(),
            next_id: 1,
            awaited: HashSet::new(),
            abandoned: HashSet::new(),
            orphans: VecDeque::new(),
            answers: HashMap::new(),
            events: VecDeque::new(),
        }
    }

    /// The peer's greeting, once it has greeted.
    #[must_use]
    pub const fn peer(&self) -> Option<&common::Hello> {
        match &self.state {
            State::Open { peer } => Some(peer),
            State::Greeting | State::Over(_) => None,
        }
    }

    /// Takes in bytes the transport read, and what came with them, and
    /// files every complete frame: an answer, an event, or the greeting
    /// (whose reply is then [`Self::outgoing`]).
    ///
    /// # Errors
    /// The conversation is over: see [`ExchangeError`].
    pub fn receive(
        &mut self,
        bytes: &[u8],
        attached: impl IntoIterator<Item = A>,
    ) -> Result<(), ExchangeError> {
        self.over()?;
        let at = self.received;
        self.unclaimed
            .extend(attached.into_iter().map(|attachment| (at, attachment)));
        self.received += bytes.len() as u64;
        self.decoder.push(bytes);
        loop {
            let start = self.decoder.consumed();
            let frame = match self.decoder.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => return Ok(()),
                Err(error) => return Err(self.end(error.into())),
            };
            let attached = self.claim(start);
            if let Err(error) = self.file(frame, attached) {
                return Err(self.end(error));
            }
        }
    }

    /// Sends a request: its frame joins [`Self::outgoing`], and its response
    /// will be kept for [`Self::answer`] under the id returned.
    ///
    /// # Errors
    /// [`RequestError::Encode`] if it's too large to send, which sends
    /// nothing and leaves the conversation as it was; or the conversation
    /// can't carry it: over, or not yet greeted.
    pub fn request(&mut self, request: P::Request) -> Result<RequestId, RequestError> {
        match &self.state {
            State::Open { .. } => {}
            State::Greeting => return Err(RequestError::NotGreeted),
            State::Over(error) => return Err(RequestError::Over(error.clone())),
        }
        let id = RequestId(self.next_id);
        self.encoder
            .encode_into(&P::request(id, request), &mut self.outgoing)?;
        self.next_id += 1;
        self.awaited.insert(id);
        Ok(id)
    }

    /// Asks the peer to stop request `id`, if the protocol can ask: its
    /// frame joins [`Self::outgoing`], and the response still comes.
    /// `false` when it can't ask, or `id` isn't awaited.
    ///
    /// # Errors
    /// As [`Self::request`].
    pub fn cancel(&mut self, id: RequestId) -> Result<bool, RequestError> {
        if !self.awaited.contains(&id) {
            return Ok(false);
        }
        let Some(frame) = P::cancel(id) else {
            return Ok(false);
        };
        self.encoder.encode_into(&frame, &mut self.outgoing)?;
        Ok(true)
    }

    /// Bytes to write, oldest first.
    #[must_use]
    pub fn outgoing(&self) -> &[u8] {
        &self.outgoing
    }

    /// Takes every byte to write, whole frames only, for a writer that
    /// writes them apart from the exchange.
    pub fn take_outgoing(&mut self) -> Bytes {
        self.outgoing.split().freeze()
    }

    /// The first `written` bytes of [`Self::outgoing`] were written.
    pub fn wrote(&mut self, written: usize) {
        self.outgoing.advance(written.min(self.outgoing.len()));
    }

    /// The response to `id`, once it has come.
    pub fn answer(&mut self, id: RequestId) -> Option<Answer<P::Response, A>> {
        self.answers.remove(&id)
    }

    /// Gives up on `id`: its response, now or later, becomes an orphan.
    pub fn abandon(&mut self, id: RequestId) {
        if let Some(answer) = self.answers.remove(&id) {
            self.orphan(answer.response);
        } else if self.awaited.remove(&id) {
            self.abandoned.insert(id);
        }
    }

    /// The responses to abandoned requests, oldest first.
    pub fn orphans(&mut self) -> impl Iterator<Item = P::Response> + '_ {
        self.orphans.drain(..)
    }

    /// Keeps the newest [`MOST_ORPHANS`]: they matter for what they say,
    /// and only a peer that keeps answering late makes many.
    fn orphan(&mut self, response: P::Response) {
        if self.orphans.len() == MOST_ORPHANS {
            self.orphans.pop_front();
        }
        self.orphans.push_back(response);
    }

    /// The oldest pushed event not yet taken.
    pub fn event(&mut self) -> Option<P::Event> {
        self.events.pop_front()
    }

    /// Forgets the events received so far.
    pub fn discard_events(&mut self) {
        self.events.clear();
    }

    /// The error that ended the conversation, if it has ended.
    ///
    /// # Errors
    /// That error.
    pub fn over(&self) -> Result<(), ExchangeError> {
        match &self.state {
            State::Over(error) => Err(error.clone()),
            State::Greeting | State::Open { .. } => Ok(()),
        }
    }

    fn end(&mut self, error: ExchangeError) -> ExchangeError {
        self.state = State::Over(error.clone());
        error
    }

    /// The attachments of the frame that starts at `start`: those that came
    /// with a chunk starting at or before it, not claimed by an earlier one.
    fn claim(&mut self, start: u64) -> Vec<A> {
        let mut claimed = Vec::new();
        while let Some((at, _)) = self.unclaimed.front()
            && *at <= start
            && let Some((_, attachment)) = self.unclaimed.pop_front()
        {
            claimed.push(attachment);
        }
        claimed
    }

    fn file(&mut self, frame: Received<P>, attached: Vec<A>) -> Result<(), ExchangeError> {
        let message = match frame {
            Received::Recognized { message, .. } => message,
            Received::Unrecognized { .. } => {
                return match self.config.unrecognized {
                    Unrecognized::Fail => Err(ExchangeError::Unexpected(
                        "frame kind unknown to this build",
                    )),
                    Unrecognized::Skip => Ok(()),
                };
            }
            Received::Malformed { .. } => {
                return Err(ExchangeError::Unexpected("malformed frame"));
            }
        };
        let inbound = message.inbound();
        let state = std::mem::replace(
            &mut self.state,
            State::Over(ExchangeError::Unexpected("interrupted")),
        );
        let (state, filed) = match (state, inbound) {
            (State::Greeting, Inbound::Hello(peer)) => self.greeted(peer),
            (
                State::Greeting,
                Inbound::Response(..) | Inbound::Event(_) | Inbound::Unexpected(_),
            ) => (
                State::Over(ExchangeError::Unexpected("expected Hello")),
                Err(ExchangeError::Unexpected("expected Hello")),
            ),
            (State::Open { peer }, Inbound::Response(id, response)) => {
                let filed = self.answered(id, response, attached);
                (State::Open { peer }, filed)
            }
            (State::Open { peer }, Inbound::Event(event)) => {
                self.pushed(event);
                (State::Open { peer }, Ok(()))
            }
            (State::Open { peer }, Inbound::Hello(_)) => (
                State::Open { peer },
                Err(ExchangeError::Unexpected("expected a response")),
            ),
            (State::Open { peer }, Inbound::Unexpected(why)) => {
                (State::Open { peer }, Err(ExchangeError::Unexpected(why)))
            }
            (State::Over(error), _) => (State::Over(error.clone()), Err(error)),
        };
        self.state = state;
        filed
    }

    fn greeted(&mut self, peer: common::Hello) -> (State, Result<(), ExchangeError>) {
        if peer.protocol_version != P::VERSION {
            let error = ExchangeError::VersionMismatch {
                ours: P::VERSION,
                theirs: peer.protocol_version,
            };
            return (State::Over(error.clone()), Err(error));
        }
        // Our own greeting is small and fixed: within any frame limit.
        match self
            .encoder
            .encode_into(&P::hello(greeting(P::VERSION)), &mut self.outgoing)
        {
            Ok(()) => (State::Open { peer }, Ok(())),
            Err(error) => {
                let error = ExchangeError::Unexpected(match error {
                    EncodeError::TooLarge { .. } => "our greeting exceeds the frame limit",
                });
                (State::Over(error.clone()), Err(error))
            }
        }
    }

    fn answered(
        &mut self,
        id: RequestId,
        response: P::Response,
        attached: Vec<A>,
    ) -> Result<(), ExchangeError> {
        if self.awaited.remove(&id) {
            self.answers.insert(id, Answer { response, attached });
            Ok(())
        } else if self.abandoned.remove(&id) {
            self.orphan(response);
            Ok(())
        } else {
            Err(ExchangeError::Unexpected("response for another request"))
        }
    }

    fn pushed(&mut self, event: P::Event) {
        match self.config.events {
            EventLimit::Ignored => return,
            EventLimit::Unbounded => {}
            EventLimit::Newest(most) => {
                while self.events.len() >= most.get() {
                    self.events.pop_front();
                }
            }
        }
        self.events.push_back(event);
    }
}

/// Why a request wasn't sent.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RequestError {
    #[error(transparent)]
    Encode(#[from] EncodeError),
    #[error("the peer hasn't greeted yet")]
    NotGreeted,
    #[error(transparent)]
    Over(ExchangeError),
}

/// Our greeting on a channel of `version`: it and this build's version.
fn greeting(version: u32) -> common::Hello {
    common::Hello {
        protocol_version: version,
        software_version: concat!("doppel ", env!("CARGO_PKG_VERSION")).into(),
        build_id: String::new(),
        capabilities: Vec::new(),
    }
}

impl Protocol for vmm::VmmFrame {
    type Request = vmm::vmm_request::Operation;
    type Response = vmm::VmmResponse;
    type Event = vmm::VmmEvent;

    /// The VMM ships beside `doppel` and is started by it, so both ends
    /// always come from one build: bump on any change an older peer would
    /// misread. A VM started before an upgrade is refused until restarted.
    const VERSION: u32 = 1;

    fn hello(hello: common::Hello) -> Self {
        Self {
            kind: Some(vmm::vmm_frame::Kind::Hello(hello)),
        }
    }

    fn request(id: RequestId, operation: Self::Request) -> Self {
        Self {
            kind: Some(vmm::vmm_frame::Kind::Request(vmm::VmmRequest {
                id: id.get(),
                operation: Some(operation),
            })),
        }
    }

    fn inbound(self) -> Inbound<Self> {
        match self.kind {
            Some(vmm::vmm_frame::Kind::Hello(hello)) => Inbound::Hello(hello),
            Some(vmm::vmm_frame::Kind::Response(response)) => {
                Inbound::Response(RequestId(response.id), response)
            }
            Some(vmm::vmm_frame::Kind::Event(event)) => Inbound::Event(event),
            Some(vmm::vmm_frame::Kind::Request(_)) | None => {
                Inbound::Unexpected("expected a response")
            }
        }
    }
}

impl Protocol for agent::AgentFrame {
    type Request = agent::agent_request::Operation;
    type Response = agent::AgentResponse;
    type Event = agent::AgentEvent;

    /// Golden images keep the agent they were prepared with, and every fork
    /// runs it, so a host must keep talking to agents older than itself.
    /// Grow this channel with new fields and capabilities instead: bumping
    /// it makes every existing golden image unusable until prepared again.
    const VERSION: u32 = 1;

    fn hello(hello: common::Hello) -> Self {
        Self {
            kind: Some(agent::agent_frame::Kind::Hello(hello)),
        }
    }

    fn request(id: RequestId, operation: Self::Request) -> Self {
        Self {
            kind: Some(agent::agent_frame::Kind::Request(agent::AgentRequest {
                id: id.get(),
                operation: Some(operation),
            })),
        }
    }

    fn inbound(self) -> Inbound<Self> {
        match self.kind {
            Some(agent::agent_frame::Kind::Hello(hello)) => Inbound::Hello(hello),
            Some(agent::agent_frame::Kind::Response(response)) => {
                Inbound::Response(RequestId(response.id), response)
            }
            Some(agent::agent_frame::Kind::Event(event)) => Inbound::Event(event),
            Some(agent::agent_frame::Kind::Request(_)) | None => {
                Inbound::Unexpected("expected a response")
            }
        }
    }
}

/// A daemon request: its caption (for a session's recording), why the
/// client makes it (empty when it didn't say), and its operation.
#[derive(Debug, Clone, PartialEq)]
pub struct DaemonCall {
    pub caption: String,
    pub why: String,
    pub operation: daemon::daemon_request::Operation,
}

impl Protocol for daemon::DaemonFrame {
    type Request = DaemonCall;
    type Response = daemon::DaemonResponse;
    type Event = daemon::DaemonEvent;

    /// Clients (the CLI, MCP relays, consoles) ship with the daemon, but a
    /// daemon started before an upgrade keeps running: bump on any change
    /// an older peer would misread, and clients say to restart it.
    const VERSION: u32 = 1;

    fn hello(hello: common::Hello) -> Self {
        Self {
            kind: Some(daemon::daemon_frame::Kind::Hello(hello)),
        }
    }

    fn request(id: RequestId, call: Self::Request) -> Self {
        Self {
            kind: Some(daemon::daemon_frame::Kind::Request(daemon::DaemonRequest {
                id: id.get(),
                caption: call.caption,
                why: call.why,
                operation: Some(call.operation),
            })),
        }
    }

    fn inbound(self) -> Inbound<Self> {
        match self.kind {
            Some(daemon::daemon_frame::Kind::Hello(hello)) => Inbound::Hello(hello),
            Some(daemon::daemon_frame::Kind::Response(response)) => {
                Inbound::Response(RequestId(response.id), response)
            }
            Some(daemon::daemon_frame::Kind::Event(event)) => Inbound::Event(event),
            Some(
                daemon::daemon_frame::Kind::Request(_) | daemon::daemon_frame::Kind::Cancel(_),
            )
            | None => Inbound::Unexpected("expected a response"),
        }
    }

    fn cancel(id: RequestId) -> Option<Self> {
        Some(Self {
            kind: Some(daemon::daemon_frame::Kind::Cancel(daemon::CancelRequest {
                id: id.get(),
            })),
        })
    }
}

/// A conversation's frame type, seen from the server: the client greets
/// second, then sends requests, and may ask to stop one.
pub trait ServerProtocol: Protocol {
    /// A request as the server receives it, its id included.
    type Incoming: fmt::Debug;

    fn incoming(self) -> Incoming<Self>;
}

/// What a received frame is to a server.
#[derive(Debug)]
pub enum Incoming<P: ServerProtocol> {
    Hello(common::Hello),
    Request(P::Incoming),
    Cancel(RequestId),
    /// A frame a server is never sent: why it isn't expected.
    Unexpected(&'static str),
}

/// What a server heard from its client, once greeted.
#[derive(Debug)]
pub enum Heard<P: ServerProtocol> {
    Request(P::Incoming),
    /// The client asks to stop the request with this id.
    Cancel(RequestId),
    /// A frame this build can't read, from a newer client: passed over,
    /// and the conversation kept.
    Unreadable {
        bytes: usize,
    },
    /// A frame a client never sends: passed over, and the conversation kept.
    Unexpected(&'static str),
}

#[derive(Debug)]
enum Hearing {
    /// Waiting for the client's `Hello`, ours already sent.
    Greeting,
    Open,
    Over(ExchangeError),
}

/// A server's reading side of one conversation, sans-IO: the client's
/// greeting checked, then each frame sorted. Writing is the server's own:
/// it greets first with [`Reception::greeting`].
#[derive(Debug)]
pub struct Reception<P: ServerProtocol> {
    decoder: FrameDecoder<P>,
    hearing: Hearing,
}

impl<P: ServerProtocol> Reception<P> {
    #[must_use]
    pub fn new(limit: MaxFrameLen) -> Self {
        Self {
            decoder: FrameDecoder::new(limit),
            hearing: Hearing::Greeting,
        }
    }

    /// The greeting a server sends first: this build's protocol and version.
    #[must_use]
    pub fn greeting() -> P {
        <P as Protocol>::hello(greeting(P::VERSION))
    }

    /// Takes in bytes the transport read.
    pub fn receive(&mut self, bytes: &[u8]) {
        self.decoder.push(bytes);
    }

    /// The next thing heard, once its frame is whole.
    ///
    /// # Errors
    /// The conversation is over: the stream broke, the client's first frame
    /// wasn't its `Hello`, or it speaks another protocol version. The same
    /// error is returned from then on.
    pub fn hear(&mut self) -> Result<Option<Heard<P>>, ExchangeError> {
        loop {
            if let Hearing::Over(error) = &self.hearing {
                return Err(error.clone());
            }
            let frame = match self.decoder.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => return Ok(None),
                Err(error) => return Err(self.end(error.into())),
            };
            let message = match frame {
                Received::Recognized { message, .. } => message,
                Received::Unrecognized { raw } | Received::Malformed { raw, .. } => {
                    return Ok(Some(Heard::Unreadable {
                        bytes: raw.payload().len(),
                    }));
                }
            };
            match (&self.hearing, message.incoming()) {
                (Hearing::Greeting, Incoming::Hello(hello)) => {
                    if hello.protocol_version != P::VERSION {
                        return Err(self.end(ExchangeError::VersionMismatch {
                            ours: P::VERSION,
                            theirs: hello.protocol_version,
                        }));
                    }
                    self.hearing = Hearing::Open;
                }
                (
                    Hearing::Greeting,
                    Incoming::Request(_) | Incoming::Cancel(_) | Incoming::Unexpected(_),
                ) => return Err(self.end(ExchangeError::Unexpected("expected the client's Hello"))),
                (Hearing::Open, Incoming::Request(request)) => {
                    return Ok(Some(Heard::Request(request)));
                }
                (Hearing::Open, Incoming::Cancel(id)) => return Ok(Some(Heard::Cancel(id))),
                (Hearing::Open, Incoming::Hello(_)) => {
                    return Ok(Some(Heard::Unexpected("a second Hello")));
                }
                (Hearing::Open, Incoming::Unexpected(why)) => {
                    return Ok(Some(Heard::Unexpected(why)));
                }
                (Hearing::Over(error), _) => return Err(error.clone()),
            }
        }
    }

    fn end(&mut self, error: ExchangeError) -> ExchangeError {
        self.hearing = Hearing::Over(error.clone());
        error
    }
}

impl ServerProtocol for daemon::DaemonFrame {
    type Incoming = daemon::DaemonRequest;

    fn incoming(self) -> Incoming<Self> {
        match self.kind {
            Some(daemon::daemon_frame::Kind::Hello(hello)) => Incoming::Hello(hello),
            Some(daemon::daemon_frame::Kind::Request(request)) => Incoming::Request(request),
            Some(daemon::daemon_frame::Kind::Cancel(cancel)) => {
                Incoming::Cancel(RequestId(cancel.id))
            }
            Some(
                daemon::daemon_frame::Kind::Response(_) | daemon::daemon_frame::Kind::Event(_),
            )
            | None => Incoming::Unexpected("a frame that isn't a request"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FrameEncoder;
    use proptest::prelude::*;

    type Vmm = Exchange<vmm::VmmFrame, &'static str>;

    fn frame(frame: &vmm::VmmFrame) -> Vec<u8> {
        FrameEncoder::new(MaxFrameLen::DEFAULT)
            .encode(frame)
            .unwrap()
            .to_vec()
    }

    fn hello(version: u32) -> Vec<u8> {
        frame(&vmm::VmmFrame::hello(common::Hello {
            protocol_version: version,
            ..greeting(0)
        }))
    }

    fn done(id: u64) -> Vec<u8> {
        frame(&vmm::VmmFrame {
            kind: Some(vmm::vmm_frame::Kind::Response(vmm::VmmResponse {
                id,
                outcome: Some(vmm::vmm_response::Outcome::Done(vmm::Done {})),
            })),
        })
    }

    fn stopped() -> Vec<u8> {
        frame(&vmm::VmmFrame {
            kind: Some(vmm::vmm_frame::Kind::Event(vmm::VmmEvent {
                kind: Some(vmm::vmm_event::Kind::GuestStopped(
                    vmm::GuestStopped::default(),
                )),
            })),
        })
    }

    fn pause() -> vmm::vmm_request::Operation {
        vmm::vmm_request::Operation::Pause(vmm::Pause {})
    }

    fn greeted(config: ExchangeConfig) -> Vmm {
        let mut exchange = Vmm::new(config);
        exchange
            .receive(&hello(vmm::VmmFrame::VERSION), [])
            .unwrap();
        let reply = exchange.outgoing().len();
        exchange.wrote(reply);
        exchange
    }

    #[test]
    fn the_peer_greets_first_and_is_answered() {
        let mut exchange = Vmm::new(ExchangeConfig::STRICT);
        assert_eq!(exchange.request(pause()), Err(RequestError::NotGreeted));
        assert!(exchange.outgoing().is_empty());
        exchange
            .receive(&hello(vmm::VmmFrame::VERSION), [])
            .unwrap();
        assert_eq!(
            exchange.peer().map(|peer| peer.protocol_version),
            Some(vmm::VmmFrame::VERSION)
        );
        assert_eq!(exchange.outgoing(), hello(vmm::VmmFrame::VERSION));
    }

    #[test]
    fn another_version_ends_the_conversation_unanswered() {
        let mut exchange = Vmm::new(ExchangeConfig::STRICT);
        let mismatch = ExchangeError::VersionMismatch {
            ours: vmm::VmmFrame::VERSION,
            theirs: vmm::VmmFrame::VERSION + 1,
        };
        assert_eq!(
            exchange.receive(&hello(vmm::VmmFrame::VERSION + 1), []),
            Err(mismatch.clone())
        );
        assert!(exchange.outgoing().is_empty());
        assert_eq!(exchange.request(pause()), Err(RequestError::Over(mismatch)));
    }

    #[test]
    fn anything_before_the_greeting_ends_the_conversation() {
        let mut exchange = Vmm::new(ExchangeConfig::STRICT);
        assert_eq!(
            exchange.receive(&stopped(), []),
            Err(ExchangeError::Unexpected("expected Hello"))
        );
    }

    #[test]
    fn responses_are_filed_by_id_and_events_queue_between_them() {
        let mut exchange = greeted(ExchangeConfig::STRICT);
        let first = exchange.request(pause()).unwrap();
        let second = exchange.request(pause()).unwrap();
        assert_ne!(first, second);
        let mut stream = done(second.get());
        stream.extend(stopped());
        stream.extend(done(first.get()));
        exchange.receive(&stream, []).unwrap();
        assert!(exchange.answer(second).is_some());
        assert!(exchange.answer(first).is_some());
        assert!(exchange.answer(first).is_none(), "taken once");
        assert!(exchange.event().is_some());
        assert!(exchange.event().is_none());
    }

    #[test]
    fn a_response_nobody_asked_for_ends_the_conversation() {
        let mut exchange = greeted(ExchangeConfig::STRICT);
        let error = ExchangeError::Unexpected("response for another request");
        assert_eq!(exchange.receive(&done(9), []), Err(error.clone()));
        assert_eq!(exchange.receive(&stopped(), []), Err(error));
    }

    #[test]
    fn an_abandoned_request_s_response_is_an_orphan_whenever_it_comes() {
        let mut exchange = greeted(ExchangeConfig::STRICT);
        let late = exchange.request(pause()).unwrap();
        exchange.abandon(late);
        exchange.receive(&done(late.get()), []).unwrap();
        assert!(exchange.answer(late).is_none());

        let early = exchange.request(pause()).unwrap();
        exchange.receive(&done(early.get()), []).unwrap();
        exchange.abandon(early);
        assert!(exchange.answer(early).is_none());

        let orphans: Vec<u64> = exchange.orphans().map(|response| response.id).collect();
        assert_eq!(orphans, vec![late.get(), early.get()]);
        assert_eq!(exchange.orphans().count(), 0, "taken once");
    }

    #[test]
    fn only_a_protocol_that_can_ask_sends_a_cancel() {
        let mut vmm = greeted(ExchangeConfig::STRICT);
        let id = vmm.request(pause()).unwrap();
        vmm.wrote(vmm.outgoing().len());
        assert_eq!(vmm.cancel(id), Ok(false));
        assert!(vmm.outgoing().is_empty());

        let mut daemon = Exchange::<daemon::DaemonFrame>::new(ExchangeConfig::STRICT);
        let greeting = FrameEncoder::new(MaxFrameLen::DEFAULT)
            .encode(&daemon::DaemonFrame::hello(common::Hello {
                protocol_version: daemon::DaemonFrame::VERSION,
                ..greeting(0)
            }))
            .unwrap();
        daemon.receive(&greeting, []).unwrap();
        drop(daemon.take_outgoing());
        let call = daemon::daemon_request::Operation::ListMachines(daemon::ListMachines {});
        let id = daemon
            .request(DaemonCall {
                caption: String::new(),
                why: String::new(),
                operation: call,
            })
            .unwrap();
        drop(daemon.take_outgoing());
        assert_eq!(daemon.cancel(id), Ok(true));
        let mut sent = FrameDecoder::<daemon::DaemonFrame>::new(MaxFrameLen::DEFAULT);
        sent.push(&daemon.take_outgoing());
        assert!(matches!(
            sent.next_frame(),
            Ok(Some(Received::Recognized {
                message: daemon::DaemonFrame {
                    kind: Some(daemon::daemon_frame::Kind::Cancel(daemon::CancelRequest { id: sent })),
                },
                ..
            })) if sent == id.get()
        ));
        // Not awaited any more: nothing to ask.
        assert_eq!(daemon.cancel(RequestId(99)), Ok(false));
    }

    #[test]
    fn a_conversation_that_asks_for_no_events_keeps_none() {
        let mut exchange = greeted(ExchangeConfig {
            events: EventLimit::Ignored,
            ..ExchangeConfig::STRICT
        });
        exchange.receive(&stopped(), []).unwrap();
        assert!(exchange.event().is_none());
    }

    #[test]
    fn hints_keep_only_the_newest() {
        let mut exchange = greeted(ExchangeConfig {
            events: EventLimit::Newest(NonZeroUsize::MIN.saturating_add(1)),
            ..ExchangeConfig::STRICT
        });
        let three: Vec<u8> = [stopped(), stopped(), stopped()].concat();
        exchange.receive(&three, []).unwrap();
        assert!(exchange.event().is_some());
        assert!(exchange.event().is_some());
        assert!(exchange.event().is_none());
    }

    #[test]
    fn unknown_kinds_are_refused_or_passed_over_as_configured() {
        // Field 99 of VmmFrame: a oneof variant this build doesn't know.
        let mut payload = BytesMut::new();
        prost::encoding::encode_varint((99 << 3) | 2, &mut payload);
        prost::encoding::encode_varint(0, &mut payload);
        let mut unknown = u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec();
        unknown.extend_from_slice(&payload);

        let mut strict = greeted(ExchangeConfig::STRICT);
        assert_eq!(
            strict.receive(&unknown, []),
            Err(ExchangeError::Unexpected(
                "frame kind unknown to this build"
            ))
        );
        let mut lenient = greeted(ExchangeConfig {
            unrecognized: Unrecognized::Skip,
            ..ExchangeConfig::STRICT
        });
        lenient.receive(&unknown, []).unwrap();
        lenient.receive(&stopped(), []).unwrap();
        assert!(lenient.event().is_some());
    }

    #[test]
    fn an_attachment_goes_with_the_frame_it_was_sent_with() {
        let mut exchange = greeted(ExchangeConfig::STRICT);
        let id = exchange.request(pause()).unwrap();
        // An event, then the response, which carries the descriptor.
        let event = stopped();
        let response = done(id.get());
        exchange.receive(&event, []).unwrap();
        exchange.receive(&response, ["memory"]).unwrap();
        assert_eq!(exchange.answer(id).unwrap().attached, vec!["memory"]);
    }

    #[test]
    fn a_stray_attachment_is_never_taken_for_a_later_response() {
        let mut exchange = greeted(ExchangeConfig::STRICT);
        // A descriptor that came with an event belongs to the event and is
        // dropped with it; the response after it has none.
        let id = exchange.request(pause()).unwrap();
        exchange.receive(&stopped(), ["stray"]).unwrap();
        exchange.receive(&done(id.get()), []).unwrap();
        assert!(exchange.answer(id).unwrap().attached.is_empty());
    }

    #[test]
    fn an_attachment_read_with_the_tail_of_a_frame_goes_to_the_next() {
        let mut exchange = greeted(ExchangeConfig::STRICT);
        let id = exchange.request(pause()).unwrap();
        let event = stopped();
        let response = done(id.get());
        // The event's last byte arrives in the same chunk as the response,
        // with the response's descriptor.
        let (head, tail) = event.split_at(event.len() - 1);
        exchange.receive(head, []).unwrap();
        exchange
            .receive(&[tail, &response].concat(), ["memory"])
            .unwrap();
        assert_eq!(exchange.answer(id).unwrap().attached, vec!["memory"]);
    }

    proptest! {
        /// Design principle §9: the same answers and events however the
        /// stream is split, and every attachment with its own response.
        #[test]
        fn filing_is_independent_of_chunking(
            shape in prop::collection::vec(any::<bool>(), 1..12),
            cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..24),
        ) {
            let mut exchange = greeted(ExchangeConfig::STRICT);
            // Each `true` is a response carrying an attachment; each `false` an event.
            let mut stream = Vec::new();
            let mut starts = Vec::new();
            let mut ids = Vec::new();
            for &is_response in &shape {
                starts.push((stream.len(), is_response));
                if is_response {
                    let id = exchange.request(pause()).unwrap();
                    ids.push(id);
                    stream.extend(done(id.get()));
                } else {
                    stream.extend(stopped());
                }
            }
            let mut positions: Vec<usize> = cuts.iter().map(|c| c.index(stream.len() + 1)).collect();
            positions.push(0);
            positions.push(stream.len());
            // A response's attachment comes with the chunk its first byte is read in.
            positions.extend(starts.iter().filter(|(_, is_response)| *is_response).map(|(at, _)| *at));
            positions.sort_unstable();
            positions.dedup();
            let mut next_response = 0;
            for window in positions.windows(2) {
                let attached: Vec<&'static str> = starts
                    .iter()
                    .filter(|(at, is_response)| *is_response && *at == window[0])
                    .map(|_| "fd")
                    .collect();
                next_response += attached.len();
                exchange.receive(&stream[window[0]..window[1]], attached).unwrap();
            }
            prop_assert_eq!(next_response, ids.len());
            for id in ids {
                prop_assert_eq!(exchange.answer(id).unwrap().attached, vec!["fd"]);
            }
            let events = std::iter::from_fn(|| exchange.event()).count();
            prop_assert_eq!(events, shape.iter().filter(|r| !**r).count());
        }
    }

    mod reception {
        use super::*;

        fn daemon_bytes(frame: &daemon::DaemonFrame) -> Vec<u8> {
            FrameEncoder::new(MaxFrameLen::DEFAULT)
                .encode(frame)
                .unwrap()
                .to_vec()
        }

        fn client_hello(version: u32) -> Vec<u8> {
            daemon_bytes(&<daemon::DaemonFrame as Protocol>::hello(common::Hello {
                protocol_version: version,
                ..greeting(0)
            }))
        }

        fn request(id: u64) -> Vec<u8> {
            daemon_bytes(&daemon::DaemonFrame {
                kind: Some(daemon::daemon_frame::Kind::Request(daemon::DaemonRequest {
                    id,
                    caption: String::new(),
                    why: String::new(),
                    operation: None,
                })),
            })
        }

        fn heard(reception: &mut Reception<daemon::DaemonFrame>) -> Vec<String> {
            std::iter::from_fn(|| reception.hear().unwrap())
                .map(|heard| match heard {
                    Heard::Request(request) => format!("request {}", request.id),
                    Heard::Cancel(id) => format!("cancel {id}"),
                    Heard::Unreadable { bytes } => format!("unreadable {bytes}"),
                    Heard::Unexpected(why) => format!("unexpected: {why}"),
                })
                .collect()
        }

        #[test]
        fn requests_and_cancels_are_heard_once_the_client_greets() {
            let mut reception = Reception::<daemon::DaemonFrame>::new(MaxFrameLen::DEFAULT);
            reception.receive(&client_hello(daemon::DaemonFrame::VERSION));
            reception.receive(&request(1));
            reception.receive(&daemon_bytes(&daemon::DaemonFrame {
                kind: Some(daemon::daemon_frame::Kind::Cancel(daemon::CancelRequest {
                    id: 1,
                })),
            }));
            reception.receive(&client_hello(daemon::DaemonFrame::VERSION));
            assert_eq!(
                heard(&mut reception),
                vec!["request 1", "cancel 1", "unexpected: a second Hello"]
            );
        }

        #[test]
        fn a_client_that_does_not_greet_first_is_refused() {
            let mut reception = Reception::<daemon::DaemonFrame>::new(MaxFrameLen::DEFAULT);
            reception.receive(&request(1));
            let refused = ExchangeError::Unexpected("expected the client's Hello");
            assert_eq!(reception.hear().map(|_| ()), Err(refused.clone()));
            reception.receive(&client_hello(daemon::DaemonFrame::VERSION));
            assert_eq!(
                reception.hear().map(|_| ()),
                Err(refused),
                "and stays refused"
            );
        }

        #[test]
        fn another_version_is_refused() {
            let mut reception = Reception::<daemon::DaemonFrame>::new(MaxFrameLen::DEFAULT);
            reception.receive(&client_hello(daemon::DaemonFrame::VERSION + 1));
            assert_eq!(
                reception.hear().map(|_| ()),
                Err(ExchangeError::VersionMismatch {
                    ours: daemon::DaemonFrame::VERSION,
                    theirs: daemon::DaemonFrame::VERSION + 1
                })
            );
        }

        #[test]
        fn a_frame_this_build_cannot_read_is_passed_over() {
            let mut reception = Reception::<daemon::DaemonFrame>::new(MaxFrameLen::DEFAULT);
            reception.receive(&client_hello(daemon::DaemonFrame::VERSION));
            let mut garbled = 2u32.to_le_bytes().to_vec();
            garbled.extend_from_slice(&[0xff, 0xff]);
            reception.receive(&garbled);
            reception.receive(&request(2));
            assert_eq!(heard(&mut reception), vec!["unreadable 2", "request 2"]);
        }
    }
}
