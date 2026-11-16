//! Where a machine's files live, and the file operations that fork or roll
//! back a machine. Pure path arithmetic: the shell performs the operations.
//!
//! ```text
//! <data root>/machines/<name>/
//!   spec.pb              doppel.bundle.v1.MachineSpec (protobuf)
//!   disk.img             sparse guest disk
//!   aux.bin              VZ auxiliary storage (NVRAM)
//!   suspended.vzsave     memory state to restore on next start (optional)
//!   vmm.log
//!   login-password       guest login password, mode 600 (optional)
//!   ssh-known-hosts      the guest's SSH host keys, from `agent bootstrap` (optional)
//!   lineage.pb           doppel.bundle.v1.Lineage: role and origin (optional: absent
//!                        before it was recorded; see [`crate::lineage`])
//!   checkpoints/<label>/{spec.pb, disk.img, aux.bin, memory.vzsave}
//! <data root>/machines/.<name>.lock   held by whoever is changing <name>
//! <runtime dir>/<name>.sock   VMM control socket
//! ```
//!
//! Invariant: a memory state file is only ever paired with the disk captured at
//! the same instant. It is consumed (deleted) by the VMM the moment it's
//! restored, before the guest runs and the disk diverges.
//!
//! Crash safety: every multi-file change either commits with one `rename` or
//! writes a commit marker first, so an interruption at any point leaves
//! [`Leftovers`] that settle to the state before or the state after, never a
//! mix. New bundles are assembled in `.<name>.staging` and checkpoints in
//! `checkpoints/.staging`; a deleted checkpoint leaves through
//! `checkpoints/.deleting`. Dot names are invisible to listings.

use crate::login::Credentials;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::machine::MachineName;
use crate::vm::RunState;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error("{0:?} must be an absolute path")]
    RelativePath(PathBuf),
    #[error(
        "socket path {path:?} is {len} bytes; unix sockets allow at most {max}. Use a shorter runtime directory"
    )]
    SocketPathTooLong {
        path: PathBuf,
        len: usize,
        max: usize,
    },
    #[error(
        "checkpoint labels are 1–64 characters of lowercase letters, digits, '-' or '_' (got {0:?})"
    )]
    InvalidLabel(String),
}

/// Root of all persisted machine data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataRoot(PathBuf);

