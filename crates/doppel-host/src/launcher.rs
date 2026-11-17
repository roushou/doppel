//! Starting `doppel-vmm` processes.
//!
//! The VMM is told explicit paths: it knows nothing of the bundle layout,
//! which `doppel-machine` alone owns.

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::golden::plan::GuestUserName;
use crate::provision::FullName;
use doppel_machine::bundle::{ImageFiles, SocketPath};
use doppel_protocol::convert::RemoteError;
use doppel_protocol::frame::{FrameDecoder, MaxFrameLen, Received, StreamError};
use doppel_protocol::pb::vmm::v1 as vmm;

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("doppel-vmm not found at {0}; build it with scripts/build.sh or set DOPPEL_VMM")]
    Missing(PathBuf),
    #[error("cannot start doppel-vmm: {0}")]
    Spawn(#[source] io::Error),
    #[error("cannot open log {path}: {source}")]
    Log {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("installer output: {0}")]
    Read(#[source] io::Error),
    #[error(transparent)]
    Stream(#[from] StreamError),
    #[error("installer exited without reporting an outcome (status {0})")]
    NoOutcome(String),
    #[error("installation failed: {0}")]
    Failed(#[from] RemoteError),
}

/// How the VM comes up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Boot {
    ColdBoot,
    /// Restore this memory state. The VMM deletes the file right after a
    /// successful restore, before the guest runs: a state file must never be
    /// paired with a disk that has moved on.
    ConsumeState(PathBuf),
    /// The first boot after an install: macOS creates this account. The
    /// password is read from its file by the VMM, never passed in argv.
    Provision {
        full_name: FullName,
        user: GuestUserName,
        password_file: PathBuf,
    },
}

pub use doppel_machine::vm::Window;

#[derive(Debug, Clone)]
pub struct VmmBinary(PathBuf);

impl VmmBinary {
    /// Checked when launched, so commands that never launch it work without it.
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self(path)
    }

    fn command(&self) -> Result<Command, LaunchError> {
        if self.0.is_file() {
            Ok(Command::new(&self.0))
        } else {
            Err(LaunchError::Missing(self.0.clone()))
        }
    }

    /// Starts a detached VMM serving `socket`. It outlives this process and
    /// exits when the VM stops. Output goes to `log`.
    ///
    /// # Errors
    /// The log can't be opened or the process can't start.
    pub fn serve(
        &self,
        image: &ImageFiles,
        socket: &SocketPath,
        boot: &Boot,
        window: Window,
        log: &Path,
    ) -> Result<Child, LaunchError> {
        let out = File::options()
            .create(true)
            .append(true)
            .open(log)
            .map_err(|source| LaunchError::Log {
                path: log.to_owned(),
                source,
            })?;
        let err = out.try_clone().map_err(|source| LaunchError::Log {
            path: log.to_owned(),
            source,
        })?;
        let mut command = self.command()?;
        command
            .arg("serve")
            .arg("--spec")
            .arg(&image.spec)
            .arg("--disk")
            .arg(&image.disk)
            .arg("--aux")
            .arg(&image.aux)
            .arg("--socket")
            .arg(socket.path());
        match boot {
            Boot::ColdBoot => {}
            Boot::ConsumeState(path) => {
                command.arg("--consume-state").arg(path);
            }
            Boot::Provision {
                full_name,
                user,
                password_file,
            } => {
                command
                    .arg("--provision-user")
                    .arg(user.as_str())
                    .arg("--provision-full-name")
                    .arg(full_name.as_str())
                    .arg("--provision-password-file")
                    .arg(password_file);
            }
        }
        match window {
            Window::Hidden => {}
            Window::Shown => {
                command.arg("--window");
            }
        }
        command
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            // Own process group: Ctrl-C in the terminal doesn't kill the VM.
            .process_group(0)
            .spawn()
            .map_err(LaunchError::Spawn)
    }

    /// Installs macOS from a restore image, reporting progress as it goes.
    /// The VMM fills the platform fields of the spec on success.
    ///
    /// # Errors
    /// The installer couldn't run or reported failure.
    pub fn install(
        &self,
        image: &ImageFiles,
        ipsw: &Path,
        mut progress: impl FnMut(f64),
    ) -> Result<(), LaunchError> {
        let mut child = self
            .command()?
            .arg("install")
            .arg("--spec")
            .arg(&image.spec)
            .arg("--disk")
            .arg(&image.disk)
            .arg("--aux")
            .arg(&image.aux)
            .arg("--ipsw")
            .arg(ipsw)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(LaunchError::Spawn)?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| LaunchError::Read(io::ErrorKind::BrokenPipe.into()))?;
        let mut decoder = FrameDecoder::<vmm::VmmFrame>::new(MaxFrameLen::DEFAULT);
        let mut buffer = [0u8; 4096];
        let mut outcome = None;
        loop {
            while let Some(frame) = decoder.next_frame()? {
                let Received::Recognized { message, .. } = frame else {
                    continue;
                };
                match message.kind {
                    Some(vmm::vmm_frame::Kind::Event(vmm::VmmEvent {
                        kind: Some(vmm::vmm_event::Kind::InstallProgress(p)),
                    })) => progress(p.fraction),
                    Some(vmm::vmm_frame::Kind::Event(vmm::VmmEvent {
                        kind: Some(vmm::vmm_event::Kind::InstallFinished(f)),
                    })) => outcome = Some(f.outcome),
                    Some(
                        vmm::vmm_frame::Kind::Event(_)
                        | vmm::vmm_frame::Kind::Hello(_)
                        | vmm::vmm_frame::Kind::Request(_)
                        | vmm::vmm_frame::Kind::Response(_),
                    )
                    | None => {}
                }
            }
            let n = stdout.read(&mut buffer).map_err(LaunchError::Read)?;
            if n == 0 {
                break;
            }
            decoder.push(&buffer[..n]);
        }
        let status = child.wait().map_err(LaunchError::Read)?;
        match outcome {
            Some(Some(vmm::install_finished::Outcome::Done(_))) => Ok(()),
            Some(Some(vmm::install_finished::Outcome::Error(error))) => {
                Err(RemoteError::from(error).into())
            }
            Some(None) => Err(RemoteError::Unrecognized.into()),
            None => Err(LaunchError::NoOutcome(status.to_string())),
        }
    }
}
