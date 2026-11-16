//! An in-memory filesystem that performs [`FileOp`]s with the semantics the
//! shell's `LocalFilesystem` has, so plans can be crashed at every step
//! without touching a disk.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::FileOp;

/// Directories and files; a file's content is a short label.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Disk {
    dirs: BTreeSet<PathBuf>,
    files: BTreeMap<PathBuf, String>,
}

impl Disk {
    /// Writes a file, creating its directories.
    pub(crate) fn write(&mut self, path: impl Into<PathBuf>, content: &str) {
        let path = path.into();
        self.make_parents(&path);
        self.files.insert(path, content.to_owned());
    }

    pub(crate) fn read(&self, path: &Path) -> Option<&str> {
        self.files.get(path).map(String::as_str)
    }

    pub(crate) fn exists(&self, path: &Path) -> bool {
        self.dirs.contains(path) || self.files.contains_key(path)
    }

    /// Whether anything at all exists at or beneath `path`.
    pub(crate) fn any_under(&self, path: &Path) -> bool {
        self.dirs.iter().any(|d| d.starts_with(path))
            || self.files.keys().any(|f| f.starts_with(path))
    }

    /// The files beneath `dir`, relative to it.
    pub(crate) fn files_under(&self, dir: &Path) -> BTreeMap<PathBuf, String> {
        self.files
            .iter()
            .filter_map(|(path, content)| {
                path.strip_prefix(dir)
                    .ok()
                    .map(|rel| (rel.to_owned(), content.clone()))
            })
            .collect()
    }

    pub(crate) fn present(&self, probes: &[PathBuf]) -> BTreeSet<PathBuf> {
        probes.iter().filter(|p| self.exists(p)).cloned().collect()
    }

    /// Performs `ops` in order, stopping at the first failure: what the
    /// shell does. Returns how many were performed.
    pub(crate) fn run(&mut self, ops: &[FileOp]) -> Result<usize, String> {
        for (done, op) in ops.iter().enumerate() {
            self.apply(op)
                .map_err(|e| format!("op {done} {op:?}: {e}"))?;
        }
        Ok(ops.len())
    }

    fn make_parents(&mut self, path: &Path) {
        for ancestor in path.ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() {
                break;
            }
            self.dirs.insert(ancestor.to_owned());
        }
    }

    fn parent_exists(&self, path: &Path) -> bool {
        path.parent()
            .is_none_or(|p| p.as_os_str().is_empty() || self.dirs.contains(p))
    }

    pub(crate) fn apply(&mut self, op: &FileOp) -> Result<(), &'static str> {
        match op {
            FileOp::CreateDir { path } => {
                if self.exists(path) {
                    return Err("exists");
                }
                self.make_parents(path);
                self.dirs.insert(path.clone());
            }
            FileOp::Clone { from, to } => {
                let content = self.files.get(from).ok_or("no source file")?.clone();
                if self.exists(to) {
                    return Err("destination exists");
                }
                if !self.parent_exists(to) {
                    return Err("no parent");
                }
                self.files.insert(to.clone(), content);
            }
            FileOp::Rename { from, to } => {
                if let Some(content) = self.files.get(from).cloned() {
                    if self.dirs.contains(to) || !self.parent_exists(to) {
                        return Err("bad destination");
                    }
                    self.files.remove(from);
                    self.files.insert(to.clone(), content);
                } else if self.dirs.contains(from) {
                    // rename(2) replaces only an empty directory.
                    if self.files.contains_key(to)
                        || (self.dirs.contains(to) && self.any_under_except_self(to))
                        || !self.parent_exists(to)
                    {
                        return Err("bad destination");
                    }
                    let moved = |p: &Path| to.join(p.strip_prefix(from).unwrap_or(p));
                    self.dirs = self
                        .dirs
                        .iter()
                        .map(|d| {
                            if d.starts_with(from) {
                                moved(d)
                            } else {
                                d.clone()
                            }
                        })
                        .collect();
                    self.files = std::mem::take(&mut self.files)
                        .into_iter()
                        .map(|(f, c)| {
                            if f.starts_with(from) {
                                (moved(&f), c)
                            } else {
                                (f, c)
                            }
                        })
                        .collect();
                } else {
                    return Err("no source");
                }
            }
            FileOp::Flush { path } => {
                if !self.files.contains_key(path) {
                    return Err("no file");
                }
            }
            FileOp::Remove { path } => {
                self.files.remove(path).ok_or("no file")?;
            }
            FileOp::CreateFile { path } => {
                if self.exists(path) {
                    return Err("exists");
                }
                if !self.parent_exists(path) {
                    return Err("no parent");
                }
                self.files.insert(path.clone(), String::new());
            }
            FileOp::RemoveTree { path } => {
                if !self.dirs.contains(path) {
                    return Err("no directory");
                }
                self.dirs.retain(|d| !d.starts_with(path));
                self.files.retain(|f, _| !f.starts_with(path));
            }
        }
        Ok(())
    }

    fn any_under_except_self(&self, dir: &Path) -> bool {
        self.dirs.iter().any(|d| d != dir && d.starts_with(dir))
            || self.files.keys().any(|f| f.starts_with(dir))
    }
}
