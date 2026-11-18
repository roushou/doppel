//! The little HTTP the gateway speaks: one GET per connection, for one of
//! the page's files, a recording's video (with a range), or the upgrade to
//! its WebSocket. Pure: a request's head in, the response's bytes out.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha1::{Digest as _, Sha1};

use crate::assets::{Asset, Assets, CACHE_CONTROL, Encoding};
use crate::tokens::{TokenError, TokenSecret};

/// The longest request head read before giving up on it.
pub(crate) const MAX_HEAD: usize = 16 * 1024;

/// Where the page opens its WebSocket.
pub(crate) const SOCKET_PATH: &str = "/socket";

/// Where recordings' videos are: `/recordings/{folder}/{file}.mp4`.
const RECORDINGS: &str = "/recordings/";

/// The cookie a request for a video carries its token in: a `<video>`
/// can't send a header, and a URL is the wrong place for a secret.
pub(crate) const TOKEN_COOKIE: &str = "doppel-token";

/// A recording's video, as a request names it: a folder in the store's
/// recordings and an `.mp4` file in it. Each is one path component of
/// letters, digits, `-`, `_` and `.`, not starting with a dot, so it can't
/// name anything outside the recordings; the daemon decides whether they
/// are a recording and a video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoPath {
    folder: String,
    file: String,
}

impl VideoPath {
    fn parse(path: &str) -> Option<Self> {
        let rest = path.strip_prefix(RECORDINGS)?;
        let (folder, file) = rest.split_once('/')?;
        let component = |part: &str| {
            !part.is_empty()
                && !part.starts_with('.')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        };
        (component(folder) && component(file) && file.ends_with(".mp4")).then(|| Self {
            folder: folder.to_owned(),
            file: file.to_owned(),
        })
    }

    /// The recording's folder name.
    #[must_use]
    pub fn folder(&self) -> &str {
        &self.folder
    }

    /// The video's file name in it.
    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }
}

/// The bytes a request asks for, from its `Range` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wanted {
    /// No range, or one this doesn't serve (several at once): the whole
    /// file, which RFC 9110 allows.
    Whole,
    /// `bytes=first-` or `bytes=first-last`.
    From { first: u64, last: Option<u64> },
    /// `bytes=-n`: the last `n`.
    Suffix(u64),
}

impl Wanted {
    fn parse(header: Option<&str>) -> Self {
        let Some(spec) = header.and_then(|value| value.trim().strip_prefix("bytes=")) else {
            return Self::Whole;
        };
        if spec.contains(',') {
            return Self::Whole;
        }
        let number = |text: &str| -> Option<u64> {
            (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
                .then(|| text.parse().ok())
                .flatten()
        };
        let Some((first, last)) = spec.trim().split_once('-') else {
            return Self::Whole;
        };
        match (first, last) {
            ("", suffix) => number(suffix).map_or(Self::Whole, Self::Suffix),
            (first, "") => {
                number(first).map_or(Self::Whole, |first| Self::From { first, last: None })
            }
            (first, last) => match (number(first), number(last)) {
                (Some(first), Some(last)) if first <= last => Self::From {
                    first,
                    last: Some(last),
                },
                (Some(_) | None, Some(_) | None) => Self::Whole,
            },
        }
    }

    /// What of a `len`-byte file to send.
    pub(crate) fn of(self, len: u64) -> Served {
        match self {
            Self::Whole => Served::Whole { len },
            Self::From { first, .. } if first >= len => Served::Unsatisfiable { len },
            Self::From { first, last } => Served::Part {
                first,
                last: last.map_or(len - 1, |last| last.min(len - 1)),
                len,
            },
            Self::Suffix(0) => Served::Unsatisfiable { len },
            Self::Suffix(_) if len == 0 => Served::Unsatisfiable { len },
            Self::Suffix(n) => Served::Part {
                first: len.saturating_sub(n),
                last: len - 1,
                len,
            },
        }
    }
}

/// What is sent of a video, once its length is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Served {
    Whole {
        len: u64,
    },
    /// Bytes `first..=last` of `len`.
    Part {
        first: u64,
        last: u64,
        len: u64,
    },
    /// A range past its end: nothing.
    Unsatisfiable {
        len: u64,
    },
}

