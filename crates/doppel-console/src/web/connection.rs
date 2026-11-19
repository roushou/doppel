//! One connection to the daemon through the gateway's WebSocket: the token
//! first, then `doppel.daemon.v1` frames, one per WebSocket message.
//! Requests are answered by id; a screen's frames and the agents' activity
//! come as events of the request that opened them, until it's cancelled.

use std::cell::RefCell;
use std::collections::HashMap;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use daemon::daemon_frame::Kind;
use daemon::daemon_response::Outcome;
use doppel_protocol::convert::RemoteError;
use doppel_protocol::exchange::Protocol as _;
use doppel_protocol::pb::common::v1 as common;
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_protocol::report::MachineFailure;
use doppel_protocol::rpc::Rpc;
use futures::Stream;
use futures::channel::{mpsc, oneshot};
use prost::Message as _;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::Closure;
use web_sys::{BinaryType, CloseEvent, MessageEvent, WebSocket};

/// Why a request got no answer it could use.
#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum Refusal {
    /// The connection is gone, or never was.
    #[error("{0}")]
    Lost(String),
    /// The daemon answered, and refused.
    #[error("{0}")]
    Refused(String),
    /// The daemon answered with something this console can't read: what
    /// it did is unknown.
    #[error("{0}")]
    Unreadable(String),
    /// A machine operation failed on the daemon's side.
    #[error("{0}")]
    Machine(MachineFailure),
}

/// A request waiting for its answer, and its events after it.
enum Pending {
    Answer(oneshot::Sender<Option<Outcome>>),
    Stream {
        answer: Option<oneshot::Sender<Option<Outcome>>>,
        events: mpsc::UnboundedSender<daemon::daemon_event::Kind>,
    },
}

#[derive(Default)]
struct State {
    pending: HashMap<u64, Pending>,
    next: u64,
    /// Why it closed, once it did.
    closed: Option<String>,
    greeted: Option<oneshot::Sender<Result<(), String>>>,
}

/// An open connection, shared by every call the window makes.
pub(crate) struct Connection {
    socket: WebSocket,
    state: Rc<RefCell<State>>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
    _on_open: Closure<dyn FnMut()>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}

impl Connection {
    /// Connects to `url`, presents `token`, and greets the daemon.
    pub(crate) async fn open(url: &str, token: &str) -> Result<Rc<Self>, String> {
        let socket = WebSocket::new(url).map_err(|_| format!("can't open {url}"))?;
        socket.set_binary_type(BinaryType::Arraybuffer);
        let (greeted, hello) = oneshot::channel();
        let state = Rc::new(RefCell::new(State {
            next: 1,
            greeted: Some(greeted),
            ..State::default()
        }));
        let on_message = {
            let state = Rc::clone(&state);
            Closure::<dyn FnMut(MessageEvent)>::new(move |message: MessageEvent| {
                let Ok(buffer) = message.data().dyn_into::<js_sys::ArrayBuffer>() else {
                    return;
                };
                let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                if let Ok(frame) = daemon::DaemonFrame::decode(bytes.as_slice()) {
                    received(&state, frame);
                }
            })
        };
        let on_close = {
            let state = Rc::clone(&state);
            Closure::<dyn FnMut(CloseEvent)>::new(move |event: CloseEvent| {
                let reason = event.reason();
                closed(
                    &state,
                    if reason.is_empty() {
                        format!("the connection closed ({})", event.code())
                    } else {
                        reason
                    },
                );
            })
        };
        let on_open = {
            let socket = socket.clone();
            let token = token.to_owned();
            Closure::<dyn FnMut()>::new(move || {
                let _ = socket.send_with_str(&token);
                send(
                    &socket,
                    Kind::Hello(common::Hello {
                        protocol_version: daemon::DaemonFrame::VERSION,
                        software_version: "doppel browser".into(),
                        ..common::Hello::default()
                    }),
                );
            })
        };
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        let connection = Rc::new(Self {
            socket,
            state,
            _on_message: on_message,
            _on_close: on_close,
            _on_open: on_open,
        });
        hello
            .await
            .unwrap_or_else(|_| Err("the connection closed".into()))?;
        Ok(connection)
    }

    /// Whether it closed.
    pub(crate) fn is_closed(&self) -> bool {
        self.state.borrow().closed.is_some()
    }

    /// Sends a request and waits for its answer.
    pub(crate) async fn call<R: Rpc>(&self, request: R) -> Result<R::Answer, Refusal> {
        let (answer, answered) = oneshot::channel();
        let id = self.request(request.into_operation(), Pending::Answer(answer))?;
        let outcome = answered.await.ok().flatten();
        let _ = id;
        Self::answer::<R>(outcome)
    }

