//! Carries one conversation over a unix socket: the blocking shell around
//! [`Exchange`], which keeps the conversation's rules. The only module of
//! this crate that does I/O; `tests/pure.rs` keeps it so.
//!
//! A [`Channel`] reads with `recvmsg`, so descriptors the peer passes
//! (`SCM_RIGHTS`) come with the frame they were sent with, and waits with
//! `poll(2)` for exactly the time left before a [`Deadline`]: the socket
//! stays non-blocking and carries no timeout state between calls. One
//! buffer is kept for reading and one for writing, so a steady stream of
//! frames costs a read each and allocates nothing per frame.
//!
//! Cancelling is closing: an [`Interrupter`] shuts the socket down from any
//! thread, and whatever waits on it ends with [`ChannelError::Closed`]. The
//! peer sees the hang-up (the agent stops a program it runs for an exec).
//! A wait for an event can instead end on a [`Wakeup`], which leaves the
//! channel open.

mod cancel;
mod server;
mod shared;
mod wakeup;

pub use cancel::{Cancel, Hook, Slept};
pub use server::{Replies, Requests};
pub use shared::SharedChannel;
pub use wakeup::{Waker, Wakeup};

use std::io::{self, IoSliceMut, Write};
use std::net::Shutdown;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

use crate::exchange::{Answer, Exchange, ExchangeConfig, ExchangeError, Protocol, RequestError};
use crate::frame::EncodeError;
use crate::pb::common::v1 as common;
use doppel_desktop::deadline::{Clock, Deadline, Remaining};
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::socket::{ControlMessageOwned, MsgFlags, recv, recvmsg};

/// Bytes read at once: a screenshot of several MiB takes a few reads.
const READ_BUFFER: usize = 256 * 1024;