impl Served {
    /// Where the body starts in the file, and how many bytes it has.
    pub(crate) const fn body(self) -> (u64, u64) {
        match self {
            Self::Whole { len } => (0, len),
            Self::Part { first, last, .. } => (first, last - first + 1),
            Self::Unsatisfiable { .. } => (0, 0),
        }
    }

    /// The response's head; the body follows it.
    pub(crate) fn head(self) -> Vec<u8> {
        let common = format!(
            "Content-Type: video/mp4\r\nAccept-Ranges: bytes\r\n\
             Cache-Control: no-store\r\n{SECURITY}Connection: close\r\n\r\n"
        );
        match self {
            Self::Whole { len } => {
                format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\n{common}")
            }
            Self::Part { first, last, len } => format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {first}-{last}/{len}\r\n\
                 Content-Length: {}\r\n{common}",
                last - first + 1
            ),
            Self::Unsatisfiable { len } => format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\n\
                 Content-Length: 0\r\n{common}"
            ),
        }
        .into_bytes()
    }
}

/// A request for a recording's video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VideoRequest {
    pub(crate) path: VideoPath,
    /// The token cookie, if it came with one, parsed: the secret never
    /// travels as text past here.
    pub(crate) token: Option<Result<TokenSecret, TokenError>>,
    pub(crate) wanted: Wanted,
}

/// Headers every response carries. Scripts may compile WebAssembly (the
/// console), never evaluate strings.
const SECURITY: &str = "Content-Security-Policy: default-src 'self'; \
    script-src 'self' 'wasm-unsafe-eval'; connect-src 'self'; img-src 'self' data: blob:; \
    style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; \
    form-action 'none'\r\n\
    X-Content-Type-Options: nosniff\r\n\
    Referrer-Policy: no-referrer\r\n";

/// A request's head: its method, path (without a query) and headers, names
/// lowercased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Request {
    method: Method,
    path: String,
    headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Method {
    Get,
    Other,
}

/// How far a request's head has arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Parsed {
    Incomplete,
    Complete(Request),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HttpError {
    #[error("the request's head is over {MAX_HEAD} bytes")]
    TooLarge,
    #[error("not an HTTP/1.1 request")]
    Malformed,
}

/// What a request gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Route<'a> {
    /// A file of the page, sent as `encoding`.
    File {
        asset: &'a Asset,
        encoding: Encoding,
    },
    /// A file the browser has this version of already.
    NotModified(&'a Asset),
    /// A recording's video: answered once the token and the file are
    /// checked.
    Video(VideoRequest),
    /// The WebSocket, answered with this `Sec-WebSocket-Accept`.
    Upgrade {
        accept: String,
    },
    NotFound,
    MethodNotAllowed,
    /// An upgrade from a page this listener didn't serve.
    ForeignOrigin,
    /// A video asked for without a token that lets the asker in.
    NoToken,
    /// An upgrade missing what RFC 6455 requires.
    BadUpgrade,
}

