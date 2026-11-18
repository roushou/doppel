//! What every command may need, from the global flags: where machines are
//! stored, the VMM binary, and the daemon to go through. The daemon's
//! socket and the launchd service are resolved only when a command asks:
//! `--home` may not exist yet.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use doppel_host::capabilities::SystemClock;
use doppel_host::files::LocalFilesystem;
use doppel_host::machines::Machines;
use doppel_host::store::Store;
use doppel_host::vmm::launcher::VmmBinary;
use doppel_machine::bundle::{DataRoot, RuntimeDir};

use crate::daemon_at::DaemonAt;
use crate::launchd;

/// This Mac's machines, as the commands see them.
pub(crate) type HostMachines = Machines<SystemClock, LocalFilesystem>;

/// Where this `doppel` is installed: the folder of the file it runs from,
/// symlinks followed, so a link on the `PATH` (as Homebrew makes) still
/// finds what ships beside it.
#[derive(Debug, Clone)]
pub(crate) struct InstallDir(PathBuf);

impl InstallDir {
    pub(crate) fn current() -> Result<Self> {
        let exe = std::env::current_exe()?;
        let real = exe
            .canonicalize()
            .with_context(|| format!("resolving {}", exe.display()))?;
        let dir = real
            .parent()
            .with_context(|| format!("{} has no folder", real.display()))?;
        Ok(Self(dir.to_path_buf()))
    }

    /// The VMM that runs each machine.
    pub(crate) fn vmm(&self) -> PathBuf {
        self.0.join("doppel-vmm")
    }

    /// The zipped guest agent app, with its `.build-id` and `.csreq` beside it.
    pub(crate) fn agent_archive(&self) -> PathBuf {
        self.0.join("doppel-agent.zip")
    }
}

/// Which VMM binary the commands run.
#[derive(Debug, Clone)]
enum Vmm {
    /// `--vmm` (`DOPPEL_VMM`).
    Given(PathBuf),
    /// The one installed beside `doppel`, found again by each run.
    Installed(PathBuf),
}

impl Vmm {
    fn path(&self) -> &Path {
        match self {
            Self::Given(path) | Self::Installed(path) => path,
        }
    }
}

/// The inputs commands share, resolved once.
pub(crate) struct Context {
    home: PathBuf,
    runtime: PathBuf,
    vmm: Vmm,
    /// Another Mac's daemon, forwarded to this socket (`--host`).
    host: Option<PathBuf>,
    machines: HostMachines,
}

impl Context {
    /// From the global flags, each defaulting as `--help` says.
    pub(crate) fn new(
        home: Option<PathBuf>,
        runtime: Option<PathBuf>,
        vmm: Option<PathBuf>,
        host: Option<PathBuf>,
    ) -> Result<Self> {
        let home = match home {
            Some(home) => home,
            None => std::env::home_dir()
                .context("no home directory")?
                .join("Library/Application Support/Doppel"),
        };
        let runtime = runtime.unwrap_or_else(|| std::env::temp_dir().join("doppel"));
        let vmm = match vmm {
            Some(path) => Vmm::Given(path),
            None => Vmm::Installed(InstallDir::current()?.vmm()),
        };
        let store = Store::new(
            DataRoot::new(home.clone())?,
            RuntimeDir::new(runtime.clone())?,
        );
        let machines = Machines::new(
            store,
            VmmBinary::new(vmm.path().to_path_buf()),
            SystemClock,
            LocalFilesystem,
        );
        Ok(Self {
            home,
            runtime,
            vmm,
            host,
            machines,
        })
    }

    pub(crate) fn home(&self) -> &Path {
        &self.home
    }

    pub(crate) fn vmm(&self) -> &Path {
        self.vmm.path()
    }

    pub(crate) const fn machines(&self) -> &HostMachines {
        &self.machines
    }

    /// The machines, for a command that keeps them: the daemon.
    pub(crate) fn into_machines(self) -> HostMachines {
        self.machines
    }

    /// The daemon commands go through: `--host`'s, or this store's.
    pub(crate) fn daemon(&self) -> Result<DaemonAt> {
        Ok(match &self.host {
            Some(socket) => DaemonAt::Forwarded(socket.clone()),
            None => DaemonAt::Store(self.machines.store().daemon_socket()?.path().to_path_buf()),
        })
    }

    /// This `doppel` and the global flags it was given, absolute, to run it
    /// again from elsewhere (`fork-run`'s console).
    pub(crate) fn doppel_command(&self) -> Result<Vec<OsString>> {
        let mut doppel = vec![std::env::current_exe()?.into_os_string()];
        let given = [
            ("--home", Some(&self.home)),
            ("--runtime-dir", Some(&self.runtime)),
            ("--host", self.host.as_ref()),
        ];
        for (flag, value) in given {
            if let Some(value) = value {
                doppel.extend([flag.into(), std::path::absolute(value)?.into_os_string()]);
            }
        }
        Ok(doppel)
    }

    /// The LaunchAgent that runs this store's daemon.
    pub(crate) fn service(&self) -> Result<launchd::Service> {
        let absolute = |path: &PathBuf| std::path::absolute(path).context("an absolute path");
        let home = absolute(&self.home)?;
        // `doppel` as it was run, through any link on the PATH, so the job
        // outlives an upgrade that moves the file behind the link. Likewise
        // the VMM is named only when given: otherwise each run finds the one
        // installed beside it.
        let mut program = vec![
            std::env::current_exe()?.into_os_string(),
            "--home".into(),
            home.clone().into_os_string(),
            "--runtime-dir".into(),
            absolute(&self.runtime)?.into_os_string(),
        ];
        match &self.vmm {
            Vmm::Given(path) => program.extend(["--vmm".into(), absolute(path)?.into_os_string()]),
            Vmm::Installed(_) => {}
        }
        program.extend(["daemon".into(), "run".into(), "--launchd".into()]);
        let agent = launchd::LaunchAgent {
            program,
            socket: home.join("doppeld.socket"),
            log: home.join("doppeld.log"),
        };
        let user_home = std::env::home_dir().context("no home directory")?;
        Ok(launchd::Service::new(
            agent,
            &user_home,
            nix::unistd::getuid().as_raw(),
        ))
    }
}
