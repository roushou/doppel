//! One browser's WebSocket, once its token is accepted: its messages go to
//! the daemon over one end of a socket pair, framed as on any daemon
//! connection, and the daemon's come back one per WebSocket message. It
//! waits on both sockets with `poll`, never on a timer, until either side
//! closes, the token expires, or the gateway closes it.

use std::io::{self, Read as _, Write as _};
use std::net::TcpStream;
use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;
use std::time::{Duration, SystemTime};

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use rustls::{ServerConnection, StreamOwned};
use tungstenite::protocol::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::{Message, WebSocket};

use crate::framing::{Splitter, framed};

pub(crate) type Socket = WebSocket<StreamOwned<ServerConnection, TcpStream>>;

/// Why the gateway closes a connection from its side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Closing {
    Revoked,
    Stopping,
}

impl Closing {
    pub(crate) const fn byte(self) -> u8 {
        match self {
            Self::Revoked => 1,
            Self::Stopping => 2,
        }
    }

    fn from_byte(byte: u8) -> Self {
        if byte == Self::Revoked.byte() {
            Self::Revoked
        } else {
            Self::Stopping
        }
    }
}

/// How a bridged connection ended.
#[derive(Debug)]
pub(crate) enum Ended {
    /// The browser closed it, or went away.
    Browser,
    /// The daemon closed its end.
    Daemon,
    Expired,
    Closed(Closing),
    Failed(String),
}

/// What the bridge needs from outside: the time, to close at expiry.
pub(crate) struct Bridge<'a> {
    pub(crate) socket: Socket,
    pub(crate) daemon: UnixStream,
    /// Readable when the gateway closes this connection: one byte says why.
    pub(crate) wake: UnixStream,
    pub(crate) expires: SystemTime,
    pub(crate) now: &'a dyn Fn() -> SystemTime,
}

fn would_block(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
}

fn ws_would_block(error: &tungstenite::Error) -> bool {
    matches!(error, tungstenite::Error::Io(io) if would_block(io))
}

impl Bridge<'_> {
    pub(crate) fn run(mut self) -> Ended {
        let ended = self.pump();
        let (code, reason) = match &ended {
            Ended::Expired => (CloseCode::Policy, "token expired"),
            Ended::Closed(Closing::Revoked) => (CloseCode::Policy, "token revoked"),
            Ended::Closed(Closing::Stopping) => (CloseCode::Away, "the listener stopped"),
            Ended::Daemon => (CloseCode::Away, "the daemon closed the connection"),
            Ended::Failed(_) => (CloseCode::Error, "the gateway failed"),
            Ended::Browser => return ended,
        };
        // Best effort, briefly: the browser may be gone already.
        let _ = self.socket.get_mut().sock.set_nonblocking(false);
        let _ = self
            .socket
            .get_mut()
            .sock
            .set_write_timeout(Some(Duration::from_secs(1)));
        let _ = self.socket.close(Some(CloseFrame {
            code,
            reason: reason.into(),
        }));
        let _ = self.socket.flush();
        ended
    }

    fn pump(&mut self) -> Ended {
        let mut setup = || -> io::Result<()> {
            self.socket.get_mut().sock.set_nonblocking(true)?;
            self.daemon.set_nonblocking(true)?;
            self.wake.set_nonblocking(true)
        };
        if let Err(error) = setup() {
            return Ended::Failed(error.to_string());
        }
        let mut from_daemon = Splitter::default();
        let mut to_daemon: Vec<u8> = Vec::new();
        // Set while the browser's socket can't take more: then nothing more
        // is read from the daemon, which waits in turn.
        let mut browser_full = false;
        let mut buffer = vec![0; 64 * 1024];
        loop {
            // The browser's messages, as far as they've arrived.
            loop {
                match self.socket.read() {
                    Ok(Message::Binary(message)) => match framed(&message) {
                        Ok(bytes) => to_daemon.extend_from_slice(&bytes),
                        Err(error) => return Ended::Failed(error.to_string()),
                    },
                    // Pings are answered when the socket next flushes.
                    Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
                    Ok(Message::Text(_)) => {
                        return Ended::Failed("a text message after the token".into());
                    }
                    Ok(Message::Close(_)) => {}
                    Err(error) if ws_would_block(&error) => break,
                    Err(_) => return Ended::Browser,
                }
            }
            // On to the daemon, as much as it takes now.
            while !to_daemon.is_empty() {
                match self.daemon.write(&to_daemon) {
                    Ok(0) => return Ended::Daemon,
                    Ok(written) => {
                        to_daemon.drain(..written);
                    }
                    Err(error) if would_block(&error) => break,
                    Err(_) => return Ended::Daemon,
                }
            }
            // The daemon's messages, while the browser keeps up: once its
            // socket can't take a message whole, the rest wait in the
            // daemon's socket, and the daemon behind them.
            'daemon: while !browser_full {
                loop {
                    match from_daemon.next_message() {
                        Ok(Some(message)) => {
                            match self.socket.write(Message::Binary(message.into())) {
                                Ok(()) => {}
                                Err(error) if ws_would_block(&error) => {
                                    browser_full = true;
                                    continue 'daemon;
                                }
                                Err(_) => return Ended::Browser,
                            }
                        }
                        Ok(None) => break,
                        Err(error) => return Ended::Failed(error.to_string()),
                    }
                }
                match self.daemon.read(&mut buffer) {
                    Ok(0) => return Ended::Daemon,
                    Ok(read) => from_daemon.push(&buffer[..read]),
                    Err(error) if would_block(&error) => break,
                    Err(_) => return Ended::Daemon,
                }
            }
            browser_full = match self.socket.flush() {
                // Messages split out but not yet sent go first.
                Ok(()) => false,
                Err(error) if ws_would_block(&error) => true,
                Err(_) => return Ended::Browser,
            };

            let now = (self.now)();
            let Ok(left) = self.expires.duration_since(now) else {
                return Ended::Expired;
            };
            if left.is_zero() {
                return Ended::Expired;
            }
            let wants_write = browser_full || self.socket.get_ref().conn.wants_write();
            let browser = PollFlags::POLLIN
                | if wants_write {
                    PollFlags::POLLOUT
                } else {
                    PollFlags::empty()
                };
            let daemon = if browser_full {
                PollFlags::empty()
            } else {
                PollFlags::POLLIN
            } | if to_daemon.is_empty() {
                PollFlags::empty()
            } else {
                PollFlags::POLLOUT
            };
            let timeout = PollTimeout::try_from(left.min(Duration::from_secs(3_600)))
                .unwrap_or(PollTimeout::MAX);
            let mut fds = [
                PollFd::new(self.socket.get_ref().sock.as_fd(), browser),
                PollFd::new(self.daemon.as_fd(), daemon),
                PollFd::new(self.wake.as_fd(), PollFlags::POLLIN),
            ];
            match poll(&mut fds, timeout) {
                Ok(_) | Err(nix::errno::Errno::EINTR) => {}
                Err(error) => return Ended::Failed(error.to_string()),
            }
            let mut why = [0];
            match self.wake.read(&mut why) {
                Ok(1) => return Ended::Closed(Closing::from_byte(why[0])),
                Ok(_) => return Ended::Closed(Closing::Stopping),
                Err(error) if would_block(&error) => {}
                Err(error) => return Ended::Failed(error.to_string()),
            }
        }
    }
}