/// Descriptors accepted with one read. The VMM passes one per response.
const MOST_DESCRIPTORS: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("{0}")]
    Io(#[source] io::Error),
    /// The peer closed the connection, or it was interrupted.
    #[error("the connection closed")]
    Closed,
    #[error("timed out")]
    Timeout,
    /// The connection failed earlier, while another caller used it.
    #[error("the connection failed: {0}")]
    Lost(String),
    /// The caller cancelled, and the protocol can't ask the peer to stop:
    /// the request was given up.
    #[error("cancelled")]
    Cancelled,
    /// The request is too large to send: nothing was sent.
    #[error(transparent)]
    Encode(#[from] EncodeError),
    /// The peer broke the conversation's rules.
    #[error(transparent)]
    Exchange(#[from] ExchangeError),
}

impl From<RequestError> for ChannelError {
    fn from(error: RequestError) -> Self {
        match error {
            RequestError::Encode(error) => Self::Encode(error),
            RequestError::Over(error) => Self::Exchange(error),
            // A channel is greeted when it opens.
            RequestError::NotGreeted => Self::Exchange(ExchangeError::Unexpected("not greeted")),
        }
    }
}

impl ChannelError {
    /// `io::ErrorKind` is a foreign `#[non_exhaustive]` enum, so this
    /// classifies by comparison rather than an exhaustive match.
    fn from_io(error: io::Error) -> Self {
        let kind = error.kind();
        if kind == io::ErrorKind::WouldBlock || kind == io::ErrorKind::TimedOut {
            Self::Timeout
        } else if kind == io::ErrorKind::UnexpectedEof
            || kind == io::ErrorKind::BrokenPipe
            || kind == io::ErrorKind::ConnectionReset
            || kind == io::ErrorKind::NotConnected
        {
            Self::Closed
        } else {
            Self::Io(error)
        }
    }
}

/// One greeted conversation with a peer that speaks first.
#[derive(Debug)]
pub struct Channel<P: Protocol, C> {
    socket: UnixStream,
    exchange: Exchange<P, OwnedFd>,
    peer: common::Hello,
    clock: C,
    reader: Reader,
}

impl<P: Protocol, C: Clock> Channel<P, C> {
    /// Waits for the peer's greeting on `socket` and answers it.
    ///
    /// # Errors
    /// The peer didn't greet by `by`, closed, or speaks another protocol.
    pub fn open(
        socket: UnixStream,
        clock: C,
        config: ExchangeConfig,
        by: Deadline,
    ) -> Result<Self, ChannelError> {
        socket.set_nonblocking(true).map_err(ChannelError::Io)?;
        let mut exchange = Exchange::new(config);
        let mut reader = Reader {
            buffer: vec![0; READ_BUFFER].into_boxed_slice(),
            control: nix::cmsg_space!([RawFd; MOST_DESCRIPTORS]),
        };
        let peer = loop {
            if let Some(peer) = exchange.peer() {
                break peer.clone();
            }
            reader.fill(&socket, &mut exchange, &clock, by)?;
        };
        let mut channel = Self {
            socket,
            exchange,
            peer,
            clock,
            reader,
        };
        channel.flush(by)?;
        Ok(channel)
    }

    /// The peer's greeting.
    #[must_use]
    pub const fn peer(&self) -> &common::Hello {
        &self.peer
    }

    #[must_use]
    pub const fn clock(&self) -> &C {
        &self.clock
    }

    /// Sends a request and waits by `by` for its response, with the
    /// descriptors sent with it. Events pushed meanwhile queue for
    /// [`Self::next_event`].
    ///
    /// # Errors
    /// See [`ChannelError`]. A request that times out is abandoned: its
    /// response, if it comes later, is dropped.
    pub fn call(
        &mut self,
        request: P::Request,
        by: Deadline,
    ) -> Result<Answer<P::Response, OwnedFd>, ChannelError> {
        let id = self.exchange.request(request)?;
        let answered = self.flush(by).and_then(|()| {
            loop {
                if let Some(answer) = self.exchange.answer(id) {
                    break Ok(answer);
                }
                self.fill(by)?;
            }
        });
        if answered.is_err() {
            self.exchange.abandon(id);
        }
        answered
    }

    /// The next pushed event: one already received, or the next to arrive
    /// before `until`. `None` once `until` passes with none.
    ///
    /// # Errors
    /// See [`ChannelError`]; never [`ChannelError::Timeout`].
    pub fn next_event(&mut self, until: Deadline) -> Result<Option<P::Event>, ChannelError> {
        loop {
            if let Some(event) = self.exchange.event() {
                return Ok(Some(event));
            }
            match self.fill(until) {
                Ok(()) => {}
                Err(ChannelError::Timeout) => return Ok(None),
                Err(error) => return Err(error),
            }
        }
    }

    /// The next pushed event: one already received, or the next to arrive
    /// before `until` (`None`: however long it takes), unless `wakeup` is
    /// woken first.
    ///
    /// # Errors
    /// See [`ChannelError`]; never [`ChannelError::Timeout`].
    pub fn wait_event(
        &mut self,
        until: Option<Deadline>,
        wakeup: &Wakeup,
    ) -> Result<Waited<P::Event>, ChannelError> {
        loop {
            if let Some(event) = self.exchange.event() {
                return Ok(Waited::Event(event));
            }
            match self.reader.fill_unless(
                &self.socket,
                &mut self.exchange,
                &self.clock,
                until,
                wakeup,
            )? {
                Filled::Read => {}
                Filled::TimedOut => return Ok(Waited::TimedOut),
                Filled::WokenUp => return Ok(Waited::WokenUp),
            }
        }
    }

    /// Forgets the events received so far.
    pub fn discard_events(&mut self) {
        self.exchange.discard_events();
    }

    /// Whether the connection still looks usable: the peer hasn't closed
    /// it. Doesn't wait, and reads nothing.
    #[must_use]
    pub fn is_open(&self) -> bool {
        let mut byte = [0u8; 1];
        match recv(
            self.socket.as_raw_fd(),
            &mut byte,
            MsgFlags::MSG_PEEK | MsgFlags::MSG_DONTWAIT,
        ) {
            Ok(0) => false,
            Ok(_) => true,
            Err(errno) => errno == Errno::EAGAIN,
        }
    }

    /// A handle that closes this channel from any thread.
    ///
    /// # Errors
    /// The socket couldn't be duplicated.
    pub fn interrupter(&self) -> Result<Interrupter, ChannelError> {
        self.socket
            .try_clone()
            .map(Interrupter)
            .map_err(ChannelError::Io)
    }

    fn fill(&mut self, by: Deadline) -> Result<(), ChannelError> {
        self.reader
            .fill(&self.socket, &mut self.exchange, &self.clock, by)
    }

    /// Its parts, for a [`SharedChannel`] that serves many callers.
    fn into_parts(self) -> (UnixStream, Exchange<P, OwnedFd>, common::Hello, C, Reader) {
        (
            self.socket,
            self.exchange,
            self.peer,
            self.clock,
            self.reader,
        )
    }

    /// Writes everything the exchange has to send, by `by`.
    fn flush(&mut self, by: Deadline) -> Result<(), ChannelError> {
        while !self.exchange.outgoing().is_empty() {
            match (&self.socket).write(self.exchange.outgoing()) {
                Ok(0) => return Err(ChannelError::Closed),
                Ok(written) => self.exchange.wrote(written),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    ready(&self.socket, PollFlags::POLLOUT, &self.clock, by)?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(ChannelError::from_io(error)),
            }
        }
        Ok(())
    }
}

/// How a [`Channel::wait_event`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waited<E> {
    Event(E),
    TimedOut,
    WokenUp,
}

/// How a [`Reader::fill_unless`] ended.
enum Filled {
    Read,
    TimedOut,
    WokenUp,
}

/// The read side's buffers, kept for the channel's life.
struct Reader {
    buffer: Box<[u8]>,
    control: Vec<u8>,
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader")
            .field("buffer", &self.buffer.len())
            .finish_non_exhaustive()
    }
}

