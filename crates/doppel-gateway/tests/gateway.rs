//! The gateway on a real listener, with a fake daemon that echoes what it
//! gets, reached by a TLS WebSocket client as a browser would.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test helpers: a failed unwrap is a failed test"
)]

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use doppel_gateway::{
    Certificate, Connections, Gateway, NoVideo, Refused, Token, TokenBook, TokenSecret, Ttl,
    VideoPath,
};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use tungstenite::client::IntoClientRequest as _;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::{Message, WebSocket};

type Client = WebSocket<StreamOwned<ClientConnection, TcpStream>>;

#[expect(clippy::disallowed_methods, reason = "the fake daemon's wall clock")]
fn wall() -> SystemTime {
    SystemTime::now()
}

/// A daemon that knows `book`, echoes every byte a browser sends, and has
/// the videos in `videos`.
struct Echo {
    book: Mutex<TokenBook>,
    served: AtomicUsize,
    videos: std::path::PathBuf,
}

impl Connections for Echo {
    fn now(&self) -> SystemTime {
        wall()
    }

    fn check(&self, secret: &TokenSecret) -> Result<Token, Refused> {
        let book = self.book.lock().unwrap();
        Ok(book.check(secret, wall())?.clone())
    }

    fn video(&self, path: &VideoPath) -> Result<(std::fs::File, u64), NoVideo> {
        let file = std::fs::File::open(self.videos.join(path.folder()).join(path.file()))
            .map_err(|error| NoVideo(error.to_string()))?;
        let len = file.metadata().unwrap().len();
        Ok((file, len))
    }