impl Request {
    /// The head at the start of `bytes`, once it has all arrived.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Parsed, HttpError> {
        let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            return if bytes.len() > MAX_HEAD {
                Err(HttpError::TooLarge)
            } else {
                Ok(Parsed::Incomplete)
            };
        };
        if end > MAX_HEAD {
            return Err(HttpError::TooLarge);
        }
        let head = std::str::from_utf8(&bytes[..end]).map_err(|_| HttpError::Malformed)?;
        let mut lines = head.split("\r\n");
        let mut start = lines.next().ok_or(HttpError::Malformed)?.split(' ');
        let (Some(method), Some(target), Some("HTTP/1.1"), None) =
            (start.next(), start.next(), start.next(), start.next())
        else {
            return Err(HttpError::Malformed);
        };
        if !target.starts_with('/') {
            return Err(HttpError::Malformed);
        }
        let path = target.split('?').next().unwrap_or(target).to_owned();
        let headers = lines
            .map(|line| {
                let (name, value) = line.split_once(':').ok_or(HttpError::Malformed)?;
                Ok((name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            })
            .collect::<Result<_, HttpError>>()?;
        let method = if method == "GET" {
            Method::Get
        } else {
            Method::Other
        };
        Ok(Parsed::Complete(Self {
            method,
            path,
            headers,
        }))
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(have, _)| have == name)
            .map(|(_, value)| value.as_str())
    }

    /// A cookie's value, from the `Cookie` headers.
    fn cookie(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .filter(|(have, _)| have == "cookie")
            .flat_map(|(_, value)| value.split(';'))
            .find_map(|pair| {
                let (have, value) = pair.trim().split_once('=')?;
                (have == name).then_some(value)
            })
    }

    /// What this request gets from the page's `assets` and its socket.
    pub(crate) fn route<'a>(&self, assets: &'a Assets) -> Route<'a> {
        match self.method {
            Method::Get => {}
            Method::Other => return Route::MethodNotAllowed,
        }
        if let Some(path) = VideoPath::parse(&self.path) {
            return Route::Video(VideoRequest {
                path,
                token: self.cookie(TOKEN_COOKIE).map(str::parse),
                wanted: Wanted::parse(self.header("range")),
            });
        }
        if self.path != SOCKET_PATH {
            return assets.get(&self.path).map_or(Route::NotFound, |asset| {
                if asset.matches(self.header("if-none-match")) {
                    Route::NotModified(asset)
                } else {
                    Route::File {
                        asset,
                        encoding: Encoding::best(self.header("accept-encoding"), asset),
                    }
                }
            });
        }
        let has = |name, token: &str| {
            self.header(name).is_some_and(|value| {
                value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case(token))
            })
        };
        let key = self
            .header("sec-websocket-key")
            .filter(|key| STANDARD.decode(key).is_ok_and(|bytes| bytes.len() == 16));
        let (true, true, Some("13"), Some(key)) = (
            has("upgrade", "websocket"),
            has("connection", "upgrade"),
            self.header("sec-websocket-version"),
            key,
        ) else {
            return Route::BadUpgrade;
        };
        // A browser always sends the page's origin; a page from anywhere
        // else doesn't get the socket, token or not.
        let ours = self.header("host").map(|host| format!("https://{host}"));
        match (self.header("origin"), ours) {
            (Some(origin), Some(ours)) if origin == ours => Route::Upgrade {
                accept: accept_key(key),
            },
            (Some(_) | None, Some(_) | None) => Route::ForeignOrigin,
        }
    }
}

/// RFC 6455's answer to a `Sec-WebSocket-Key`.
fn accept_key(key: &str) -> String {
    const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let mut sha = Sha1::new();
    sha.update(key.as_bytes());
    sha.update(GUID.as_bytes());
    STANDARD.encode(sha.finalize())
}

impl Route<'_> {
    /// The response's bytes. Every response closes the connection, except
    /// the upgrade, after which it carries the WebSocket.
    pub(crate) fn response(&self) -> Vec<u8> {
        let plain = |status: &str, body: &str| {
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\n\
                 Content-Length: {}\r\n{SECURITY}Connection: close\r\n\r\n{body}",
                body.len()
            )
            .into_bytes()
        };
        match self {
            Self::File { asset, encoding } => {
                let body = asset.bytes(*encoding);
                let coded = encoding
                    .name()
                    .map_or_else(String::new, |name| format!("Content-Encoding: {name}\r\n"));
                let mut bytes = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\n{coded}Content-Length: {}\r\n\
                     ETag: {}\r\nCache-Control: {CACHE_CONTROL}\r\nVary: Accept-Encoding\r\n\
                     {SECURITY}Connection: close\r\n\r\n",
                    asset.content_type,
                    body.len(),
                    asset.etag,
                )
                .into_bytes();
                bytes.extend_from_slice(body);
                bytes
            }
            Self::NotModified(asset) => format!(
                "HTTP/1.1 304 Not Modified\r\nETag: {}\r\nCache-Control: {CACHE_CONTROL}\r\n\
                 Vary: Accept-Encoding\r\n{SECURITY}Connection: close\r\n\r\n",
                asset.etag,
            )
            .into_bytes(),
            Self::Upgrade { accept } => format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                 Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            )
            .into_bytes(),
            // Its head comes once the file's length is known.
            Self::Video(_) => Vec::new(),
            Self::NotFound => plain("404 Not Found", "not found\n"),
            Self::MethodNotAllowed => plain("405 Method Not Allowed", "only GET\n"),
            Self::ForeignOrigin => plain("403 Forbidden", "not from this page\n"),
            Self::NoToken => plain("403 Forbidden", "a valid token is needed\n"),
            Self::BadUpgrade => plain("400 Bad Request", "not a WebSocket upgrade\n"),
        }
    }
}