impl Reader {
    /// Reads once, waiting by `by` for something to read, and gives the
    /// exchange what came.
    fn fill<P: Protocol>(
        &mut self,
        socket: &UnixStream,
        exchange: &mut Exchange<P, OwnedFd>,
        clock: &impl Clock,
        by: Deadline,
    ) -> Result<(), ChannelError> {
        loop {
            match self.read(socket) {
                Ok((0, _)) => return Err(ChannelError::Closed),
                Ok((read, attached)) => {
                    let bytes = self.buffer.get(..read).unwrap_or_default();
                    exchange.receive(bytes, attached)?;
                    return Ok(());
                }
                Err(Errno::EAGAIN) => ready(socket, PollFlags::POLLIN, clock, by)?,
                Err(Errno::EINTR) => {}
                Err(errno) => return Err(ChannelError::from_io(errno.into())),
            }
        }
    }

    /// Reads once, waiting until `until` (`None`: however long it takes)
    /// for something to read, unless `wakeup` is woken first.
    fn fill_unless<P: Protocol>(
        &mut self,
        socket: &UnixStream,
        exchange: &mut Exchange<P, OwnedFd>,
        clock: &impl Clock,
        until: Option<Deadline>,
        wakeup: &Wakeup,
    ) -> Result<Filled, ChannelError> {
        loop {
            match self.read(socket) {
                Ok((0, _)) => return Err(ChannelError::Closed),
                Ok((read, attached)) => {
                    let bytes = self.buffer.get(..read).unwrap_or_default();
                    exchange.receive(bytes, attached)?;
                    return Ok(Filled::Read);
                }
                Err(Errno::EAGAIN) => {
                    let timeout = match until.map(|by| by.remaining(clock.now())) {
                        None => PollTimeout::NONE,
                        Some(Remaining::Passed) => return Ok(Filled::TimedOut),
                        Some(Remaining::Left(left)) => poll_timeout(left),
                    };
                    let mut fds = [
                        PollFd::new(socket.as_fd(), PollFlags::POLLIN),
                        PollFd::new(wakeup.fd(), PollFlags::POLLIN),
                    ];
                    match poll(&mut fds, timeout) {
                        // Ready, hung up, failed or timed out: the read that
                        // follows, or the deadline, says which.
                        Ok(_) | Err(Errno::EINTR) => {}
                        Err(errno) => return Err(ChannelError::Io(errno.into())),
                    }
                    let woken = fds[1].revents().is_some_and(|revents| {
                        revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP)
                    });
                    if woken {
                        return Ok(Filled::WokenUp);
                    }
                }
                Err(Errno::EINTR) => {}
                Err(errno) => return Err(ChannelError::from_io(errno.into())),
            }
        }
    }

    /// Reads once, waiting as long as it takes: what came, and the
    /// descriptors with it. `Ok((0, _))` once the peer closed.
    fn read_blocking(
        &mut self,
        socket: &UnixStream,
    ) -> Result<(&[u8], Vec<OwnedFd>), ChannelError> {
        loop {
            match self.read(socket) {
                Ok((read, attached)) => {
                    return Ok((self.buffer.get(..read).unwrap_or_default(), attached));
                }
                Err(Errno::EAGAIN) => {
                    let mut fds = [PollFd::new(socket.as_fd(), PollFlags::POLLIN)];
                    match poll(&mut fds, PollTimeout::NONE) {
                        Ok(_) | Err(Errno::EINTR) => {}
                        Err(errno) => return Err(ChannelError::Io(errno.into())),
                    }
                }
                Err(Errno::EINTR) => {}
                Err(errno) => return Err(ChannelError::from_io(errno.into())),
            }
        }
    }

    /// One `recvmsg`, taking ownership of any descriptors that came.
    fn read(&mut self, socket: &UnixStream) -> Result<(usize, Vec<OwnedFd>), Errno> {
        let mut iov = [IoSliceMut::new(&mut self.buffer)];
        let message = recvmsg::<()>(
            socket.as_raw_fd(),
            &mut iov,
            Some(&mut self.control),
            MsgFlags::empty(),
        )?;
        let mut attached = Vec::new();
        for control in message.cmsgs()? {
            if let ControlMessageOwned::ScmRights(raw) = control {
                for raw in raw {
                    #[expect(
                        unsafe_code,
                        reason = "taking ownership of a descriptor fresh from SCM_RIGHTS"
                    )]
                    // SAFETY: the kernel just installed `raw` in our table for
                    // this message. Nothing else knows it: we are its sole owner.
                    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
                    // macOS has no MSG_CMSG_CLOEXEC: keep it out of processes
                    // spawned later (a VMM started after this read).
                    nix::fcntl::fcntl(
                        &fd,
                        nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
                    )?;
                    attached.push(fd);
                }
            }
        }
        Ok((message.bytes, attached))
    }
}