impl DataRoot {
    /// # Errors
    /// [`LayoutError::RelativePath`].
    pub fn new(path: PathBuf) -> Result<Self, LayoutError> {
        if path.is_absolute() {
            Ok(Self(path))
        } else {
            Err(LayoutError::RelativePath(path))
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    #[must_use]
    pub fn machines_dir(&self) -> PathBuf {
        self.0.join("machines")
    }

    #[must_use]
    pub fn machine(&self, name: &MachineName) -> MachineBundle {
        MachineBundle {
            name: name.clone(),
            dir: self.machines_dir().join(name.as_str()),
        }
    }

    /// Where a machine goes while it is deleted: out of `machines/`, so it
    /// leaves the list whole, and into `deleting/`, where nothing reads.
    #[must_use]
    pub fn deleting(&self, name: &MachineName) -> PathBuf {
        self.0.join("deleting").join(name.as_str())
    }

    /// The daemon's socket: beside the machines it serves, where it lasts
    /// (launchd binds it at login, and a temporary directory may be
    /// cleaned in between), and where one store has exactly one.
    ///
    /// # Errors
    /// [`LayoutError::SocketPathTooLong`] for a long data root.
    pub fn daemon_socket(&self) -> Result<SocketPath, LayoutError> {
        RuntimeDir::checked(self.0.join("doppeld.socket"))
    }
}

/// Directory for sockets. Kept separate from the data root because unix socket
/// paths are limited to 104 bytes and data roots can be long.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDir(PathBuf);

impl RuntimeDir {
    /// `sockaddr_un.sun_path` is 104 bytes on macOS, including the NUL.
    pub const MAX_SOCKET_PATH: usize = 103;

    /// # Errors
    /// [`LayoutError::RelativePath`].
    pub fn new(path: PathBuf) -> Result<Self, LayoutError> {
        if path.is_absolute() {
            Ok(Self(path))
        } else {
            Err(LayoutError::RelativePath(path))
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// # Errors
    /// [`LayoutError::SocketPathTooLong`].
    pub fn vmm_socket(&self, name: &MachineName) -> Result<SocketPath, LayoutError> {
        Self::checked(self.0.join(format!("{name}.sock")))
    }

    fn checked(path: PathBuf) -> Result<SocketPath, LayoutError> {
        let len = path.as_os_str().len();
        if len > Self::MAX_SOCKET_PATH {
            return Err(LayoutError::SocketPathTooLong {
                path,
                len,
                max: Self::MAX_SOCKET_PATH,
            });
        }
        Ok(SocketPath(path))
    }
}

/// A unix socket path known to fit in `sockaddr_un`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketPath(PathBuf);

impl SocketPath {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CheckpointLabel(String);

impl CheckpointLabel {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CheckpointLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CheckpointLabel {
    type Err = LayoutError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid = !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if valid {
            Ok(Self(s.to_owned()))
        } else {
            Err(LayoutError::InvalidLabel(s.to_owned()))
        }
    }
}

/// The files that make up one bootable machine image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageFiles {
    pub spec: PathBuf,
    pub disk: PathBuf,
    pub aux: PathBuf,
}

impl ImageFiles {
    fn in_dir(dir: &Path) -> Self {
        Self {
            spec: dir.join("spec.pb"),
            disk: dir.join("disk.img"),
            aux: dir.join("aux.bin"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineBundle {
    name: MachineName,
    dir: PathBuf,
}

impl MachineBundle {
    #[must_use]
    pub fn name(&self) -> &MachineName {
        &self.name
    }
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Beside the bundle, not in it, so a name can be claimed before its
    /// bundle exists (create, import, fork).
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.dir.with_file_name(format!(".{}.lock", self.name))
    }

    /// Where a new bundle is assembled before one rename commits it.
    fn staging(&self) -> Self {
        Self {
            name: self.name.clone(),
            dir: self.dir.with_file_name(format!(".{}.staging", self.name)),
        }
    }

    /// Where a checkpoint is written before one rename commits it under its
    /// label. One at a time per machine: whoever writes it holds the lock.
    #[must_use]
    pub fn checkpoint_staging(&self) -> CheckpointFiles {
        CheckpointFiles::in_dir(self.checkpoints_dir().join(".staging"))
    }

    /// Where a deleted checkpoint goes, in one rename, before it is removed.
    /// One at a time per machine: whoever deletes it holds the lock.
    fn checkpoint_trash(&self) -> PathBuf {
        self.checkpoints_dir().join(".deleting")
    }

    /// Present while a restore replaces the image: every staged file is
    /// complete, and the restore must be finished, never undone.
    fn restore_marker(&self) -> PathBuf {
        self.dir.join("restore.commit")
    }

    /// (staged, final) for each file a restore replaces.
    fn restore_pairs(&self) -> [(PathBuf, PathBuf); 4] {
        let image = self.image();
        let staged = |path: &Path| path.with_extension("restoring");
        // The spec too: a memory state only restores into the configuration it was saved with.
        [image.spec, image.disk, image.aux, self.suspended_state()].map(|to| (staged(&to), to))
    }
    #[must_use]
    pub fn image(&self) -> ImageFiles {
        ImageFiles::in_dir(&self.dir)
    }
    #[must_use]
    pub fn suspended_state(&self) -> PathBuf {
        self.dir.join("suspended.vzsave")
    }
    #[must_use]
    pub fn log(&self) -> PathBuf {
        self.dir.join("vmm.log")
    }
    /// The guest user's login password, readable by the owner only
    /// (`doppel set-password`); see [`crate::login`].
    #[must_use]
    pub fn login_password(&self) -> PathBuf {
        self.dir.join("login-password")
    }
    /// The guest's SSH host keys, trusted on first use by `agent bootstrap`.
    #[must_use]
    pub fn ssh_known_hosts(&self) -> PathBuf {
        self.dir.join("ssh-known-hosts")
    }
    /// The machine's role and origin ([`crate::lineage`]). Machine-wide,
    /// not part of an image: checkpoints don't copy it and restores leave
    /// it alone.
    #[must_use]
    pub fn lineage(&self) -> PathBuf {
        self.dir.join("lineage.pb")
    }
    #[must_use]
    pub fn checkpoints_dir(&self) -> PathBuf {
        self.dir.join("checkpoints")
    }
    #[must_use]
    pub fn checkpoint(&self, label: &CheckpointLabel) -> CheckpointFiles {
        CheckpointFiles::in_dir(self.checkpoints_dir().join(label.as_str()))
    }

    /// Copy-on-write clones a checkpoint into a new machine that resumes
    /// exactly where the checkpoint was taken. The source is untouched. A
    /// stored login password comes along: the fork is the same guest user.
    ///
    /// The fork is assembled out of sight and appears with its last
    /// operation, a rename, so it never exists half-made: the shell runs
    /// `build`, writes the fork's lineage at `lineage`, then runs `commit`.
    pub fn fork_from(
        &self,
        checkpoint: &CheckpointFiles,
        into: &Self,
        login: Credentials,
    ) -> CloneAssembly {
        let staging = into.staging();
        let to = staging.image();
        let mut build = vec![
            FileOp::CreateDir {
                path: staging.dir.clone(),
            },
            FileOp::Clone {
                from: checkpoint.image.spec.clone(),
                to: to.spec,
            },
            FileOp::Clone {
                from: checkpoint.image.disk.clone(),
                to: to.disk,
            },
            FileOp::Clone {
                from: checkpoint.image.aux.clone(),
                to: to.aux,
            },
            FileOp::Clone {
                from: checkpoint.memory.clone(),
                to: staging.suspended_state(),
            },
        ];
        match login {
            Credentials::Stored => build.push(FileOp::Clone {
                from: self.login_password(),
                to: staging.login_password(),
            }),
            Credentials::None => {}
        }
        CloneAssembly {
            build,
            lineage: staging.lineage(),
            commit: FileOp::Rename {
                from: staging.dir,
                to: into.dir.clone(),
            },
        }
    }

    /// A fresh fork of a checkpoint: its disk, aux storage and login, but
    /// not its memory, so the fork cold-boots. Unlike [`Self::fork_from`]'s,
    /// such forks can run side by side; the shell writes the checkpoint's
    /// spec with a MAC of the fork's own (`MachineSpec::with_mac`).
    pub fn fresh_fork_from(
        &self,
        checkpoint: &CheckpointFiles,
        into: &Self,
        login: Credentials,
    ) -> Assembly {
        let staging = into.staging();
        let image = staging.image();
        let mut build = vec![
            FileOp::CreateDir {
                path: staging.dir.clone(),
            },
            FileOp::Clone {
                from: checkpoint.image.disk.clone(),
                to: image.disk,
            },
            FileOp::Clone {
                from: checkpoint.image.aux.clone(),
                to: image.aux,
            },
        ];
        match login {
            Credentials::Stored => build.push(FileOp::Clone {
                from: self.login_password(),
                to: staging.login_password(),
            }),
            Credentials::None => {}
        }
        Assembly {
            build,
            spec: image.spec,
            lineage: staging.lineage(),
            commit: FileOp::Rename {
                from: staging.dir,
                to: into.dir.clone(),
            },
        }
    }

    /// Adopts existing machine files as this new bundle. The shell runs
    /// `build`, writes the spec to `spec` and the lineage to `lineage`,
    /// then runs `commit`.
    pub fn import_from(&self, aux: &Path, disk: &Path) -> Assembly {
        let staging = self.staging();
        let image = staging.image();
        Assembly {
            build: vec![
                FileOp::CreateDir {
                    path: staging.dir.clone(),
                },
                FileOp::Clone {
                    from: aux.to_owned(),
                    to: image.aux,
                },
                FileOp::Clone {
                    from: disk.to_owned(),
                    to: image.disk,
                },
            ],
            spec: image.spec,
            lineage: staging.lineage(),
            commit: FileOp::Rename {
                from: staging.dir,
                to: self.dir.clone(),
            },
        }
    }

    /// Rolls this (stopped) machine back to one of its checkpoints, discarding
    /// its current disk. Clones land beside the originals; a commit marker is
    /// written once they all exist, then they are renamed over the originals.
    /// Interrupted before the marker, the current image is intact; after it,
    /// [`Leftovers`] finishes the restore.
    ///
    /// An aux copy kept by an unfinished resume ([`ResumeAttempt`]) belongs to
    /// the memory state being replaced, so it goes too: put back later, it
    /// would overwrite the restored aux storage.
    pub fn restore_from(&self, checkpoint: &CheckpointFiles, kept: KeptAux) -> Restoration {
        let pairs = self.restore_pairs();
        let sources = [
            &checkpoint.image.spec,
            &checkpoint.image.disk,
            &checkpoint.image.aux,
            &checkpoint.memory,
        ];
        let prepare = sources
            .iter()
            .zip(&pairs)
            .map(|(from, (staged, _))| FileOp::Clone {
                from: (*from).clone(),
                to: staged.clone(),
            })
            .collect();
        let mut finish = Vec::new();
        match kept {
            KeptAux::Absent => {}
            KeptAux::Present => finish.push(self.resume_attempt().succeeded()),
        }
        for (staged, to) in pairs {
            finish.push(FileOp::Rename { from: staged, to });
        }
        finish.push(FileOp::Remove {
            path: self.restore_marker(),
        });
        Restoration {
            prepare,
            commit: FileOp::CreateFile {
                path: self.restore_marker(),
            },
            finish,
        }
    }

    /// What an interrupted change to this machine can have left behind.
    #[must_use]
    pub fn leftovers(&self) -> Leftovers {
        Leftovers {
            bundle: self.clone(),
        }
    }

    /// Guards the auxiliary storage while the VMM resumes the suspended state.
    #[must_use]
    pub fn resume_attempt(&self) -> ResumeAttempt {
        let aux = self.image().aux;
        ResumeAttempt {
            kept: aux.with_extension("resuming"),
            aux,
        }
    }
}

/// A failed restore still rewrites `aux.bin`, after which the suspended state
/// never restores again (seen live: VZ "invalid argument" on every retry). So
/// the aux storage is cloned aside before a resume and put back if the resume
/// fails.
///
/// A kept copy found beside a suspended state means an earlier attempt never
/// finished; its aux storage is the one that matches the state. A kept copy
/// without a suspended state is from an attempt that succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeAttempt {
    aux: PathBuf,
    kept: PathBuf,
}

/// What an earlier resume attempt left beside the aux storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeptAux {
    Absent,
    Present,
}

impl ResumeAttempt {
    #[must_use]
    pub fn kept(&self) -> &Path {
        &self.kept
    }

    /// Before the VMM restores: the aux storage that matches the state is set aside.
    #[must_use]
    pub fn begin(&self, earlier: KeptAux) -> Vec<FileOp> {
        let mut ops = Vec::new();
        match earlier {
            KeptAux::Absent => {}
            KeptAux::Present => ops.push(FileOp::Rename {
                from: self.kept.clone(),
                to: self.aux.clone(),
            }),
        }
        ops.push(FileOp::Clone {
            from: self.aux.clone(),
            to: self.kept.clone(),
        });
        ops
    }

    /// The guest runs: its aux storage moves on, the kept copy is obsolete.
    #[must_use]
    pub fn succeeded(&self) -> FileOp {
        FileOp::Remove {
            path: self.kept.clone(),
        }
    }

    /// The VMM has exited without restoring: put the matching aux storage back.
    #[must_use]
    pub fn failed(&self) -> FileOp {
        FileOp::Rename {
            from: self.kept.clone(),
            to: self.aux.clone(),
        }
    }

    /// Cold boot: a kept copy can only be left from a resume that succeeded.
    #[must_use]
    pub fn discard(&self, earlier: KeptAux) -> Option<FileOp> {
        match earlier {
            KeptAux::Absent => None,
            KeptAux::Present => Some(self.succeeded()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointFiles {
    pub dir: PathBuf,
    pub image: ImageFiles,
    pub memory: PathBuf,
}

impl CheckpointFiles {
    fn in_dir(dir: PathBuf) -> Self {
        Self {
            image: ImageFiles::in_dir(&dir),
            memory: dir.join("memory.vzsave"),
            dir,
        }
    }
}

/// A new bundle assembled out of sight: the shell runs `build`, writes the
/// spec at `spec` and the lineage at `lineage`, then runs `commit`, the
/// rename that makes it appear.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct Assembly {
    pub build: Vec<FileOp>,
    pub spec: PathBuf,
    pub lineage: PathBuf,
    pub commit: FileOp,
}

/// A new bundle cloned whole from a checkpoint, spec included: the shell
/// runs `build`, writes the lineage at `lineage`, then runs `commit`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct CloneAssembly {
    pub build: Vec<FileOp>,
    pub lineage: PathBuf,
    pub commit: FileOp,
}

/// A restore in three parts, split where its outcome changes. Interrupted
/// in `prepare`, nothing has changed: the next claim removes the staged
/// copies. Once `commit` (the marker) may exist, the restore is decided:
/// interrupted from there on, the next claim finishes it with `finish`'s
/// renames.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct Restoration {
    pub prepare: Vec<FileOp>,
    pub commit: FileOp,
    pub finish: Vec<FileOp>,
}

impl Restoration {
    /// Every operation, in order.
    pub fn into_ops(self) -> Vec<FileOp> {
        let mut ops = self.prepare;
        ops.push(self.commit);
        ops.extend(self.finish);
        ops
    }
}

/// What interrupted changes can leave in a machine's files, and how to
/// settle them. The shell checks which [`Self::probes`] exist, then performs
/// [`Self::settle`] before changing the machine again. Settling is itself
/// interruptible: run again, it carries on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftovers {
    bundle: MachineBundle,
}

impl Leftovers {
    /// The paths whose existence decides what to do.
    #[must_use]
    pub fn probes(&self) -> Vec<PathBuf> {
        let b = &self.bundle;
        let mut probes = vec![
            b.staging().dir,
            b.checkpoint_staging().dir,
            b.checkpoint_trash(),
            b.restore_marker(),
            b.resume_attempt().kept,
        ];
        probes.extend(b.restore_pairs().map(|(staged, _)| staged));
        probes
    }

    /// What to do about the guest, given which probes exist and the VM's
    /// settled state (`None`: not running). A checkpoint pauses the guest
    /// and resumes it whatever happens; one interrupted before that leaves
    /// it paused, so the next owner resumes it, as the checkpoint would
    /// have. A guest paused on purpose has no checkpoint staging beside it.
    pub fn guest(&self, present: &BTreeSet<PathBuf>, state: Option<RunState>) -> GuestRepair {
        let interrupted = present.contains(&self.bundle.checkpoint_staging().dir);
        match (interrupted, state) {
            (true, Some(RunState::Paused)) => GuestRepair::Resume,
            (
                true | false,
                None
                | Some(
                    RunState::Starting
                    | RunState::Running
                    | RunState::Pausing
                    | RunState::Paused
                    | RunState::Resuming
                    | RunState::Saving
                    | RunState::Restoring
                    | RunState::Stopping
                    | RunState::Stopped
                    | RunState::Error
                    | RunState::Unrecognized(_),
                ),
            ) => GuestRepair::Leave,
        }
    }

    /// The operations that bring the machine to a settled state, given which
    /// probes exist:
    ///
    /// - an unfinished new bundle or checkpoint never committed: removed;
    /// - a checkpoint deleted but not yet removed: removed;
    /// - a restore past its commit marker: finished;
    /// - a restore before its marker: its staged clones removed.
    #[must_use]
    pub fn settle(&self, present: &BTreeSet<PathBuf>) -> Vec<FileOp> {
        let b = &self.bundle;
        let mut ops = Vec::new();
        for dir in [
            b.staging().dir,
            b.checkpoint_staging().dir,
            b.checkpoint_trash(),
        ] {
            if present.contains(&dir) {
                ops.push(FileOp::RemoveTree { path: dir });
            }
        }
        let marker = b.restore_marker();
        let staged = b
            .restore_pairs()
            .into_iter()
            .filter(|(staged, _)| present.contains(staged));
        if present.contains(&marker) {
            let kept = b.resume_attempt();
            if present.contains(kept.kept()) {
                ops.push(kept.succeeded());
            }
            ops.extend(staged.map(|(from, to)| FileOp::Rename { from, to }));
            ops.push(FileOp::Remove { path: marker });
        } else {
            ops.extend(staged.map(|(path, _)| FileOp::Remove { path }));
        }
        ops
    }
}

/// What settling leftovers asks of a running guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum GuestRepair {
    Leave,
    /// An interrupted checkpoint left it paused.
    Resume,
}

impl MachineBundle {
    /// Deletes one checkpoint: it leaves the listing in one rename to
    /// `checkpoints/.deleting`, so it is gone whole or not at all, and is
    /// then removed. Interrupted after the rename, it leaves only the trash,
    /// which [`Leftovers`] removes. Nothing else needs its files: a restore
    /// or fork clones them, and a fork's lineage names a checkpoint that may
    /// since have gone. Whether the machine keeps it as a golden image's
    /// template is [`crate::lineage::RecordedLineage::may_delete`]'s call.
    #[must_use]
    pub fn delete_checkpoint(&self, label: &CheckpointLabel) -> Vec<FileOp> {
        let trash = self.checkpoint_trash();
        vec![
            FileOp::Rename {
                from: self.checkpoint(label).dir,
                to: trash.clone(),
            },
            FileOp::RemoveTree { path: trash },
        ]
    }
}

/// What is already at a machine's place in `deleting/`, before it is
/// deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trash {
    /// No `deleting/` folder yet.
    NoFolder,
    /// The folder, with nothing of this machine's in it.
    Clear,
    /// What an interrupted delete of a machine of this name left.
    Leftover,
}

impl MachineBundle {
    /// Deletes the machine and its checkpoints: its folder leaves
    /// `machines/` in one rename to `trash` ([`DataRoot::deleting`]), so it
    /// is gone whole or not at all, and is then removed. Interrupted after
    /// the rename, it leaves only `trash`, which the next delete of the
    /// name removes first.
    #[must_use]
    pub fn delete(&self, trash: &Path, there: Trash) -> Vec<FileOp> {
        let mut ops = Vec::new();
        match there {
            Trash::NoFolder => {
                if let Some(folder) = trash.parent() {
                    ops.push(FileOp::CreateDir {
                        path: folder.to_owned(),
                    });
                }
            }
            Trash::Clear => {}
            Trash::Leftover => ops.push(FileOp::RemoveTree {
                path: trash.to_owned(),
            }),
        }
        ops.push(FileOp::Rename {
            from: self.dir.clone(),
            to: trash.to_owned(),
        });
        ops.push(FileOp::RemoveTree {
            path: trash.to_owned(),
        });
        ops
    }
}

/// A filesystem operation the shell must perform, in order, stopping at the
/// first failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    /// Creates a new directory. It must not already exist.
    CreateDir { path: PathBuf },
    /// APFS copy-on-write clone. Never falls back to a byte copy: a 64 GiB
    /// copy is not an acceptable silent substitute.
    Clone { from: PathBuf, to: PathBuf },
    /// Atomic replace.
    Rename { from: PathBuf, to: PathBuf },
    /// Flushes a file's data to stable storage (`F_FULLFSYNC`).
    Flush { path: PathBuf },
    /// Deletes one file (never a directory).
    Remove { path: PathBuf },
    /// Creates an empty file, which must not exist, and flushes it: a marker
    /// that must survive a crash.
    CreateFile { path: PathBuf },
    /// Deletes a directory and everything in it. Only for uncommitted
    /// staging directories, a deleted machine's folder in `deleting/` and a
    /// deleted checkpoint's in `checkpoints/.deleting`.
    RemoveTree { path: PathBuf },
}

#[cfg(test)]
pub(crate) mod model;

#[cfg(test)]
mod crash;

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(name: &str) -> MachineBundle {
        DataRoot::new("/data".into())
            .unwrap()
            .machine(&name.parse().unwrap())
    }

    #[test]
    fn a_machine_leaves_the_list_in_one_rename_then_is_removed() {
        let root = DataRoot::new("/data".into()).unwrap();
        let name = "work".parse().unwrap();
        let work = root.machine(&name);
        let trash = root.deleting(&name);
        assert_eq!(trash, Path::new("/data/deleting/work"));
        let renamed = FileOp::Rename {
            from: "/data/machines/work".into(),
            to: trash.clone(),
        };
        let removed = FileOp::RemoveTree {
            path: trash.clone(),
        };
        assert_eq!(
            work.delete(&trash, Trash::Clear),
            vec![renamed.clone(), removed.clone()]
        );
        assert_eq!(
            work.delete(&trash, Trash::NoFolder),
            vec![
                FileOp::CreateDir {
                    path: "/data/deleting".into()
                },
                renamed.clone(),
                removed.clone()
            ]
        );
        assert_eq!(
            work.delete(&trash, Trash::Leftover),
            vec![removed.clone(), renamed, removed]
        );
    }

    #[test]
    fn a_checkpoint_leaves_the_list_in_one_rename_then_is_removed() {
        let b = bundle("work");
        let trash = PathBuf::from("/data/machines/work/checkpoints/.deleting");
        assert_eq!(
            b.delete_checkpoint(&"stale".parse().unwrap()),
            vec![
                FileOp::Rename {
                    from: "/data/machines/work/checkpoints/stale".into(),
                    to: trash.clone(),
                },
                FileOp::RemoveTree { path: trash },
            ]
        );
    }

    #[test]
    fn layout() {
        let b = bundle("work");
        assert_eq!(
            b.image().disk,
            PathBuf::from("/data/machines/work/disk.img")
        );
        let cp = b.checkpoint(&"before-install".parse().unwrap());
        assert_eq!(
            cp.memory,
            PathBuf::from("/data/machines/work/checkpoints/before-install/memory.vzsave")
        );
        assert_eq!(
            cp.image.spec,
            PathBuf::from("/data/machines/work/checkpoints/before-install/spec.pb")
        );
    }

    #[test]
    fn relative_roots_are_rejected() {
        assert!(DataRoot::new("data".into()).is_err());
        assert!(RuntimeDir::new("run".into()).is_err());
    }

    #[test]
    fn socket_paths_respect_sun_path_limit() {
        let name = "a".repeat(MachineName::MAX_LEN).parse().unwrap();
        assert!(
            RuntimeDir::new("/tmp/doppel".into())
                .unwrap()
                .vmm_socket(&name)
                .is_ok()
        );
        let long = RuntimeDir::new(PathBuf::from("/").join("d".repeat(80))).unwrap();
        assert!(matches!(
            long.vmm_socket(&name),
            Err(LayoutError::SocketPathTooLong { .. })
        ));
        let root = DataRoot::new(PathBuf::from("/data")).unwrap();
        assert_eq!(
            root.daemon_socket().unwrap().path(),
            Path::new("/data/doppeld.socket")
        );
        let long = DataRoot::new(PathBuf::from("/").join("d".repeat(100))).unwrap();
        assert!(matches!(
            long.daemon_socket(),
            Err(LayoutError::SocketPathTooLong { .. })
        ));
    }

    #[test]
    fn checkpoint_labels() {
        assert!("pre-login_2".parse::<CheckpointLabel>().is_ok());
        for bad in ["", "../x", "A", "a b"] {
            assert!(bad.parse::<CheckpointLabel>().is_err(), "{bad}");
        }
    }

    #[test]
    fn fork_clones_the_checkpoint_into_a_suspended_machine() {
        let source = bundle("base");
        let cp = source.checkpoint(&"clean".parse().unwrap());
        let fork = source.fork_from(&cp, &bundle("fork"), Credentials::Stored);
        let ops = &fork.build;
        assert!(ops.contains(&FileOp::Clone {
            from: "/data/machines/base/login-password".into(),
            to: "/data/machines/.fork.staging/login-password".into()
        }));
        let without = source.fork_from(&cp, &bundle("fork"), Credentials::None);
        assert_eq!(without.build.len(), ops.len() - 1);
        assert!(
            !without
                .build
                .iter()
                .any(|op| matches!(op, FileOp::Clone { to, .. } if to.ends_with("login-password")))
        );
        assert_eq!(
            ops[0],
            FileOp::CreateDir {
                path: "/data/machines/.fork.staging".into()
            }
        );
        assert!(ops.contains(&FileOp::Clone {
            from: cp.memory.clone(),
            to: "/data/machines/.fork.staging/suspended.vzsave".into()
        }));
        // Its lineage is written out of sight, before it appears.
        assert_eq!(
            fork.lineage,
            PathBuf::from("/data/machines/.fork.staging/lineage.pb")
        );
        // It appears with its last operation.
        assert_eq!(
            fork.commit,
            FileOp::Rename {
                from: "/data/machines/.fork.staging".into(),
                to: "/data/machines/fork".into()
            }
        );
        // Nothing is written into the source.
        for op in ops.iter().chain([&fork.commit]) {
            match op {
                FileOp::CreateDir { path, .. }
                | FileOp::Clone { to: path, .. }
                | FileOp::Rename { to: path, .. }
                | FileOp::Flush { path }
                | FileOp::Remove { path }
                | FileOp::CreateFile { path }
                | FileOp::RemoveTree { path } => {
                    assert!(
                        path.starts_with("/data/machines/.fork.staging")
                            || path.starts_with("/data/machines/fork"),
                        "{op:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_fresh_fork_takes_the_disk_and_login_but_not_the_memory() {
        let source = bundle("base");
        let cp = source.checkpoint(&"clean".parse().unwrap());
        let fresh = source.fresh_fork_from(&cp, &bundle("base-clean-1"), Credentials::Stored);
        let staging = PathBuf::from("/data/machines/.base-clean-1.staging");
        assert_eq!(
            fresh.build,
            [
                FileOp::CreateDir {
                    path: staging.clone()
                },
                FileOp::Clone {
                    from: cp.image.disk.clone(),
                    to: staging.join("disk.img")
                },
                FileOp::Clone {
                    from: cp.image.aux.clone(),
                    to: staging.join("aux.bin")
                },
                FileOp::Clone {
                    from: "/data/machines/base/login-password".into(),
                    to: staging.join("login-password")
                },
            ]
        );
        assert_eq!(fresh.spec, staging.join("spec.pb"));
        assert_eq!(fresh.lineage, staging.join("lineage.pb"));
        assert_eq!(
            fresh.commit,
            FileOp::Rename {
                from: staging,
                to: "/data/machines/base-clean-1".into()
            }
        );
        let without = source.fresh_fork_from(&cp, &bundle("x"), Credentials::None);
        assert_eq!(without.build.len(), 3);
    }

    #[test]
    fn locks_and_staging_are_hidden_siblings_of_the_bundle() {
        let b = bundle("work");
        assert_eq!(b.lock_file(), PathBuf::from("/data/machines/.work.lock"));
        assert_eq!(
            b.checkpoint_staging().dir,
            PathBuf::from("/data/machines/work/checkpoints/.staging")
        );
        let assembly = b.import_from(Path::new("/src/aux"), Path::new("/src/disk"));
        assert_eq!(
            assembly.spec,
            PathBuf::from("/data/machines/.work.staging/spec.pb")
        );
        assert_eq!(
            assembly.lineage,
            PathBuf::from("/data/machines/.work.staging/lineage.pb")
        );
        assert_eq!(b.lineage(), PathBuf::from("/data/machines/work/lineage.pb"));
        assert_eq!(
            assembly.commit,
            FileOp::Rename {
                from: "/data/machines/.work.staging".into(),
                to: "/data/machines/work".into()
            }
        );
    }

    #[test]
    fn a_resume_keeps_the_aux_storage_until_it_succeeds() {
        let attempt = bundle("fork").resume_attempt();
        let aux = PathBuf::from("/data/machines/fork/aux.bin");
        let kept = PathBuf::from("/data/machines/fork/aux.resuming");
        assert_eq!(attempt.kept(), kept);
        assert_eq!(
            attempt.begin(KeptAux::Absent),
            [FileOp::Clone {
                from: aux.clone(),
                to: kept.clone()
            }]
        );
        assert_eq!(
            attempt.failed(),
            FileOp::Rename {
                from: kept.clone(),
                to: aux.clone()
            }
        );
        assert_eq!(attempt.succeeded(), FileOp::Remove { path: kept.clone() });
    }

    #[test]
    fn an_unfinished_resume_puts_its_aux_storage_back_first() {
        let attempt = bundle("fork").resume_attempt();
        let aux = PathBuf::from("/data/machines/fork/aux.bin");
        let kept = PathBuf::from("/data/machines/fork/aux.resuming");
        // The aux storage may have been rewritten by the attempt that left the copy.
        assert_eq!(
            attempt.begin(KeptAux::Present),
            [
                FileOp::Rename {
                    from: kept.clone(),
                    to: aux.clone()
                },
                FileOp::Clone {
                    from: aux,
                    to: kept.clone()
                },
            ]
        );
        assert_eq!(attempt.discard(KeptAux::Absent), None);
        assert_eq!(
            attempt.discard(KeptAux::Present),
            Some(FileOp::Remove { path: kept })
        );
    }

    #[test]
    fn restore_stages_every_clone_then_commits_before_replacing_anything() {
        let b = bundle("work");
        let restore = b.restore_from(&b.checkpoint(&"clean".parse().unwrap()), KeptAux::Absent);
        assert_eq!(restore.prepare.len(), 4);
        assert!(
            restore
                .prepare
                .iter()
                .all(|op| matches!(op, FileOp::Clone { .. }))
        );
        assert_eq!(
            restore.commit,
            FileOp::CreateFile {
                path: "/data/machines/work/restore.commit".into()
            }
        );
        let finish = &restore.finish;
        assert!(
            finish[..finish.len() - 1]
                .iter()
                .all(|op| matches!(op, FileOp::Rename { .. }))
        );
        assert!(finish.contains(&FileOp::Rename {
            from: "/data/machines/work/suspended.restoring".into(),
            to: "/data/machines/work/suspended.vzsave".into()
        }));
        assert_eq!(
            finish.last(),
            Some(&FileOp::Remove {
                path: "/data/machines/work/restore.commit".into()
            })
        );
    }

    #[test]
    fn only_a_guest_an_interrupted_checkpoint_paused_is_resumed() {
        let b = bundle("work");
        let leftovers = b.leftovers();
        let staging: BTreeSet<_> = [b.checkpoint_staging().dir].into();
        let none = BTreeSet::new();
        assert_eq!(
            leftovers.guest(&staging, Some(RunState::Paused)),
            GuestRepair::Resume
        );
        // Paused on purpose: no staging beside it.
        assert_eq!(
            leftovers.guest(&none, Some(RunState::Paused)),
            GuestRepair::Leave
        );
        assert_eq!(
            leftovers.guest(&staging, Some(RunState::Running)),
            GuestRepair::Leave
        );
        assert_eq!(leftovers.guest(&staging, None), GuestRepair::Leave);
    }

    #[test]
    fn restore_drops_an_aux_copy_kept_for_the_state_it_replaces() {
        let b = bundle("work");
        let cp = b.checkpoint(&"clean".parse().unwrap());
        let kept = FileOp::Remove {
            path: "/data/machines/work/aux.resuming".into(),
        };
        assert!(
            !b.restore_from(&cp, KeptAux::Absent)
                .into_ops()
                .contains(&kept)
        );
        assert!(
            b.restore_from(&cp, KeptAux::Present).finish.contains(&kept),
            "only once the restore is committed"
        );
    }
}
