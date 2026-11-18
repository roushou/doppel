//! Runs [`crate::agent_grant::plan`] on a stopped machine's disk: attaches
//! the image (no root needed: it's the user's file), mounts its Data
//! volume, checks and writes the TCC database with `sqlite3` and the
//! capture approvals plist, then unmounts and ejects.
//!
//! The disk is only ever written while no VM uses it and no memory state
//! expects its old contents: the caller checks both. Interrupted, it leaves
//! at most an attached image (`diskutil eject` it); each record is written
//! whole or not at all by sqlite's transaction and a single file write.

pub mod plan;

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

use crate::agent_grant::plan::{
    AttachedImage, CaptureApprovals, CodeRequirement, GrantPlanError, TCC_DB, TccGrant, TccSchema,
};
use crate::golden::plan::GuestUserName;
use crate::machines::MachineError;
use doppel_machine::machine::MachineName;

const DISKUTIL: &str = "/usr/sbin/diskutil";
const SQLITE: &str = "/usr/bin/sqlite3";

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
    Tool { what: String, why: String },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Writes both grants into the Data volume of the disk image at `disk`,
/// mounted at `mount` meanwhile.
///
/// # Errors
/// [`AgentGrantError`]; nothing is written when the database's layout or
/// the approvals' shape is unknown.
pub(crate) fn grant(
    machine: &MachineName,
    disk: &Path,
    mount: &Path,
    user: &GuestUserName,
    requirement: &CodeRequirement,
    at: SystemTime,
) -> Result<(), AgentGrantError> {
    let attached = Attached::attach(disk)?;
    let mounted = Mounted::mount(&attached.image.data, mount)?;
    let written = write(machine, mounted.dir(), user, requirement, at);
    let unmounted = mounted.unmount();
    let detached = attached.eject();
    written?;
    unmounted?;
    detached
}

fn write(
    machine: &MachineName,
    root: &Path,
    user: &GuestUserName,
    requirement: &CodeRequirement,
    at: SystemTime,
) -> Result<(), AgentGrantError> {
    let approvals = CaptureApprovals::new(user.clone());
    if !root.join(approvals.home()).is_dir() {
        return Err(AgentGrantError::NoSuchUser {
            machine: machine.clone(),
            user: user.as_str().to_owned(),
        });
    }
    // Checked first, both: an unknown shape stops the grant before a write.
    let db = root.join(TCC_DB);
    let schema = TccSchema::parse(&sqlite(
        &db,
        &["-list", "-noheader"],
        TccSchema::QUERY,
        None,
    )?)?;
    let plist = root.join(approvals.path());
    let existing = match fs::read(&plist) {
        Ok(bytes) => {
            Some(plist::from_bytes::<plist::Value>(&bytes).map_err(|_| GrantPlanError::Approvals)?)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(AgentGrantError::Io {
                path: plist,
                source,
            });
        }
    };
    let merged = approvals.merged(existing, at)?;

    sqlite(
        &db,
        &["-bail"],
        "",
        Some(TccGrant::new(&schema, requirement, at)?.sql()),
    )?;
    write_plist(&root.join(approvals.dir()), &plist, &merged)
}

/// Writes in place when the file exists, so it keeps the guest's owner.
fn write_plist(dir: &Path, path: &Path, value: &plist::Value) -> Result<(), AgentGrantError> {
    let io = |path: &Path| {
        let path = path.to_owned();
        move |source| AgentGrantError::Io { path, source }
    };
    let mut bytes = Vec::new();
    plist::to_writer_binary(&mut bytes, value).map_err(|error| AgentGrantError::Tool {
        what: "encoding the capture approvals".to_owned(),
        why: error.to_string(),
    })?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(io(dir))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(io(path))?;
    file.write_all(&bytes).map_err(io(path))?;
    file.sync_all().map_err(io(path))
}

/// Runs `sqlite3 OPTIONS DB QUERY`, or feeds it `stdin`; its output.
fn sqlite(
    db: &Path,
    options: &[&str],
    query: &str,
    stdin: Option<&str>,
) -> Result<String, AgentGrantError> {
    let mut command = Command::new(SQLITE);
    command.args(options).arg(db);
    if !query.is_empty() {
        command.arg(query);
    }
    let output = run(&mut command, stdin, "sqlite3 on the guest's TCC database")?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run(command: &mut Command, stdin: Option<&str>, what: &str) -> Result<Output, AgentGrantError> {
    let tool = |why: String| AgentGrantError::Tool {
        what: what.to_owned(),
        why,
    };
    command
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

/// An attached disk image, ejected when dropped if not before.
struct Attached {
    image: AttachedImage,
    ejected: bool,
}

impl Attached {
    fn attach(disk: &Path) -> Result<Self, AgentGrantError> {
        let output = run(
            Command::new(DISKUTIL)
                .args(["image", "attach", "--noMount", "--plist"])
                .arg(disk),
            None,
            "attaching the machine's disk",
        )?;
        Ok(Self {
            image: AttachedImage::parse(&output.stdout)?,
            ejected: false,
        })
    }

    fn eject(mut self) -> Result<(), AgentGrantError> {
        self.ejected = true;
        run(
            Command::new(DISKUTIL).args(["eject", &self.image.disk]),
            None,
            "ejecting the machine's disk",
        )
        .map(drop)
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        if !self.ejected {
            let _ = Command::new(DISKUTIL)
                .args(["eject", &self.image.disk])
                .output();
        }
    }
}

/// The Data volume, mounted on a directory made for it; unmounted, and
/// the directory removed, when dropped if not before.
struct Mounted {
    dir: PathBuf,
    unmounted: bool,
}

impl Mounted {
    fn mount(volume: &str, dir: &Path) -> Result<Self, AgentGrantError> {
        fs::create_dir_all(dir).map_err(|source| AgentGrantError::Io {
            path: dir.to_owned(),
            source,
        })?;
        run(
            Command::new(DISKUTIL)
                .args(["mount", "-mountPoint"])
                .arg(dir)
                .arg(volume),
            None,
            "mounting the machine's Data volume",
        )?;
        Ok(Self {
            dir: dir.to_owned(),
            unmounted: false,
        })
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    fn unmount(mut self) -> Result<(), AgentGrantError> {
        self.unmounted = true;
        run(
            Command::new(DISKUTIL).arg("unmount").arg(&self.dir),
            None,
            "unmounting the machine's Data volume",
        )?;
        let _ = fs::remove_dir(&self.dir);
        Ok(())
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        if !self.unmounted {
            let _ = Command::new(DISKUTIL)
                .arg("unmount")
                .arg(&self.dir)
                .output();
            let _ = fs::remove_dir(&self.dir);
        }
    }
}
