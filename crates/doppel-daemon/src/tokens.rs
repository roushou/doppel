//! The token book's file, `tokens.json` in the data root: read for every
//! check, so a token revoked by any process stops working at once, and
//! rewritten whole for every change.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use doppel_gateway::{BookError, ChangeError, TokenBook};
use doppel_machine::outcome::{Certainty, Failure};

/// Where the book lives, and a lock so changes from two connections don't
/// lose one another.
#[derive(Debug)]
pub(crate) struct TokenFile {
    path: PathBuf,
    changing: Mutex<()>,
}

/// Why the book couldn't be read or changed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TokenFileError {
    #[error("reading {}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: {source}", path.display())]
    Book { path: PathBuf, source: BookError },
    #[error("writing {}: {source}", path.display())]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Change(#[from] ChangeError),
}

impl TokenFile {
    const NAME: &'static str = "tokens.json";

    pub(crate) fn in_root(root: &Path) -> Self {
        Self {
            path: root.join(Self::NAME),
            changing: Mutex::new(()),
        }
    }

    /// The book as it is on disk; none yet is an empty one.
    pub(crate) fn read(&self) -> Result<TokenBook, TokenFileError> {
        match fs::read(&self.path) {
            Ok(bytes) => TokenBook::parse(&bytes).map_err(|source| TokenFileError::Book {
                path: self.path.clone(),
                source,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(TokenBook::default()),
            Err(source) => Err(TokenFileError::Read {
                path: self.path.clone(),
                source,
            }),
        }
    }

    /// Changes the book and writes it, if `change` succeeds. The file is
    /// replaced whole (written beside it, then renamed), readable only by
    /// this user, so a crash leaves the old book or the new one.
    pub(crate) fn change<T>(
        &self,
        change: impl FnOnce(&mut TokenBook) -> Result<T, ChangeError>,
    ) -> Result<T, TokenFileError> {
        let _changing = self.changing.lock().unwrap_or_else(PoisonError::into_inner);
        let mut book = self.read()?;
        let changed = change(&mut book)?;
        let staged = self.path.with_extension("json.new");
        let write = || -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&staged)?;
            file.write_all(book.to_json().as_bytes())?;
            file.sync_all()?;
            fs::rename(&staged, &self.path)
        };
        write().map_err(|source| TokenFileError::Write {
            path: self.path.clone(),
            source,
        })?;
        Ok(changed)
    }
}

/// Only a failed write may have changed the book.
impl Failure for TokenFileError {
    fn certainty(&self) -> Certainty {
        match self {
            Self::Read { .. } | Self::Book { .. } | Self::Change(_) => Certainty::NotDone,
            Self::Write { .. } => Certainty::MaybeDone,
        }
    }
}
