//! Persisted machines on disk.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use doppel_machine::bundle::{
    CheckpointFiles, CheckpointLabel, DataRoot, LayoutError, MachineBundle, RuntimeDir, SocketPath,
};
use doppel_machine::lineage::{Lineage, RecordedLineage};
use doppel_machine::machine::{MachineName, MachineSpec};
use doppel_protocol::convert::ParseError;
use doppel_protocol::pb::bundle::v1 as bundle;
use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use prost::Message;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("machine '{0}' does not exist")]
    NoSuchMachine(MachineName),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path}: not a valid machine spec: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: prost::DecodeError,
    },
    #[error("{path}: {source}")]
    InvalidSpec {
        path: PathBuf,
        #[source]
        source: ParseError,
    },
    #[error(
        "machine '{0}' is busy: another doppel command is changing it. Try again once it has finished"
    )]
    Busy(MachineName),
    #[error(transparent)]
    Layout(#[from] LayoutError),
}

impl StoreError {
    fn io(path: &Path) -> impl FnOnce(io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_owned(),
            source,
        }
    }
}

/// A directory entry under `machines/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listed {
    Machine(MachineName),
    /// Not a valid machine name: kept so it isn't silently ignored.
    Unrecognized(OsString),
}

#[derive(Debug, Clone)]
pub struct Store {
    root: DataRoot,
    runtime: RuntimeDir,
}

impl Store {
    #[must_use]
    pub const fn new(root: DataRoot, runtime: RuntimeDir) -> Self {
        Self { root, runtime }
    }

    /// Where the machines are stored: what a daemon serving this store
    /// reports, so a client can tell it serves the same one.
    #[must_use]
    pub const fn root(&self) -> &DataRoot {
        &self.root
    }

    /// Where this store's sockets, and other short-lived paths, go.
    #[must_use]
    pub const fn runtime(&self) -> &RuntimeDir {
        &self.runtime
    }

    #[must_use]
    pub fn bundle(&self, name: &MachineName) -> MachineBundle {
        self.root.machine(name)
    }

