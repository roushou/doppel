//! Performs the core's [`FileOp`]s. The filesystem is a capability.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use doppel_machine::bundle::FileOp;

#[derive(Debug, thiserror::Error)]
#[error("{op} {path}: {source}")]
pub struct FileOpError {
    pub op: &'static str,
    pub path: PathBuf,
    #[source]
    pub source: io::Error,
}

pub trait Filesystem {
    /// # Errors
    /// The operation failed. Earlier operations in a batch are not undone.
    fn apply(&self, op: &FileOp) -> Result<(), FileOpError>;

    /// Whether anything (file or directory) is at `path`.
    ///
    /// # Errors
    /// Existence couldn't be determined.
    fn exists(&self, path: &Path) -> Result<bool, FileOpError>;

    /// The `probes` that exist.
    ///
    /// # Errors
    /// The first probe whose existence couldn't be determined.
    fn present(&self, probes: Vec<PathBuf>) -> Result<BTreeSet<PathBuf>, FileOpError> {
        let mut present = BTreeSet::new();
        for path in probes {
            if self.exists(&path)? {
                present.insert(path);
            }
        }
        Ok(present)
    }

    /// # Errors
    /// The first failing operation.
    fn apply_all(&self, ops: &[FileOp]) -> Result<(), FileOpError> {
        ops.iter().try_for_each(|op| self.apply(op))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalFilesystem;

impl LocalFilesystem {
    fn flush(file: &File, path: &Path) -> Result<(), FileOpError> {
        nix::fcntl::fcntl(file, nix::fcntl::FcntlArg::F_FULLFSYNC)
            .map(|_| ())
            .map_err(|errno| FileOpError {
                op: "flush",
                path: path.to_owned(),
                source: errno.into(),
            })
    }

    fn wrap<'a>(op: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> FileOpError + 'a {
        move |source| FileOpError {
            op,
            path: path.to_owned(),
            source,
        }
    }
}

impl Filesystem for LocalFilesystem {
    fn apply(&self, op: &FileOp) -> Result<(), FileOpError> {
        match op {
            FileOp::CreateDir { path } => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(Self::wrap("create", parent))?;
                }
                fs::create_dir(path).map_err(Self::wrap("create", path))
            }
            // clonefile(2). No byte-copy fallback: on a non-APFS volume this fails
            // instead of silently copying 64 GiB.
            FileOp::Clone { from, to } => {
                reflink_copy::reflink(from, to).map_err(Self::wrap("clone", from))
            }
            FileOp::Rename { from, to } => fs::rename(from, to).map_err(Self::wrap("rename", from)),
            FileOp::Remove { path } => fs::remove_file(path).map_err(Self::wrap("remove", path)),
            FileOp::Flush { path } => {
                let file = File::open(path).map_err(Self::wrap("open", path))?;
                Self::flush(&file, path)
            }
            FileOp::CreateFile { path } => {
                let file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(Self::wrap("create", path))?;
                Self::flush(&file, path)
            }
            FileOp::RemoveTree { path } => {
                fs::remove_dir_all(path).map_err(Self::wrap("remove", path))
            }
        }
    }

    fn exists(&self, path: &Path) -> Result<bool, FileOpError> {
        // Not `Path::exists`: an unreadable parent must not read as "absent".
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(FileOpError {
                op: "inspect",
                path: path.to_owned(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_rename_flush_on_apfs() {
        let dir = std::env::temp_dir().join(format!("doppel-files-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let fs_ = LocalFilesystem;
        let a = dir.join("sub/a");
        fs_.apply_all(&[FileOp::CreateDir {
            path: dir.join("sub"),
        }])
        .unwrap();
        fs::write(&a, b"data").unwrap();
        fs_.apply_all(&[
            FileOp::Flush { path: a.clone() },
            FileOp::Clone {
                from: a.clone(),
                to: dir.join("sub/b"),
            },
            FileOp::Rename {
                from: dir.join("sub/b"),
                to: dir.join("sub/c"),
            },
        ])
        .unwrap();
        assert_eq!(fs::read(dir.join("sub/c")).unwrap(), b"data");
        // A marker is created once, empty.
        let marker = dir.join("sub/marker");
        fs_.apply(&FileOp::CreateFile {
            path: marker.clone(),
        })
        .unwrap();
        assert_eq!(fs::read(&marker).unwrap(), b"");
        assert!(fs_.apply(&FileOp::CreateFile { path: marker }).is_err());
        assert!(fs_.exists(&dir.join("sub")).unwrap());
        assert!(!fs_.exists(&dir.join("nothing")).unwrap());
        // Clone never overwrites.
        assert!(
            fs_.apply(&FileOp::Clone {
                from: a.clone(),
                to: dir.join("sub/c")
            })
            .is_err()
        );
        // CreateDir requires a new directory.
        assert!(
            fs_.apply(&FileOp::CreateDir {
                path: dir.join("sub")
            })
            .is_err()
        );
        fs_.apply(&FileOp::RemoveTree {
            path: dir.join("sub"),
        })
        .unwrap();
        assert!(!fs_.exists(&dir.join("sub")).unwrap());
        fs::remove_dir_all(&dir).unwrap();
    }
}