    /// Sends a request whose events follow its answer: they come until the
    /// returned stream is dropped, which cancels the request.
    pub(crate) async fn stream<R: Rpc>(
        self: &Rc<Self>,
        request: R,
    ) -> Result<(R::Answer, Events), Refusal> {
        let (answer, answered) = oneshot::channel();
        let (events, arrived) = mpsc::unbounded();
        let id = self.request(
            request.into_operation(),
            Pending::Stream {
                answer: Some(answer),
                events,
            },
        )?;
        let events = Events {
            connection: Rc::clone(self),
            id,
            arrived,
        };
        let outcome = answered.await.ok().flatten();
        Ok((Self::answer::<R>(outcome)?, events))
    }

    fn request(
        &self,
        operation: daemon::daemon_request::Operation,
        pending: Pending,
    ) -> Result<u64, Refusal> {
        let mut state = self.state.borrow_mut();
        if let Some(why) = &state.closed {
            return Err(Refusal::Lost(why.clone()));
        }
        let id = state.next;
        state.next += 1;
        state.pending.insert(id, pending);
        drop(state);
        send(
            &self.socket,
            Kind::Request(daemon::DaemonRequest {
                id,
                caption: String::new(),
                why: String::new(),
                operation: Some(operation),
            }),
        );
        Ok(id)
    }

    /// Stops the request `id`, and forgets it.
    fn cancel(&self, id: u64) {
        if self.state.borrow_mut().pending.remove(&id).is_some() && !self.is_closed() {
            send(&self.socket, Kind::Cancel(daemon::CancelRequest { id }));
        }
    }

    /// The answer an outcome carries, or why there's none.
    fn answer<R: Rpc>(outcome: Option<Outcome>) -> Result<R::Answer, Refusal> {
        match outcome {
            None => Err(Refusal::Lost("the connection closed".into())),
            Some(Outcome::Error(error)) => {
                Err(Refusal::Refused(RemoteError::from(error).to_string()))
            }
            Some(Outcome::MachineFailure(failure)) => {
                Err(Refusal::Machine(MachineFailure::from_wire(failure)))
            }
            Some(outcome) => R::answered(outcome)
                .ok_or_else(|| Refusal::Unreadable(format!("an unexpected answer to {}", R::NAME))),
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.socket.close();
    }
}

fn send(socket: &WebSocket, kind: Kind) {
    let frame = daemon::DaemonFrame { kind: Some(kind) };
    let _ = socket.send_with_u8_array(&frame.encode_to_vec());
}

/// Files a frame from the daemon under its request.
fn received(state: &Rc<RefCell<State>>, frame: daemon::DaemonFrame) {
    let mut state = state.borrow_mut();
    match frame.kind {
        Some(Kind::Hello(_)) => {
            if let Some(greeted) = state.greeted.take() {
                let _ = greeted.send(Ok(()));
            }
        }
        Some(Kind::Response(response)) => match state.pending.remove(&response.id) {
            Some(Pending::Answer(answer)) => {
                let _ = answer.send(response.outcome);
            }
            Some(Pending::Stream { answer, events }) => {
                if let Some(answer) = answer {
                    let _ = answer.send(response.outcome);
                }
                // Its events go on until it's cancelled.
                state.pending.insert(
                    response.id,
                    Pending::Stream {
                        answer: None,
                        events,
                    },
                );
            }
            None => {}
        },
        Some(Kind::Event(event)) => {
            if let (Some(Pending::Stream { events, .. }), Some(kind)) =
                (state.pending.get(&event.request), event.kind)
            {
                let _ = events.unbounded_send(kind);
            }
        }
        // A client's frames, or kinds a newer daemon adds: passed over.
        Some(Kind::Request(_) | Kind::Cancel(_)) | None => {}
    }
}

/// The connection ended: every request still waiting is answered with
/// nothing, and every stream ends.
fn closed(state: &Rc<RefCell<State>>, why: String) {
    let mut state = state.borrow_mut();
    if let Some(greeted) = state.greeted.take() {
        let _ = greeted.send(Err(why.clone()));
    }
    state.closed = Some(why);
    // Dropping the senders answers and ends them.
    state.pending.clear();
}

/// A request's events, until it's dropped, which cancels the request.
pub(crate) struct Events {
    connection: Rc<Connection>,
    id: u64,
    arrived: mpsc::UnboundedReceiver<daemon::daemon_event::Kind>,
}

impl Stream for Events {
    type Item = daemon::daemon_event::Kind;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.arrived).poll_next(cx)
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        self.connection.cancel(self.id);
    }
}