/// What to say to a request that broke off or couldn't be read.
pub(crate) fn bad_request() -> Vec<u8> {
    Route::BadUpgrade.response()
}

/// What to say to a request for a video without a valid token.
pub(crate) fn forbidden() -> Vec<u8> {
    Route::NoToken.response()
}

/// What to say to a request for a video that isn't there.
pub(crate) fn not_found() -> Vec<u8> {
    Route::NotFound.response()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: Asset = Asset {
        path: "/index.html",
        content_type: "text/html; charset=utf-8",
        etag: "\"0123abcd\"",
        body: b"<!doctype html>",
        brotli: Some(b"br-bytes"),
        gzip: None,
    };
    const PLAIN: Route<'static> = Route::File {
        asset: &PAGE,
        encoding: Encoding::Identity,
    };
    const ASSETS: Assets = Assets::new(&[PAGE]);

    fn request(head: &str) -> Request {
        match Request::parse(head.as_bytes()) {
            Ok(Parsed::Complete(request)) => request,
            other => panic!("{other:?}"),
        }
    }

    fn upgrade(origin: &str) -> String {
        format!(
            "GET /socket HTTP/1.1\r\nHost: mac.local:7443\r\nUpgrade: websocket\r\n\
             Connection: keep-alive, Upgrade\r\nSec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nOrigin: {origin}\r\n\r\n"
        )
    }

    #[test]
    fn a_head_is_read_once_it_has_all_arrived() {
        let head = "GET /?q=1 HTTP/1.1\r\nHost: mac.local:7443\r\n\r\n";
        for cut in 0..head.len() - 1 {
            assert_eq!(
                Request::parse(&head.as_bytes()[..cut]),
                Ok(Parsed::Incomplete),
                "{cut}"
            );
        }
        assert_eq!(request(head).route(&ASSETS), PLAIN);
        assert_eq!(
            Request::parse(&vec![b'a'; MAX_HEAD + 1]),
            Err(HttpError::TooLarge)
        );
        for wrong in [
            "GET / HTTP/1.0\r\n\r\n",
            "GET  HTTP/1.1\r\n\r\n",
            "GET http://x/ HTTP/1.1\r\n\r\n",
            "GET / HTTP/1.1\r\nno colon\r\n\r\n",
        ] {
            assert_eq!(
                Request::parse(wrong.as_bytes()),
                Err(HttpError::Malformed),
                "{wrong:?}"
            );
        }
    }

    #[test]
    fn files_are_found_by_path_and_only_got() {
        assert_eq!(
            request("GET /index.html HTTP/1.1\r\n\r\n").route(&ASSETS),
            PLAIN
        );
        assert_eq!(
            request("GET /tokens.json HTTP/1.1\r\n\r\n").route(&ASSETS),
            Route::NotFound
        );
        assert_eq!(
            request("POST / HTTP/1.1\r\n\r\n").route(&ASSETS),
            Route::MethodNotAllowed
        );
    }

    #[test]
    fn the_socket_is_only_for_this_listener_s_page() {
        // RFC 6455's example key and answer.
        assert_eq!(
            request(&upgrade("https://mac.local:7443")).route(&ASSETS),
            Route::Upgrade {
                accept: "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=".into()
            }
        );
        for origin in ["https://evil.example", "http://mac.local:7443", "null"] {
            assert_eq!(
                request(&upgrade(origin)).route(&ASSETS),
                Route::ForeignOrigin,
                "{origin}"
            );
        }
        let no_key = upgrade("https://mac.local:7443").replace("Sec-WebSocket-Key", "X-Key");
        assert_eq!(request(&no_key).route(&ASSETS), Route::BadUpgrade);
        let old = upgrade("https://mac.local:7443").replace("Version: 13", "Version: 8");
        assert_eq!(request(&old).route(&ASSETS), Route::BadUpgrade);
    }

    fn video(head: &str) -> VideoRequest {
        let route = request(head).route(&ASSETS);
        let Route::Video(video) = route else {
            panic!("{route:?}");
        };
        video
    }

    #[test]
    fn a_video_is_named_by_one_folder_and_one_mp4_in_it() {
        let secret = TokenSecret::from_bytes([7; TokenSecret::BYTES]);
        let asked = video(&format!(
            "GET /recordings/2026-11-14T06-15-42Z-s1/live-001.mp4 HTTP/1.1\r\n\
             Cookie: theme=dark; doppel-token={}\r\nRange: bytes=0-1\r\n\r\n",
            secret.expose()
        ));
        assert_eq!(asked.path.folder(), "2026-11-14T06-15-42Z-s1");
        assert_eq!(asked.path.file(), "live-001.mp4");
        assert_eq!(asked.token, Some(Ok(secret)));
        let malformed =
            video("GET /recordings/a/b.mp4 HTTP/1.1\r\nCookie: doppel-token=dpl_abc\r\n\r\n");
        assert_eq!(malformed.token, Some(Err(TokenError::Malformed)));
        assert_eq!(
            asked.wanted,
            Wanted::From {
                first: 0,
                last: Some(1)
            }
        );
        let bare = video("GET /recordings/a/b.mp4 HTTP/1.1\r\n\r\n");
        assert_eq!((bare.token, bare.wanted), (None, Wanted::Whole));
        for path in [
            "/recordings/a/../b.mp4",
            "/recordings/../tokens.json",
            "/recordings/a/b.vtt",
            "/recordings/a/.b.mp4",
            "/recordings/.a/b.mp4",
            "/recordings/a/b/c.mp4",
            "/recordings//b.mp4",
            "/recordings/a%2F..%2Fb/c.mp4",
        ] {
            let head = format!("GET {path} HTTP/1.1\r\n\r\n");
            assert!(
                !matches!(request(&head).route(&ASSETS), Route::Video(_)),
                "{path}"
            );
        }
    }

    #[test]
    fn a_range_is_read_and_one_this_doesn_t_serve_gets_the_whole_file() {
        let wanted = |value: &str| Wanted::parse(Some(value));
        assert_eq!(
            wanted("bytes=100-"),
            Wanted::From {
                first: 100,
                last: None
            }
        );
        assert_eq!(wanted("bytes=-500"), Wanted::Suffix(500));
        for whole in [
            "bytes=0-1,5-6",
            "bytes=5-1",
            "items=0-1",
            "bytes=a-b",
            "bytes=-",
            "bytes=1",
        ] {
            assert_eq!(wanted(whole), Wanted::Whole, "{whole}");
        }
        assert_eq!(Wanted::parse(None), Wanted::Whole);
    }

    #[test]
    fn what_is_sent_of_a_video_fits_its_length() {
        let from = |first, last| Wanted::From { first, last };
        assert_eq!(Wanted::Whole.of(10), Served::Whole { len: 10 });
        assert_eq!(
            from(0, Some(1)).of(10),
            Served::Part {
                first: 0,
                last: 1,
                len: 10
            }
        );
        // A last byte past the end is the end.
        assert_eq!(
            from(4, Some(99)).of(10),
            Served::Part {
                first: 4,
                last: 9,
                len: 10
            }
        );
        assert_eq!(
            from(4, None).of(10),
            Served::Part {
                first: 4,
                last: 9,
                len: 10
            }
        );
        assert_eq!(
            Wanted::Suffix(3).of(10),
            Served::Part {
                first: 7,
                last: 9,
                len: 10
            }
        );
        assert_eq!(
            Wanted::Suffix(30).of(10),
            Served::Part {
                first: 0,
                last: 9,
                len: 10
            }
        );
        assert_eq!(from(10, None).of(10), Served::Unsatisfiable { len: 10 });
        assert_eq!(Wanted::Suffix(0).of(10), Served::Unsatisfiable { len: 10 });
        assert_eq!(Wanted::Suffix(5).of(0), Served::Unsatisfiable { len: 0 });
        assert_eq!(
            Served::Part {
                first: 7,
                last: 9,
                len: 10
            }
            .body(),
            (7, 3)
        );
        assert_eq!(Served::Unsatisfiable { len: 10 }.body(), (0, 0));

        let head = |served: Served| String::from_utf8(served.head()).unwrap();
        let part = head(Served::Part {
            first: 7,
            last: 9,
            len: 10,
        });
        assert!(
            part.starts_with("HTTP/1.1 206 Partial Content\r\n"),
            "{part}"
        );
        assert!(part.contains("\r\nContent-Range: bytes 7-9/10\r\n"));
        assert!(part.contains("\r\nContent-Length: 3\r\n"));
        assert!(part.contains("\r\nAccept-Ranges: bytes\r\n"));
        assert!(part.contains("\r\nCache-Control: no-store\r\n"));
        assert!(part.ends_with("\r\n\r\n"));
        let past = head(Served::Unsatisfiable { len: 10 });
        assert!(past.starts_with("HTTP/1.1 416 "), "{past}");
        assert!(past.contains("\r\nContent-Range: bytes */10\r\n"));
        assert!(head(Served::Whole { len: 10 }).contains("\r\nContent-Length: 10\r\n"));
    }

    #[test]
    fn every_response_but_the_upgrade_closes_and_carries_the_policy() {
        for route in [
            PLAIN,
            Route::NotModified(&PAGE),
            Route::NotFound,
            Route::MethodNotAllowed,
            Route::ForeignOrigin,
            Route::NoToken,
            Route::BadUpgrade,
        ] {
            let response = String::from_utf8(route.response()).unwrap();
            assert!(response.contains("\r\nConnection: close\r\n"), "{response}");
            assert!(response.contains("Content-Security-Policy: default-src 'self'"));
            assert!(response.contains("frame-ancestors 'none'"));
        }
        let page = String::from_utf8(PLAIN.response()).unwrap();
        assert!(page.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(page.ends_with("\r\n\r\n<!doctype html>"));
    }

    #[test]
    fn the_page_answers_its_places() {
        for path in [
            "/",
            "/machines",
            "/machines/live",
            "/replay",
            "/replay/2026-11-17T01-55-02Z-s1",
        ] {
            let head = format!("GET {path} HTTP/1.1\r\n\r\n");
            assert_eq!(request(&head).route(&ASSETS), PLAIN, "{path}");
        }
        for path in ["/machinesx", "/replays", "/recordings"] {
            let head = format!("GET {path} HTTP/1.1\r\n\r\n");
            assert_eq!(request(&head).route(&ASSETS), Route::NotFound, "{path}");
        }
    }

    #[test]
    fn a_file_comes_compressed_when_asked_and_not_again_once_held() {
        let get = |extra: &str| request(&format!("GET / HTTP/1.1\r\n{extra}\r\n")).route(&ASSETS);
        let sent = |encoding| Route::File {
            asset: &PAGE,
            encoding,
        };
        assert_eq!(get(""), sent(Encoding::Identity));
        assert_eq!(
            get("Accept-Encoding: gzip, deflate, br\r\n"),
            sent(Encoding::Brotli)
        );
        assert_eq!(
            get("Accept-Encoding: br;q=0, gzip\r\n"),
            sent(Encoding::Identity)
        );
        assert_eq!(get("Accept-Encoding: BR;q=0.5\r\n"), sent(Encoding::Brotli));
        assert_eq!(
            get("If-None-Match: \"0123abcd\"\r\n"),
            Route::NotModified(&PAGE)
        );
        assert_eq!(
            get("If-None-Match: W/\"0123abcd\"\r\n"),
            Route::NotModified(&PAGE)
        );
        assert_eq!(get("If-None-Match: \"old\"\r\n"), sent(Encoding::Identity));

        let response = String::from_utf8(sent(Encoding::Brotli).response()).unwrap();
        assert!(
            response.contains("\r\nContent-Encoding: br\r\n"),
            "{response}"
        );
        assert!(response.contains("\r\nContent-Length: 8\r\n"));
        assert!(response.contains("\r\nETag: \"0123abcd\"\r\n"));
        assert!(response.contains("\r\nVary: Accept-Encoding\r\n"));
        assert!(response.ends_with("\r\n\r\nbr-bytes"));
        let held = String::from_utf8(Route::NotModified(&PAGE).response()).unwrap();
        assert!(held.starts_with("HTTP/1.1 304 Not Modified\r\n"), "{held}");
        assert!(held.ends_with("\r\n\r\n"));
    }
}
