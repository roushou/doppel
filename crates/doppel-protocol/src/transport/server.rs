//! A server's side of one conversation over a unix socket: [`Requests`]
//! reads what the client sends, on one thread, and [`Replies`] writes back
//! from any number, each frame whole.

use std::io::{IoSlice, Write};
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::BytesMut;
use nix::sys::socket::{ControlMessage, MsgFlags, sendmsg};

use super::{ChannelError, MOST_DESCRIPTORS, READ_BUFFER, Reader};
use crate::exchange::{Heard, Reception, ServerProtocol};
use crate::frame::{FrameEncoder, MaxFrameLen};

/// What the client sends, as it comes.
#[derive(Debug)]
pub struct Requests<P: ServerProtocol> {
    socket: UnixStream,
    reader: Reader,
    reception: Reception<P>,
}

impl<P: ServerProtocol> Requests<P> {
    /// Reads from `socket`; the server greets first, with [`Replies::greet`].
    #[must_use]
    pub fn new(socket: UnixStream) -> Self {
        Self {
            socket,
            reader: Reader {
                buffer: vec![0; READ_BUFFER].into_boxed_slice(),
                control: nix::cmsg_space!([RawFd; MOST_DESCRIPTORS]),
            },
            reception: Reception::new(MaxFrameLen::DEFAULT),
        }
    }

    /// The next thing the client says, waiting as long as it takes; `None`
    /// once it closes the connection.
    ///
    /// # Errors
    /// The connection failed, or the client broke the conversation's rules
    /// (it didn't greet, or speaks another protocol version).
    pub fn hear(&mut self) -> Result<Option<Heard<P>>, ChannelError> {
        loop {
            if let Some(heard) = self.reception.hear()? {
                return Ok(Some(heard));
            }
            match self.reader.read_blocking(&self.socket) {
                Ok(([], _)) | Err(ChannelError::Closed) => return Ok(None),
                // A client passes no descriptors: any that come are closed.
                Ok((bytes, _)) => self.reception.receive(bytes),
                Err(error) => return Err(error),
            }
        }
    }
}

/// The way back to the client, shared by whoever answers it.
#[derive(Debug)]
pub struct Replies<P: ServerProtocol> {
    writing: Mutex<Writing>,
    frames: PhantomData<fn(&P)>,
}

#[derive(Debug)]
struct Writing {
    socket: UnixStream,
    encoder: FrameEncoder,
    /// Reused for each frame.
    buffer: BytesMut,
}

impl<P: ServerProtocol> Replies<P> {
    /// Writes on `socket`; a client that reads nothing for `patience` while
    /// a frame waits to be written fails that write.
    ///
    /// # Errors
    /// The socket can't be duplicated, or set up.
    pub fn new(socket: &UnixStream, patience: Duration) -> Result<Self, ChannelError> {
        let socket = socket.try_clone().map_err(ChannelError::Io)?;
        socket
            .set_write_timeout(Some(patience))
            .map_err(ChannelError::Io)?;
        Ok(Self {
            writing: Mutex::new(Writing {
                socket,
                encoder: FrameEncoder::new(MaxFrameLen::DEFAULT),
                buffer: BytesMut::new(),
            }),
            frames: PhantomData,
        })
    }

    /// Greets the client: the server speaks first.
    ///
    /// # Errors
    /// See [`Self::send`].
    pub fn greet(&self) -> Result<(), ChannelError> {
        self.send(&Reception::<P>::greeting())
    }

    /// Writes `frame`, whole: frames sent from other threads never
    /// interleave with it.
    ///
    /// # Errors
    /// It's too large to send, or the write failed.
    pub fn send(&self, frame: &P) -> Result<(), ChannelError> {
        let mut writing = self.lock();
        let Writing {
            socket,
            encoder,
            buffer,
        } = &mut *writing;
        buffer.clear();
        encoder.encode_into(frame, buffer)?;
        (&*socket).write_all(buffer).map_err(ChannelError::from_io)
    }

    /// Writes `frame` with `attached` passed along (`SCM_RIGHTS`) on its
    /// first byte, so the client receives the descriptor with this frame.
    ///
    /// # Errors
    /// See [`Self::send`].
    pub fn send_with(&self, frame: &P, attached: BorrowedFd<'_>) -> Result<(), ChannelError> {
        let mut writing = self.lock();
        let Writing {
            socket,
            encoder,
            buffer,
        } = &mut *writing;
        buffer.clear();
        encoder.encode_into(frame, buffer)?;
        let fds = [attached.as_raw_fd()];
        let sent = sendmsg::<()>(
            socket.as_raw_fd(),
            &[IoSlice::new(buffer)],
            &[ControlMessage::ScmRights(&fds)],
            MsgFlags::empty(),
            None,
        )
        .map_err(|errno| ChannelError::from_io(errno.into()))?;
        (&*socket)
            .write_all(buffer.get(sent..).unwrap_or_default())
            .map_err(ChannelError::from_io)
    }

    /// Ends the connection both ways: the client reads its end, and the
    /// server's [`Requests`] stops.
    pub fn close(&self) {
        // Already shut down or closed: then it's ended anyway.
        let _ = self.lock().socket.shutdown(std::net::Shutdown::Both);
    }

    /// The writer is a socket and a buffer: usable after any panic.
    fn lock(&self) -> MutexGuard<'_, Writing> {
        self.writing.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
