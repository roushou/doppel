//! Writes an [`AgentGrant`] into a stopped machine's disk.
//!
//! The disk goes through states, each a type whose steps consume it, so no
//! step can run out of order: a [`DiskImage`] attaches (no root needed:
//! it's the user's file) to an [`AttachedDisk`], whose Data volume mounts
//! to a [`MountedData`]; that gives the guest's [`TccDatabase`]s (the
//! system's and the user's) and [`CaptureApprovalsFile`], then unmounts back to the attached disk, which
//! ejects. A state dropped on the way, by an error or a panic, undoes
//! itself: unmounted, then ejected. If even that fails, an attached image is
//! left (`diskutil eject` it); each record is written whole or not at all,
//! by sqlite's transaction and a single file write.
//!
//! The disk is only ever written while no VM uses it and no memory state
//! expects its old contents: the caller checks both.

pub mod plan;

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::agent_grant::plan::{
    AgentGrant, AttachedDevices, CONTAINER_METADATA, GrantPlanError, PROTECTED_CONTAINERS, TCC_DB,
    TccSchema, UserTcc,
};
use crate::machines::MachineError;
use doppel_machine::machine::MachineName;

#[derive(Debug, thiserror::Error)]
pub enum AgentGrantError {
    #[error(transparent)]
    Machine(#[from] MachineError),
    #[error(transparent)]
    Plan(#[from] GrantPlanError),
    #[error(
        "machine '{0}' has saved memory, which expects its disk as it was: start it, then `doppel stop` it to shut it down, and grant again"
    )]
    SavedState(MachineName),
    #[error("machine '{machine}' has no user '{user}' (no /Users/{user} on its disk)")]
    NoSuchUser { machine: MachineName, user: String },
    #[error("{what} failed: {why}")]
    Tool { what: &'static str, why: String },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl AgentGrantError {
    /// For an I/O failure on `path`.
    fn io(path: &Path) -> impl Fn(io::Error) -> Self {
        let path = path.to_owned();
        move |source| Self::Io {
            path: path.clone(),
            source,
        }
    }
}

impl AgentGrant {
    /// Writes this grant into `machine`'s disk image, its Data volume
    /// mounted at `mount` meanwhile.
    ///
    /// # Errors
    /// [`AgentGrantError`]. Nothing is written unless every record was
    /// read and known.
    pub(crate) fn apply(
        &self,
        machine: &MachineName,
        image: DiskImage,
        mount: &Path,
    ) -> Result<(), AgentGrantError> {
        let data = image.attach()?.mount_data(mount)?;
        if !data.contains(&self.home()) {
            return Err(AgentGrantError::NoSuchUser {
                machine: machine.clone(),
                user: self.user().as_str().to_owned(),
            });
        }
        let system = data.system_tcc();
        let user = data.user_tcc(self)?;
        let approvals = data.approvals(self);
        let checked = self.check(&system.schema()?, &user.schema()?, approvals.read()?)?;
        system.apply(checked.system_sql())?;
        user.apply(checked.user_sql())?;
        approvals.write(checked.approvals())?;
        data.unmount()?.eject()
    }
}

/// A machine's disk image, not attached.
#[derive(Debug)]
pub(crate) struct DiskImage(PathBuf);

impl DiskImage {
    pub(crate) fn at(path: &Path) -> Self {
        Self(path.to_owned())
    }

    fn attach(self) -> Result<AttachedDisk, AgentGrantError> {
        let output = Tool::Diskutil.run(
            [
                OsStr::new("image"),
                OsStr::new("attach"),
                OsStr::new("--noMount"),
                OsStr::new("--plist"),
                self.0.as_os_str(),
            ],
            None,
            "attaching the machine's disk",
        )?;
        Ok(AttachedDisk {
            devices: AttachedDevices::parse(&output.stdout)?,
            ejected: false,
        })
    }
}

/// The disk image, attached with nothing mounted. Ejected when dropped.
#[derive(Debug)]
#[must_use = "dropping an attached disk ejects it"]
struct AttachedDisk {
    devices: AttachedDevices,
    ejected: bool,
}

impl AttachedDisk {
    /// Mounts the Data volume on `at`, a directory made for it.
    fn mount_data(self, at: &Path) -> Result<MountedData, AgentGrantError> {
        fs::create_dir_all(at).map_err(AgentGrantError::io(at))?;
        let mounted = Tool::Diskutil.run(
            [
                OsStr::new("mount"),
                OsStr::new("-mountPoint"),
                at.as_os_str(),
                OsStr::new(&self.devices.data),
            ],
            None,
            "mounting the machine's Data volume",
        );
        if let Err(error) = mounted {
            let _ = fs::remove_dir(at);
            return Err(error);
        }
        Ok(MountedData {
            mount: Mount {
                root: at.to_owned(),
                unmounted: false,
            },
            disk: self,
        })
    }

    fn eject(mut self) -> Result<(), AgentGrantError> {
        self.ejected = true;
        Tool::Diskutil
            .run(
                ["eject", self.devices.disk.as_str()],
                None,
                "ejecting the machine's disk",
            )
            .map(drop)
    }
}

impl Drop for AttachedDisk {
    fn drop(&mut self) {
        if !self.ejected {
            let _ = Tool::Diskutil
                .command()
                .args(["eject", &self.devices.disk])
                .output();
        }
    }
}

/// The Data volume, mounted. Dropped, it unmounts, then the disk ejects.
#[derive(Debug)]
#[must_use = "dropping a mounted volume unmounts and ejects it"]
struct MountedData {
    // Fields drop in order: unmounted before ejected.
    mount: Mount,
    disk: AttachedDisk,
}

impl MountedData {
    /// Whether `path`, from the volume's root, is a directory there.
    fn contains(&self, path: &Path) -> bool {
        self.mount.root.join(path).is_dir()
    }

