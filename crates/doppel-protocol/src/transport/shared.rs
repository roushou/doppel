//! One conversation shared by callers on any number of threads.
//!
//! A reader thread files each response under its request's id as it comes,
//! and wakes the callers waiting. A caller waits for exactly the time left
//! before its deadline, or until its [`Cancel`] fires: nothing polls.
//! Writers take whole frames out of the exchange under a lock of their own,
//! so a large request being written never keeps the reader from filing
//! what arrives meanwhile, and requests sent from one thread arrive in the
//! order it sent them.

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use doppel_desktop::deadline::{Clock, Deadline, Remaining};

use super::{Cancel, Channel, ChannelError, Reader, write_all};
use crate::exchange::{Answer, Exchange, ExchangeConfig, Protocol, RequestId};
use crate::pb::common::v1 as common;

/// A conversation callers on many threads share. Dropping it closes the
/// connection and ends its reader.
#[derive(Debug)]
pub struct SharedChannel<P: Protocol, C> {
    socket: UnixStream,
    peer: common::Hello,
    clock: C,
    /// Held while a frame is taken and written, so frames never interleave.
    writer: Mutex<()>,
    mailbox: Arc<Mailbox<P>>,
    reader: Option<JoinHandle<()>>,
}

/// What the reader and the callers share.
#[derive(Debug)]
struct Mailbox<P: Protocol> {
    state: Mutex<State<P>>,
    /// Signalled whenever something is filed, the connection breaks, or a
    /// waiting caller is cancelled.
    changed: Condvar,
}

#[derive(Debug)]
struct State<P: Protocol> {
    exchange: Exchange<P, OwnedFd>,
    /// Why nothing more will come.
    broken: Option<Broken>,
}

#[derive(Debug, Clone)]
enum Broken {
    Closed,
    Failed(String),
}

impl Broken {
    fn error(&self) -> ChannelError {
        match self {
            Self::Closed => ChannelError::Closed,
            Self::Failed(why) => ChannelError::Lost(why.clone()),
        }
    }
}

impl<P: Protocol> Mailbox<P> {
    /// The state is the exchange and a flag: usable after any panic.
    fn lock(&self) -> MutexGuard<'_, State<P>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wake(&self) {
        // Taken, so a caller between its check and its wait can't miss it.
        drop(self.lock());
        self.changed.notify_all();
    }

    fn break_with(&self, broken: Broken) {
        self.lock().broken.get_or_insert(broken);
        self.changed.notify_all();
    }

    /// Files what the socket delivers until it closes or fails.
    fn read(&self, socket: &UnixStream, mut reader: Reader) {
        loop {
            let broken = match reader.read_blocking(socket) {
                Ok(([], _)) => Broken::Closed,
                Ok((bytes, attached)) => {
                    let mut state = self.lock();
                    let filed = state.exchange.receive(bytes, attached);
                    drop(state);
                    self.changed.notify_all();
                    match filed {
                        Ok(()) => continue,
                        Err(error) => Broken::Failed(error.to_string()),
                    }
                }
                Err(ChannelError::Closed) => Broken::Closed,
                Err(error) => Broken::Failed(error.to_string()),
            };
            self.break_with(broken);
            return;
        }
    }
}