    fn serve(&self, mut stream: UnixStream, _: &Token) {
        self.served.fetch_add(1, Ordering::SeqCst);
        let mut buffer = [0; 64 * 1024];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => {
                    if stream.write_all(&buffer[..read]).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

struct Running {
    gateway: Arc<Gateway>,
    daemon: Arc<Echo>,
    address: SocketAddr,
    tls: Arc<ClientConfig>,
    serving: Option<std::thread::JoinHandle<()>>,
    dir: std::path::PathBuf,
}

const TOKEN: [u8; 32] = [7; 32];

/// A video's bytes, as far as the gateway knows: it never reads them.
const VIDEO: &[u8] = b"0123456789abcdefghij";

/// A response's status line and body.
fn status_and_body(response: &str) -> (&str, &str) {
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap(), body)
}

impl Running {
    fn start(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("dg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let certificate =
            Certificate::load_or_make(&dir, &["localhost".into(), "127.0.0.1".into()], wall())
                .unwrap();
        let gateway = Arc::new(Gateway::new(&certificate).unwrap());
        let mut book = TokenBook::default();
        book.add(
            TokenSecret::from_bytes(TOKEN).digest(),
            None,
            Ttl::DEFAULT,
            wall(),
        )
        .unwrap();
        let videos = dir.join("videos");
        std::fs::create_dir_all(videos.join("rec")).unwrap();
        std::fs::write(videos.join("rec/dev-001.mp4"), VIDEO).unwrap();
        let daemon = Arc::new(Echo {
            book: Mutex::new(book),
            served: AtomicUsize::new(0),
            videos,
        });
        let listener = Gateway::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = listener.local_addr().unwrap();
        let serving = {
            let (gateway, daemon) = (Arc::clone(&gateway), Arc::clone(&daemon));
            std::thread::spawn(move || gateway.serve(&listener, &*daemon).unwrap())
        };
        let mut roots = RootCertStore::empty();
        roots.add(certificate.der().clone()).unwrap();
        let tls = Arc::new(
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        Self {
            gateway,
            daemon,
            address,
            tls,
            serving: Some(serving),
            dir,
        }
    }

    fn tls(&self) -> StreamOwned<ClientConnection, TcpStream> {
        let tcp = TcpStream::connect(self.address).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let name = ServerName::try_from("localhost").unwrap();
        StreamOwned::new(
            ClientConnection::new(Arc::clone(&self.tls), name).unwrap(),
            tcp,
        )
    }

    /// The raw response to one request.
    fn get(&self, request: &str) -> String {
        let mut stream = self.tls();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        String::from_utf8_lossy(&response).into_owned()
    }

    fn socket_from(&self, origin: &str) -> Result<Client, tungstenite::Error> {
        let url = format!("wss://localhost:{}/socket", self.address.port());
        let mut request = url.into_client_request().unwrap();
        request
            .headers_mut()
            .insert("Origin", origin.parse().unwrap());
        tungstenite::client::client(request, self.tls())
            .map(|(socket, _)| socket)
            .map_err(|error| match error {
                tungstenite::HandshakeError::Failure(error) => error,
                tungstenite::HandshakeError::Interrupted(_) => {
                    panic!("a blocking handshake was interrupted")
                }
            })
    }

    fn socket(&self) -> Client {
        self.socket_from(&format!("https://localhost:{}", self.address.port()))
            .unwrap()
    }

    /// A socket the token let in.
    fn admitted(&self) -> Client {
        let mut socket = self.socket();
        socket
            .send(Message::text(TokenSecret::from_bytes(TOKEN).expose()))
            .unwrap();
        socket
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.gateway.stop();
        if let Some(serving) = self.serving.take() {
            serving.join().unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The close the socket ends with, skipping nothing else: any other
/// message is a failure.
fn closed_with(socket: &mut Client) -> CloseCode {
    match socket.read() {
        Ok(Message::Close(Some(frame))) => frame.code,
        Ok(Message::Close(None)) => CloseCode::Status,
        Ok(
            other @ (Message::Text(_)
            | Message::Binary(_)
            | Message::Ping(_)
            | Message::Pong(_)
            | Message::Frame(_)),
        ) => {
            panic!("got {other:?} before the close")
        }
        Err(error) => panic!("no close: {error}"),
    }
}

#[test]
fn the_page_is_served_and_nothing_else_is() {
    let running = Running::start("page");
    let page = running.get("GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(page.starts_with("HTTP/1.1 200 OK\r\n"), "{page}");
    assert!(page.contains("<!doctype html>"));
    let missing = running.get("GET /tokens.json HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");
    let garbage = running.get("hello\r\n\r\n");
    assert!(garbage.starts_with("HTTP/1.1 400"), "{garbage}");
}

#[test]
fn a_socket_without_a_valid_token_gets_nothing() {
    let running = Running::start("deny");
    // Another token, well formed.
    let mut stranger = running.socket();
    stranger
        .send(Message::text(TokenSecret::from_bytes([8; 32]).expose()))
        .unwrap();
    assert_eq!(closed_with(&mut stranger), CloseCode::Policy);
    // Something other than a token first.
    let mut eager = running.socket();
    eager.send(Message::binary(vec![1, 2, 3])).unwrap();
    assert_eq!(closed_with(&mut eager), CloseCode::Policy);
    assert_eq!(running.daemon.served.load(Ordering::SeqCst), 0);
    // A page from elsewhere doesn't get the socket at all.
    match running.socket_from("https://evil.example") {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status(), 403),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_admitted_socket_reaches_the_daemon_message_by_message() {
    let running = Running::start("echo");
    let mut socket = running.admitted();
    let large: Vec<u8> = (0..3_000_000_u32).map(|n| (n % 251) as u8).collect();
    for message in [b"hello".to_vec(), Vec::new(), large] {
        socket.send(Message::binary(message.clone())).unwrap();
        match socket.read().unwrap() {
            Message::Binary(echoed) => assert_eq!(echoed.as_ref(), message.as_slice()),
            other @ (Message::Text(_)
            | Message::Ping(_)
            | Message::Pong(_)
            | Message::Close(_)
            | Message::Frame(_)) => panic!("{other:?}"),
        }
    }
    assert_eq!(running.daemon.served.load(Ordering::SeqCst), 1);
    socket.close(None).unwrap();
}

#[test]
fn a_revoked_token_s_sockets_are_closed() {
    let running = Running::start("revoke");
    let mut socket = running.admitted();
    socket.send(Message::binary(b"ping".to_vec())).unwrap();
    assert!(matches!(socket.read().unwrap(), Message::Binary(_)));
    let id = TokenSecret::from_bytes(TOKEN).digest().id();
    running.daemon.book.lock().unwrap().revoke(&id).unwrap();
    running.gateway.revoke(&id);
    assert_eq!(closed_with(&mut socket), CloseCode::Policy);
    // And it lets no one in again.
    let mut again = running.admitted();
    assert_eq!(closed_with(&mut again), CloseCode::Policy);
}

#[test]
fn a_socket_closes_when_its_token_expires() {
    let running = Running::start("expire");
    let secret = TokenSecret::from_bytes([9; 32]);
    // Issued a little under a minute ago: it expires within two seconds.
    running
        .daemon
        .book
        .lock()
        .unwrap()
        .add(
            secret.digest(),
            None,
            Ttl::from_secs(60).unwrap(),
            wall() - Duration::from_millis(58_500),
        )
        .unwrap();
    let mut socket = running.socket();
    socket.send(Message::text(secret.expose())).unwrap();
    assert_eq!(closed_with(&mut socket), CloseCode::Policy);
}

#[test]
fn stopping_closes_every_socket_and_returns() {
    let mut running = Running::start("stop");
    let mut socket = running.admitted();
    socket.send(Message::binary(b"ping".to_vec())).unwrap();
    assert!(matches!(socket.read().unwrap(), Message::Binary(_)));
    running.gateway.stop();
    assert_eq!(closed_with(&mut socket), CloseCode::Away);
    running.serving.take().unwrap().join().unwrap();
}

#[test]
fn a_video_goes_to_a_token_s_cookie_by_ranges() {
    let running = Running::start("video");
    let token = TokenSecret::from_bytes(TOKEN);
    let ask = |cookie: &str, range: &str, path: &str| {
        running.get(&format!(
            "GET /recordings/{path} HTTP/1.1\r\nHost: localhost\r\n{cookie}{range}\r\n"
        ))
    };
    let ours = format!("Cookie: doppel-token={}\r\n", token.expose());
    let video = "rec/dev-001.mp4";

    let whole = ask(&ours, "", video);
    assert_eq!(
        status_and_body(&whole),
        ("HTTP/1.1 200 OK", "0123456789abcdefghij")
    );
    let part = ask(&ours, "Range: bytes=5-9\r\n", video);
    assert_eq!(
        status_and_body(&part),
        ("HTTP/1.1 206 Partial Content", "56789")
    );
    assert!(part.contains("Content-Range: bytes 5-9/20\r\n"), "{part}");
    let tail = ask(&ours, "Range: bytes=-3\r\n", video);
    assert_eq!(status_and_body(&tail).1, "hij");
    let past = ask(&ours, "Range: bytes=20-\r\n", video);
    assert_eq!(
        status_and_body(&past),
        ("HTTP/1.1 416 Range Not Satisfiable", "")
    );
    let missing = ask(&ours, "", "rec/dev-002.mp4");
    assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");

    // No cookie, another token, or one revoked since: nothing.
    let stranger = format!(
        "Cookie: doppel-token={}\r\n",
        TokenSecret::from_bytes([8; 32]).expose()
    );
    for cookie in [
        String::new(),
        stranger,
        "Cookie: doppel-token=nonsense\r\n".into(),
    ] {
        let refused = ask(&cookie, "", video);
        assert!(refused.starts_with("HTTP/1.1 403"), "{cookie}: {refused}");
        assert!(!refused.contains("0123"), "{refused}");
    }
    let id = token.digest().id();
    running.daemon.book.lock().unwrap().revoke(&id).unwrap();
    let revoked = ask(&ours, "Range: bytes=0-1\r\n", video);
    assert!(revoked.starts_with("HTTP/1.1 403"), "{revoked}");
}