    /// The guest's system TCC database.
    fn system_tcc(&self) -> TccDatabase {
        TccDatabase(self.mount.root.join(TCC_DB))
    }

    /// `grant`'s user's TCC database.
    fn user_tcc(&self, grant: &AgentGrant) -> Result<TccDatabase, AgentGrantError> {
        let containers = self.mount.root.join(PROTECTED_CONTAINERS);
        let mut tccd = Vec::new();
        match fs::read_dir(&containers) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(AgentGrantError::io(&containers))?;
                    let metadata = entry.path().join(CONTAINER_METADATA);
                    match fs::read(&metadata) {
                        Ok(bytes) if UserTcc::is_tccd_container(&bytes) => {
                            tccd.push(entry.file_name().to_string_lossy().into_owned());
                        }
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(source) => return Err(AgentGrantError::io(&metadata)(source)),
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(AgentGrantError::io(&containers)(source)),
        }
        tccd.sort();
        let in_home = self.mount.root.join(grant.home()).join(TCC_DB).is_file();
        let user = UserTcc::find(grant, &tccd, in_home)?;
        Ok(TccDatabase(self.mount.root.join(user.path())))
    }

    /// The capture approvals of `grant`'s user.
    fn approvals(&self, grant: &AgentGrant) -> CaptureApprovalsFile {
        CaptureApprovalsFile {
            dir: self.mount.root.join(grant.approvals_dir()),
            path: self.mount.root.join(grant.approvals_file()),
        }
    }

    fn unmount(self) -> Result<AttachedDisk, AgentGrantError> {
        let Self { mount, disk } = self;
        mount.unmount()?;
        Ok(disk)
    }
}

/// Where the Data volume is mounted. Unmounted when dropped, and its
/// directory removed.
#[derive(Debug)]
struct Mount {
    root: PathBuf,
    unmounted: bool,
}

impl Mount {
    fn unmount(mut self) -> Result<(), AgentGrantError> {
        self.unmounted = true;
        Tool::Diskutil.run(
            [OsStr::new("unmount"), self.root.as_os_str()],
            None,
            "unmounting the machine's Data volume",
        )?;
        let _ = fs::remove_dir(&self.root);
        Ok(())
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        if !self.unmounted {
            let _ = Tool::Diskutil
                .command()
                .arg("unmount")
                .arg(&self.root)
                .output();
            let _ = fs::remove_dir(&self.root);
        }
    }
}

/// One of the guest's TCC databases on the mounted volume.
#[derive(Debug)]
struct TccDatabase(PathBuf);

impl TccDatabase {
    const WHAT: &'static str = "sqlite3 on the guest's TCC database";

    /// Its `access` table's layout, if a grant fits it.
    fn schema(&self) -> Result<TccSchema, AgentGrantError> {
        let output = Tool::Sqlite3.run(
            [
                OsStr::new("-list"),
                OsStr::new("-noheader"),
                self.0.as_os_str(),
                OsStr::new(TccSchema::QUERY),
            ],
            None,
            Self::WHAT,
        )?;
        Ok(TccSchema::parse(&String::from_utf8_lossy(&output.stdout))?)
    }

    /// Writes a grant's rows for it, `sql` being one transaction.
    fn apply(&self, sql: &str) -> Result<(), AgentGrantError> {
        Tool::Sqlite3
            .run(
                [OsStr::new("-bail"), self.0.as_os_str()],
                Some(sql),
                Self::WHAT,
            )
            .map(drop)
    }
}

/// replayd's capture approvals for one guest user, on the mounted volume.
#[derive(Debug)]
struct CaptureApprovalsFile {
    dir: PathBuf,
    path: PathBuf,
}

impl CaptureApprovalsFile {
    /// What's there, if anything.
    fn read(&self) -> Result<Option<plist::Value>, AgentGrantError> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(
                plist::from_bytes(&bytes).map_err(|_| GrantPlanError::Approvals)?,
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(AgentGrantError::io(&self.path)(source)),
        }
    }

    /// Writes in place when the file exists, so it keeps the guest's owner.
    fn write(&self, approvals: &plist::Value) -> Result<(), AgentGrantError> {
        let mut bytes = Vec::new();
        plist::to_writer_binary(&mut bytes, approvals).map_err(|error| AgentGrantError::Tool {
            what: "encoding the capture approvals",
            why: error.to_string(),
        })?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
            .map_err(AgentGrantError::io(&self.dir))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&self.path)
            .map_err(AgentGrantError::io(&self.path))?;
        file.write_all(&bytes)
            .map_err(AgentGrantError::io(&self.path))?;
        file.sync_all().map_err(AgentGrantError::io(&self.path))
    }
}

/// The command line tools a grant runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Diskutil,
    Sqlite3,
}

impl Tool {
    fn command(self) -> Command {
        Command::new(match self {
            Self::Diskutil => "/usr/sbin/diskutil",
            Self::Sqlite3 => "/usr/bin/sqlite3",
        })
    }

    /// Runs it with `args` to the end: its output, or `what` failed with
    /// what it said on stderr.
    fn run<A: AsRef<OsStr>>(
        self,
        args: impl IntoIterator<Item = A>,
        stdin: Option<&str>,
        what: &'static str,
    ) -> Result<Output, AgentGrantError> {
        let tool = |why: String| AgentGrantError::Tool { what, why };
        let mut command = self.command();
        command
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| tool(error.to_string()))?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(text.as_bytes())
                .map_err(|error| tool(error.to_string()))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|error| tool(error.to_string()))?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(tool(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ))
        }
    }
}
