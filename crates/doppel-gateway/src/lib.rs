//! Serves the web console to holders of a token, over TLS.
//!
//! One listener, one origin: the page's files over HTTPS, and its WebSocket
//! at `/socket`. A WebSocket's first message must be a token the daemon
//! issued; only then is it bridged to the daemon, which serves it as it
//! serves any connection. Blocking: a thread per connection.
//!
//! - [`tokens`]: who may connect. Pure.
//! - `http`, `framing`, `assets`: requests and responses, and messages in
//!   and out of the daemon's framing. Pure.
//! - `tls`, `bridge`, [`Gateway`]: the shell around them.

mod assets;
mod bridge;
mod framing;
mod http;
mod tls;
pub mod tokens;

use std::collections::HashMap;
use std::io::{self, Read as _, Seek as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tungstenite::{Message, WebSocket};

pub use http::VideoPath;
pub use tls::{Certificate, Fingerprint, TlsError};
pub use tokens::{
    BookError, ChangeError, Denied, Token, TokenBook, TokenDigest, TokenError, TokenId, TokenLabel,
    TokenSecret, Ttl,
};

use crate::assets::Assets;
use crate::bridge::{Bridge, Closing, Ended};
use crate::http::{Parsed, Request, Route, VideoRequest};

/// What the gateway needs of the daemon. A port: the daemon implements it,
/// tests a fake.
pub trait Connections: Sync {
    /// The wall clock, for tokens' expiry.
    fn now(&self) -> SystemTime;

    /// The token a browser presents, if it lets them in now.
    ///
    /// # Errors
    /// Why it doesn't, for the daemon's log.
    fn check(&self, secret: &TokenSecret) -> Result<Token, Refused>;

    /// Serves a browser that holds `token` on `stream`, one end of a socket
    /// pair, until it closes.
    fn serve(&self, stream: UnixStream, token: &Token);

    /// A recording's video, opened for a browser whose token let it in,
    /// with its length in bytes.
    ///
    /// # Errors
    /// It isn't a recording's video, or it can't be opened.
    fn video(&self, path: &VideoPath) -> Result<(std::fs::File, u64), NoVideo>;
}

/// Why a video isn't served, for the daemon's log; the browser gets a 404.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct NoVideo(pub String);

/// Why a presented token is refused.
#[derive(Debug, thiserror::Error)]
pub enum Refused {
    #[error(transparent)]
    Denied(#[from] Denied),
    /// The book couldn't be read: no one gets in.
    #[error("the token book can't be read: {0}")]
    Book(String),
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error(transparent)]
    Tls(#[from] TlsError),
    #[error("listening on {address}: {source}")]
    Listen {
        address: SocketAddr,
        source: io::Error,
    },
    #[error("accepting a connection: {0}")]
    Accept(io::Error),
}

/// A connection bridged to the daemon, and how to close it.
#[derive(Debug)]
struct Open {
    token: TokenId,
    wake: UnixStream,
}

/// The gateway: its TLS setup, its open connections, and whether it's
/// stopping.
#[derive(Debug)]
pub struct Gateway {
    tls: Arc<ServerConfig>,
    assets: Assets,
    open: Mutex<HashMap<u64, Open>>,
    next: AtomicU64,
    stopping: AtomicBool,
    /// Wakes the accept loop to stop it.
    wake: (UnixStream, UnixStream),
}

impl Gateway {
    /// How long a browser has to finish the TLS handshake, send its request
    /// and then its token.
    const HANDSHAKE: Duration = Duration::from_secs(10);

    /// A gateway serving with `certificate`.
    ///
    /// # Errors
    /// The certificate can't be used, or the wake socket can't be made.
    pub fn new(certificate: &Certificate) -> Result<Self, GatewayError> {
        Ok(Self {
            tls: certificate.server_config()?,
            assets: Assets::page(),
            open: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            wake: UnixStream::pair().map_err(GatewayError::Accept)?,
        })
    }

    /// Binds the listener.
    ///
    /// # Errors
    /// [`GatewayError::Listen`].
    pub fn bind(address: SocketAddr) -> Result<TcpListener, GatewayError> {
        TcpListener::bind(address).map_err(|source| GatewayError::Listen { address, source })
    }

    /// Serves `listener` until [`Self::stop`], then waits for its
    /// connections to close.
    ///
    /// # Errors
    /// [`GatewayError::Accept`]: the listener failed.
    pub fn serve(
        &self,
        listener: &TcpListener,
        connections: &dyn Connections,
    ) -> Result<(), GatewayError> {
        listener
            .set_nonblocking(true)
            .map_err(GatewayError::Accept)?;
        std::thread::scope(|scope| {
            loop {
                if self.stopping.load(Ordering::SeqCst) {
                    return Ok(());
                }
                let mut fds = [
                    PollFd::new(listener.as_fd(), PollFlags::POLLIN),
                    PollFd::new(self.wake.1.as_fd(), PollFlags::POLLIN),
                ];
                match poll(&mut fds, PollTimeout::NONE) {
                    Ok(_) | Err(nix::errno::Errno::EINTR) => {}
                    Err(error) => return Err(GatewayError::Accept(error.into())),
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        scope.spawn(move || self.connection(stream, connections));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    // One peer's failed accept (it reset, say) isn't the
                    // listener's.
                    Err(error) if error.kind() == io::ErrorKind::ConnectionAborted => {}
                    Err(error) => return Err(GatewayError::Accept(error)),
                }
            }
        })
    }

    /// Closes the connections that hold `token`: it was revoked.
    pub fn revoke(&self, token: &TokenId) {
        for open in self.open().values() {
            if open.token == *token {
                let _ = (&open.wake).write_all(&[Closing::Revoked.byte()]);
            }
        }
    }

    /// Stops accepting and closes every connection; [`Self::serve`] returns
    /// once they have.
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = (&self.wake.0).write_all(&[0]);
        for open in self.open().values() {
            let _ = (&open.wake).write_all(&[Closing::Stopping.byte()]);
        }
    }

    fn open(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Open>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn connection(&self, stream: TcpStream, connections: &dyn Connections) {
        let peer = stream
            .peer_addr()
            .map_or_else(|_| "a browser".to_owned(), |peer| peer.to_string());
        if let Err(error) = self.serve_one(stream, connections) {
            eprintln!("doppeld: web: {peer}: {error}");
        }
    }

    /// One connection: TLS, one request, and the WebSocket if that's what
    /// it asked for and its token lets it in.
    fn serve_one(&self, stream: TcpStream, connections: &dyn Connections) -> Result<(), String> {
        stream
            .set_nonblocking(false)
            .and_then(|()| stream.set_read_timeout(Some(Self::HANDSHAKE)))
            .and_then(|()| stream.set_write_timeout(Some(Self::HANDSHAKE)))
            .map_err(|error| error.to_string())?;
        let tls =
            ServerConnection::new(Arc::clone(&self.tls)).map_err(|error| error.to_string())?;
        let mut stream = StreamOwned::new(tls, stream);

        let mut head = Vec::new();
        let mut buffer = [0; 4096];
        let request = loop {
            match Request::parse(&head) {
                Ok(Parsed::Complete(request)) => break request,
                Ok(Parsed::Incomplete) => {}
                Err(error) => {
                    let _ = stream.write_all(&http::bad_request());
                    return Err(error.to_string());
                }
            }
            match stream.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(read) => head.extend_from_slice(&buffer[..read]),
                Err(error) => return Err(format!("reading its request: {error}")),
            }
        };
        let route = request.route(&self.assets);
        if let Route::Video(video) = route {
            return Self::video(stream, video, connections);
        }
        stream
            .write_all(&route.response())
            .and_then(|()| stream.flush())
            .map_err(|error| format!("answering: {error}"))?;
        let Route::Upgrade { .. } = route else {
            stream.conn.send_close_notify();
            let _ = stream.flush();
            return Ok(());
        };
        // Anything after the head is the WebSocket's.
        let end = head
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map_or(head.len(), |at| at + 4);
        let config = WebSocketConfig::default()
            .max_message_size(Some(framing::MAX_MESSAGE))
            .max_frame_size(Some(framing::MAX_MESSAGE))
            .max_write_buffer_size(4 * framing::MAX_MESSAGE);
        let mut socket = WebSocket::from_partially_read(
            stream,
            head[end..].to_vec(),
            Role::Server,
            Some(config),
        );

        let token = match Self::authenticate(&mut socket, connections) {
            Ok(token) => token,
            Err(why) => {
                let _ = socket.close(Some(CloseFrame {
                    code: CloseCode::Policy,
                    reason: "a valid token is needed".into(),
                }));
                let _ = socket.flush();
                return Err(format!("refused: {why}"));
            }
        };
        self.bridge(socket, token, connections)
    }

    /// A recording's video, or the part of it asked for, to a request whose
    /// token cookie lets it in: checked on every request, so a revoked or
    /// expired token stops a video at its next range.
    fn video(
        mut stream: StreamOwned<ServerConnection, TcpStream>,
        request: VideoRequest,
        connections: &dyn Connections,
    ) -> Result<(), String> {
        let end = |mut stream: StreamOwned<ServerConnection, TcpStream>, answer: &[u8]| {
            let _ = stream.write_all(answer);
            stream.conn.send_close_notify();
            let _ = stream.flush();
        };
        let secret = request.token.as_deref().map(str::parse::<TokenSecret>);
        let checked = match secret {
            None => Err("no token".to_owned()),
            Some(Err(error)) => Err(error.to_string()),
            Some(Ok(secret)) => connections
                .check(&secret)
                .map_err(|error| error.to_string()),
        };
        if let Err(why) = checked {
            end(stream, &http::forbidden());
            return Err(format!("video refused: {why}"));
        }
        let (file, len) = match connections.video(&request.path) {
            Ok(opened) => opened,
            Err(why) => {
                end(stream, &http::not_found());
                return Err(format!("no video: {why}"));
            }
        };
        let served = request.wanted.of(len);
        let (start, count) = served.body();
        let mut file = file;
        file.seek(io::SeekFrom::Start(start))
            .map_err(|error| format!("reading the video: {error}"))?;
        let sent = stream
            .write_all(&served.head())
            .and_then(|()| io::copy(&mut file.take(count), &mut stream).map(drop))
            .and_then(|()| stream.flush());
        match sent {
            Ok(()) => {
                end(stream, &[]);
                Ok(())
            }
            // A player drops a request it no longer needs, as when it
            // seeks: not a failure.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(format!("sending the video: {error}")),
        }
    }

    /// The token, from the socket's first message: nothing else is read
    /// from or sent to it before.
    fn authenticate(
        socket: &mut bridge::Socket,
        connections: &dyn Connections,
    ) -> Result<Token, String> {
        let message = socket.read().map_err(|error| error.to_string())?;
        let Message::Text(text) = message else {
            return Err("its first message wasn't a token".into());
        };
        let secret: TokenSecret = text
            .as_str()
            .parse()
            .map_err(|error: TokenError| error.to_string())?;
        connections
            .check(&secret)
            .map_err(|error| error.to_string())
    }

    fn bridge(
        &self,
        socket: bridge::Socket,
        token: Token,
        connections: &dyn Connections,
    ) -> Result<(), String> {
        let (ours, daemons) = UnixStream::pair().map_err(|error| error.to_string())?;
        let (wake, waker) = UnixStream::pair().map_err(|error| error.to_string())?;
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.open().insert(
            id,
            Open {
                token: token.id().clone(),
                wake: waker,
            },
        );
        // Stopped while this one authenticated: it closes at once.
        if self.stopping.load(Ordering::SeqCst)
            && let Some(open) = self.open().get(&id)
        {
            let _ = (&open.wake).write_all(&[Closing::Stopping.byte()]);
        }
        let now = || connections.now();
        let ended = std::thread::scope(|scope| {
            scope.spawn(|| connections.serve(daemons, &token));
            Bridge {
                socket,
                daemon: ours,
                wake,
                expires: token.expires(),
                now: &now,
            }
            .run()
        });
        self.open().remove(&id);
        match ended {
            Ended::Browser | Ended::Daemon | Ended::Expired | Ended::Closed(_) => Ok(()),
            Ended::Failed(why) => Err(why),
        }
    }
}