impl<P, C> SharedChannel<P, C>
where
    P: Protocol + Send + 'static,
    P::Response: Send,
    P::Event: Send,
    C: Clock,
{
    /// Waits for the peer's greeting on `socket`, answers it, and starts
    /// the reader.
    ///
    /// # Errors
    /// As [`Channel::open`], or the reader's socket can't be duplicated.
    pub fn open(
        socket: UnixStream,
        clock: C,
        config: ExchangeConfig,
        by: Deadline,
    ) -> Result<Self, ChannelError> {
        let (socket, exchange, peer, clock, reader) =
            Channel::<P, C>::open(socket, clock, config, by)?.into_parts();
        let mailbox = Arc::new(Mailbox {
            state: Mutex::new(State {
                exchange,
                broken: None,
            }),
            changed: Condvar::new(),
        });
        let reading = socket.try_clone().map_err(ChannelError::Io)?;
        let reader = std::thread::Builder::new()
            .name("doppel-channel-reader".into())
            .spawn({
                let mailbox = Arc::clone(&mailbox);
                move || mailbox.read(&reading, reader)
            })
            .map_err(ChannelError::Io)?;
        Ok(Self {
            socket,
            peer,
            clock,
            writer: Mutex::new(()),
            mailbox,
            reader: Some(reader),
        })
    }

    /// The peer's greeting.
    #[must_use]
    pub const fn peer(&self) -> &common::Hello {
        &self.peer
    }

    /// Sends a request and waits by `by` for its response.
    ///
    /// Cancelling asks the peer to stop it, when the protocol can ask, and
    /// still waits for the answer it then gives; otherwise the request is
    /// given up at once ([`ChannelError::Cancelled`]). A deadline that
    /// passes gives it up too, after asking the peer to stop it.
    ///
    /// # Errors
    /// See [`ChannelError`].
    pub fn call(
        &self,
        request: P::Request,
        by: Deadline,
        cancel: &Cancel,
    ) -> Result<Answer<P::Response, OwnedFd>, ChannelError> {
        let id = self.send(|exchange| exchange.request(request).map_err(Into::into), by)?;
        let _wake = cancel.on_cancel({
            let mailbox = Arc::clone(&self.mailbox);
            move || mailbox.wake()
        });
        let mut asked_to_stop = false;
        let mut state = self.mailbox.lock();
        loop {
            if let Some(answer) = state.exchange.answer(id) {
                return Ok(answer);
            }
            if let Some(broken) = &state.broken {
                return Err(broken.error());
            }
            if !asked_to_stop && cancel.is_cancelled() {
                asked_to_stop = true;
                drop(state);
                if !self.ask_to_stop(id, by)? {
                    self.mailbox.lock().exchange.abandon(id);
                    return Err(ChannelError::Cancelled);
                }
                state = self.mailbox.lock();
                continue;
            }
            let left = match by.remaining(self.clock.now()) {
                Remaining::Left(left) => left,
                Remaining::Passed => {
                    drop(state);
                    // Best effort: the peer may stop sooner.
                    let _ = self.ask_to_stop(id, by);
                    self.mailbox.lock().exchange.abandon(id);
                    return Err(ChannelError::Timeout);
                }
            };
            state = self
                .mailbox
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The responses to requests given up on, oldest first.
    pub fn orphans(&self) -> Vec<P::Response> {
        self.mailbox.lock().exchange.orphans().collect()
    }

    /// Whether the peer can still answer: the connection hasn't broken.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.mailbox.lock().broken.is_none()
    }

    /// Asks the peer to stop `id`: whether the protocol could ask.
    fn ask_to_stop(&self, id: RequestId, by: Deadline) -> Result<bool, ChannelError> {
        self.send(|exchange| exchange.cancel(id).map_err(Into::into), by)
    }

    /// Lets `prepare` add frames to the exchange, then writes them, whole,
    /// by `by`. A write cut short leaves a partial frame on the socket, so
    /// it breaks the connection.
    fn send<T>(
        &self,
        prepare: impl FnOnce(&mut Exchange<P, OwnedFd>) -> Result<T, ChannelError>,
        by: Deadline,
    ) -> Result<T, ChannelError> {
        let _writing = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let (prepared, bytes) = {
            let mut state = self.mailbox.lock();
            if let Some(broken) = &state.broken {
                return Err(broken.error());
            }
            let prepared = prepare(&mut state.exchange)?;
            (prepared, state.exchange.take_outgoing())
        };
        if let Err(error) = write_all(&self.socket, &bytes, &self.clock, by) {
            self.mailbox.break_with(Broken::Failed(error.to_string()));
            let _ = self.socket.shutdown(std::net::Shutdown::Both);
            return Err(error);
        }
        Ok(prepared)
    }
}

impl<P: Protocol, C> Drop for SharedChannel<P, C> {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            // The reader ends at the shutdown; a panic in it is past mending.
            let _ = reader.join();
        }
    }
}