    /// Takes the exclusive, advisory lock on a machine name without
    /// waiting. Held until the returned file is dropped; the OS releases it
    /// if the process dies, so a crash never leaves a machine locked.
    ///
    /// # Errors
    /// [`StoreError::Busy`] if another holder has it, or I/O failure.
    pub fn lock(&self, bundle: &MachineBundle) -> Result<Flock<File>, StoreError> {
        let path = bundle.lock_file();
        let dir = self.root.machines_dir();
        fs::create_dir_all(&dir).map_err(StoreError::io(&dir))?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(StoreError::io(&path))?;
        Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, errno)| {
            // `Errno` is foreign and open-ended: compare rather than match.
            if errno == Errno::EWOULDBLOCK {
                StoreError::Busy(bundle.name().clone())
            } else {
                StoreError::Io {
                    path,
                    source: errno.into(),
                }
            }
        })
    }

    /// The VMM socket path, creating the private runtime directory if needed.
    ///
    /// # Errors
    /// The path is too long for a unix socket, or the directory can't be created.
    pub fn socket(&self, name: &MachineName) -> Result<SocketPath, StoreError> {
        let socket = self.runtime.vmm_socket(name)?;
        self.private_runtime_dir()?;
        Ok(socket)
    }

    /// The daemon's socket path, in the data root, creating the root if
    /// needed.
    ///
    /// # Errors
    /// The path is too long for a unix socket, or the root can't be created.
    pub fn daemon_socket(&self) -> Result<SocketPath, StoreError> {
        let socket = self.root.daemon_socket()?;
        let dir = self.root.path();
        fs::create_dir_all(dir).map_err(StoreError::io(dir))?;
        Ok(socket)
    }

    /// Only this user may reach the sockets in the runtime directory.
    fn private_runtime_dir(&self) -> Result<(), StoreError> {
        let dir = self.runtime.path();
        fs::create_dir_all(dir).map_err(StoreError::io(dir))?;
        fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .map_err(StoreError::io(dir))
    }

    /// # Errors
    /// Missing machine, unreadable or invalid spec.
    pub fn read_spec(&self, bundle: &MachineBundle) -> Result<MachineSpec, StoreError> {
        let path = bundle.image().spec;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::NoSuchMachine(bundle.name().clone()));
            }
            Err(source) => return Err(StoreError::Io { path, source }),
        };
        Self::parse_spec(path, &bytes)
    }

    /// The spec a checkpoint was taken with.
    ///
    /// # Errors
    /// Unreadable or invalid spec.
    pub fn read_checkpoint_spec(
        &self,
        checkpoint: &CheckpointFiles,
    ) -> Result<MachineSpec, StoreError> {
        let path = checkpoint.image.spec.clone();
        let bytes = fs::read(&path).map_err(StoreError::io(&path))?;
        Self::parse_spec(path, &bytes)
    }

    fn parse_spec(path: PathBuf, bytes: &[u8]) -> Result<MachineSpec, StoreError> {
        let wire = bundle::MachineSpec::decode(bytes).map_err(|source| StoreError::Decode {
            path: path.clone(),
            source,
        })?;
        MachineSpec::try_from(wire).map_err(|source| StoreError::InvalidSpec { path, source })
    }

    /// Writes atomically: a crash leaves the old spec or the new one.
    ///
    /// # Errors
    /// I/O failure.
    pub fn write_spec(&self, path: &Path, spec: &MachineSpec) -> Result<(), StoreError> {
        let staged = path.with_extension("pb.new");
        fs::write(&staged, bundle::MachineSpec::from(spec).encode_to_vec())
            .map_err(StoreError::io(&staged))?;
        fs::rename(&staged, path).map_err(StoreError::io(path))
    }

    /// The machine's role and origin. A missing file was never written
    /// (the machine predates lineage): [`Lineage::default`]. One that
    /// can't be read or decoded is [`RecordedLineage::Unreadable`], never
    /// a default.
    #[must_use]
    pub fn read_lineage(&self, bundle: &MachineBundle) -> RecordedLineage {
        let path = bundle.lineage();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return RecordedLineage::Known(Lineage::default());
            }
            Err(error) => {
                return RecordedLineage::Unreadable(format!("{}: {error}", path.display()));
            }
        };
        let parsed = bundle::Lineage::decode(bytes.as_slice())
            .map_err(|error| error.to_string())
            .and_then(|wire| Lineage::try_from(&wire).map_err(|error| error.to_string()));
        match parsed {
            Ok(lineage) => RecordedLineage::Known(lineage),
            Err(why) => RecordedLineage::Unreadable(format!("{}: {why}", path.display())),
        }
    }

    /// Writes a lineage file at `path` (a bundle's, or one being
    /// assembled) atomically: a crash leaves the old file or the new one.
    ///
    /// # Errors
    /// I/O failure.
    pub fn write_lineage(&self, path: &Path, lineage: &Lineage) -> Result<(), StoreError> {
        let staged = path.with_extension("pb.new");
        fs::write(&staged, bundle::Lineage::from(lineage).encode_to_vec())
            .map_err(StoreError::io(&staged))?;
        fs::rename(&staged, path).map_err(StoreError::io(path))
    }

    /// # Errors
    /// The machines directory exists but can't be read.
    pub fn machines(&self) -> Result<Vec<Listed>, StoreError> {
        Self::entries(&self.root.machines_dir()).map(|names| {
            names
                .into_iter()
                .map(|n| {
                    n.to_str()
                        .and_then(|s| s.parse().ok())
                        .map_or(Listed::Unrecognized(n), Listed::Machine)
                })
                .collect()
        })
    }

    /// # Errors
    /// The checkpoints directory exists but can't be read.
    pub fn checkpoints(
        &self,
        bundle: &MachineBundle,
    ) -> Result<Vec<Result<CheckpointLabel, OsString>>, StoreError> {
        Self::entries(&bundle.checkpoints_dir()).map(|names| {
            names
                .into_iter()
                .map(|n| n.to_str().and_then(|s| s.parse().ok()).ok_or(n))
                .collect()
        })
    }

    fn entries(dir: &Path) -> Result<Vec<OsString>, StoreError> {
        let read = match fs::read_dir(dir) {
            Ok(read) => read,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(StoreError::Io {
                    path: dir.to_owned(),
                    source,
                });
            }
        };
        let mut names = Vec::new();
        for entry in read {
            let entry = entry.map_err(StoreError::io(dir))?;
            let name = entry.file_name();
            if entry.file_type().map_err(StoreError::io(dir))?.is_dir()
                && !name.as_encoded_bytes().starts_with(b".")
            {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }
}