/// Writes all of `bytes` by `by`.
fn write_all(
    socket: &UnixStream,
    mut bytes: &[u8],
    clock: &impl Clock,
    by: Deadline,
) -> Result<(), ChannelError> {
    while !bytes.is_empty() {
        match (&*socket).write(bytes) {
            Ok(0) => return Err(ChannelError::Closed),
            Ok(written) => bytes = bytes.get(written..).unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                ready(socket, PollFlags::POLLOUT, clock, by)?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(ChannelError::from_io(error)),
        }
    }
    Ok(())
}

/// Waits until `socket` is ready for `flags`, or `by` passes.
fn ready(
    socket: &UnixStream,
    flags: PollFlags,
    clock: &impl Clock,
    by: Deadline,
) -> Result<(), ChannelError> {
    let left = match by.remaining(clock.now()) {
        Remaining::Passed => return Err(ChannelError::Timeout),
        Remaining::Left(left) => left,
    };
    let timeout = poll_timeout(left);
    let mut fds = [PollFd::new(socket.as_fd(), flags)];
    match poll(&mut fds, timeout) {
        // Ready, hung up or failed: the read or write that follows says which.
        Ok(_) | Err(Errno::EINTR) => Ok(()),
        Err(errno) => Err(ChannelError::Io(errno.into())),
    }
}

/// `left` as a poll's timeout: rounded up, so a wait never ends just
/// before its deadline and spins.
fn poll_timeout(left: std::time::Duration) -> PollTimeout {
    let millis = left.as_micros().div_ceil(1000);
    PollTimeout::try_from(millis).unwrap_or(PollTimeout::MAX)
}

/// Closes a [`Channel`] from another thread: its waits end, and the peer
/// sees the connection close.
#[derive(Debug)]
pub struct Interrupter(UnixStream);

impl Interrupter {
    pub fn interrupt(&self) {
        // Already shut down or closed: then it's interrupted anyway.
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests;
