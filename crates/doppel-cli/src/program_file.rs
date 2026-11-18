//! The file `doppel` runs from, to tell when an upgrade replaced it: the
//! daemon's [`doppel_daemon::Program`].
//!
//! Watched by the path it was started by, links and all: Homebrew's
//! `bin/doppel` links to one version's folder, and an upgrade points it at
//! the next one's and removes the old. A rebuild in place makes a new file
//! at the same path.

use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;

use anyhow::{Context as _, Result};

/// Which file a path leads to, and its last change: another file, or the
/// same one rewritten, is another identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    modified_ns: i128,
    bytes: u64,
}

impl FileIdentity {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            modified_ns: i128::from(metadata.mtime()) * 1_000_000_000
                + i128::from(metadata.mtime_nsec()),
            bytes: metadata.size(),
        }
    }
}

/// This process's program, as its start path led to it when it started.
#[derive(Debug)]
pub(crate) struct ProgramFile {
    path: PathBuf,
    started: FileIdentity,
}

impl ProgramFile {
    /// This process's, now.
    pub(crate) fn current() -> Result<Self> {
        let path = std::env::current_exe().context("finding this program")?;
        let metadata =
            std::fs::metadata(&path).with_context(|| format!("reading {}", path.display()))?;
        Ok(Self {
            started: FileIdentity::of(&metadata),
            path,
        })
    }
}

impl doppel_daemon::Program for ProgramFile {
    /// Replaced when the path leads to another file, or to none: an
    /// unreadable path is taken as an uninstall.
    fn replaced(&self) -> bool {
        std::fs::metadata(&self.path)
            .map_or(true, |metadata| FileIdentity::of(&metadata) != self.started)
    }
}

#[cfg(test)]
mod tests {
    use doppel_daemon::Program as _;

    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("doppel-program-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn watching(path: PathBuf) -> ProgramFile {
        ProgramFile {
            started: FileIdentity::of(&std::fs::metadata(&path).unwrap()),
            path,
        }
    }

    #[test]
    fn a_link_moved_to_another_version_is_a_replacement() {
        let dir = scratch("link");
        std::fs::write(dir.join("old"), b"old").unwrap();
        std::fs::write(dir.join("new"), b"new").unwrap();
        std::os::unix::fs::symlink(dir.join("old"), dir.join("doppel")).unwrap();
        let program = watching(dir.join("doppel"));
        assert!(!program.replaced());
        std::fs::remove_file(dir.join("doppel")).unwrap();
        std::os::unix::fs::symlink(dir.join("new"), dir.join("doppel")).unwrap();
        assert!(program.replaced());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_new_file_at_the_path_or_none_is_a_replacement() {
        let dir = scratch("file");
        std::fs::write(dir.join("doppel"), b"one").unwrap();
        let program = watching(dir.join("doppel"));
        std::fs::write(dir.join("next"), b"two").unwrap();
        std::fs::rename(dir.join("next"), dir.join("doppel")).unwrap();
        assert!(program.replaced());
        let gone = watching(dir.join("doppel"));
        std::fs::remove_file(dir.join("doppel")).unwrap();
        assert!(gone.replaced());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
