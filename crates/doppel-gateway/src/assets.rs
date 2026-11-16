//! The page's files, served as they are. They carry no data: everything a
//! page shows comes over its authenticated socket. Pure.

/// How long a browser may keep a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cache {
    /// Asked again each time: the page, whose name never changes.
    Revalidate,
    /// Kept: a file whose name changes with its content.
    Immutable,
}

impl Cache {
    pub(crate) const fn header(self) -> &'static str {
        match self {
            Self::Revalidate => "no-cache",
            Self::Immutable => "public, max-age=31536000, immutable",
        }
    }
}

/// One file of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Asset {
    /// Its path on the listener, from `/`.
    pub(crate) path: &'static str,
    pub(crate) content_type: &'static str,
    pub(crate) body: &'static [u8],
    pub(crate) cache: Cache,
}

/// The page's files, with `/index.html` its entry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Assets(&'static [Asset]);

impl Assets {
    /// Served until the page is built into this crate: it says so.
    pub(crate) const PLACEHOLDER: Self = Self(&[Asset {
        path: "/index.html",
        content_type: "text/html; charset=utf-8",
        body: b"<!doctype html>\n<meta charset=\"utf-8\">\n<title>doppel</title>\n\
            <p>This doppel was built without its browser viewer.</p>\n",
        cache: Cache::Revalidate,
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

    /// The file at `path`; `/` is the page.
    pub(crate) fn get(&self, path: &str) -> Option<&Asset> {
        let path = if path == "/" { "/index.html" } else { path };
        self.0.iter().find(|asset| asset.path == path)
    }
}

include!(concat!(env!("OUT_DIR"), "/built.rs"));
