//! The page's files, served as they are. They carry no data: everything a
//! page shows comes over its authenticated socket. Pure.

/// How a browser may keep a file: it asks again each time, and a file
/// whose [`Asset::etag`] it already has comes back as "not modified".
pub(crate) const CACHE_CONTROL: &str = "no-cache";

/// One file of the page, with the compressed copies the build made of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Asset {
    /// Its path on the listener, from `/`.
    pub(crate) path: &'static str,
    pub(crate) content_type: &'static str,
    /// A quoted hash of its bytes, which changes whenever they do.
    pub(crate) etag: &'static str,
    pub(crate) body: &'static [u8],
    /// The same bytes compressed with brotli, when that's smaller.
    pub(crate) brotli: Option<&'static [u8]>,
    /// The same bytes compressed with gzip, when that's smaller.
    pub(crate) gzip: Option<&'static [u8]>,
}

/// How a file's bytes are sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    Brotli,
    Gzip,
    Identity,
}

impl Encoding {
    /// Its `Content-Encoding` name; `None` for the bytes as they are.
    pub(crate) const fn name(self) -> Option<&'static str> {
        match self {
            Self::Brotli => Some("br"),
            Self::Gzip => Some("gzip"),
            Self::Identity => None,
        }
    }

    /// The smallest copy of `asset` a request's `Accept-Encoding` takes.
    pub(crate) fn best(accept: Option<&str>, asset: &Asset) -> Self {
        let takes = |name: &str| {
            accept.is_some_and(|accept| {
                accept.split(',').any(|offer| {
                    let mut parts = offer.split(';').map(str::trim);
                    let coding = parts.next().unwrap_or_default();
                    // `q=0` refuses it.
                    let refused = parts.any(|param| {
                        param
                            .strip_prefix("q=")
                            .and_then(|q| q.parse::<f32>().ok())
                            .is_some_and(|q| q <= 0.0)
                    });
                    coding.eq_ignore_ascii_case(name) && !refused
                })
            })
        };
        match (asset.brotli, asset.gzip) {
            (Some(_), _) if takes("br") => Self::Brotli,
            (_, Some(_)) if takes("gzip") => Self::Gzip,
            (Some(_) | None, Some(_) | None) => Self::Identity,
        }
    }
}

impl Asset {
    /// Its bytes, sent as `encoding`; the plain bytes when it has no such
    /// copy.
    pub(crate) fn bytes(&self, encoding: Encoding) -> &'static [u8] {
        match encoding {
            Encoding::Brotli => self.brotli.unwrap_or(self.body),
            Encoding::Gzip => self.gzip.unwrap_or(self.body),
            Encoding::Identity => self.body,
        }
    }

    /// Whether a request's `If-None-Match` names this version.
    pub(crate) fn matches(&self, if_none_match: Option<&str>) -> bool {
        if_none_match.is_some_and(|tags| {
            tags.split(',')
                .map(str::trim)
                .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == self.etag)
        })
    }
}

/// The page's files, with `/index.html` its entry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Assets(&'static [Asset]);

impl Assets {
    /// Served until the page is built into this crate: it says so.
    pub(crate) const PLACEHOLDER: Self = Self::new(&[Asset {
        path: "/index.html",
        content_type: "text/html; charset=utf-8",
        body: b"<!doctype html>\n<meta charset=\"utf-8\">\n<title>doppel</title>\n\
            <p>This doppel was built without its browser viewer: <code>scripts/build-web.sh</code> builds it.</p>\n",
        etag: "\"placeholder\"",
        brotli: None,
        gzip: None,
    }]);

    pub(crate) const fn new(files: &'static [Asset]) -> Self {
        Self(files)
    }

    /// The page built into this doppel, or the placeholder.
    pub(crate) const fn page() -> Self {
        match BUILT {
            Some(built) => built,
            None => Self::PLACEHOLDER,
        }
    }

    /// The file at `path`. The page answers `/` and every place in it
    /// (`/machines/…`, `/replay/…`), which it reads from its address.
    pub(crate) fn get(&self, path: &str) -> Option<&Asset> {
        let place = |prefix: &str| {
            path.strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        };
        let path = if path == "/" || place("/machines") || place("/replay") {
            "/index.html"
        } else {
            path
        };
        self.0.iter().find(|asset| asset.path == path)
    }
}

include!(concat!(env!("OUT_DIR"), "/built.rs"));
