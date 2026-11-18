//! Runs [`crate::bootstrap::plan`]: finds the guest's address in the host's
//! DHCP leases, starts the agent over SSH with the stored password, waits
//! for it on vsock, installs it, and removes the staging directory.
//!
//! The password never reaches argv, the environment or a log: `ssh` asks for
//! it through `SSH_ASKPASS`, which is this program again ([`Askpass`]),
//! told only where the password *file* is.

pub mod plan;

use std::fs::File;
use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use crate::agent_install::plan::{self as agent_install, AgentBuild};
use crate::bootstrap::plan::{
    AskpassPrompt, BootstrapError as PlanError, RemoteStep, SshTarget, StagingDir, Stdin, StepName,
};
use crate::dhcp_leases::{self, DhcpLeases};
use crate::golden::plan::GuestUserName;
use doppel_desktop::guest_path::GuestPath;
use doppel_machine::machine::{MacAddress, MachineName, NetworkSpec};

use crate::agent_install::{AgentInstallError, Installed};
use crate::capabilities::Clock;
use crate::files::Filesystem;
use crate::login::{PasswordFile, PasswordFileError};
use crate::machines::{MachineError, Machines};
use crate::store::StoreError;

/// From the start to giving up on a DHCP lease for the machine's MAC.
pub const ADDRESS_TIMEOUT: Duration = Duration::from_secs(120);
/// From finding the address to giving up on Remote Login accepting connections.
pub const SSH_TIMEOUT: Duration = Duration::from_secs(120);
/// From `open` to giving up on the agent answering on vsock.
pub const AGENT_TIMEOUT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_secs(1);
const SSH: &str = "/usr/bin/ssh";

/// The environment variable that makes `doppel` act as `SSH_ASKPASS`: the
/// path of the password file to answer from.
pub const ASKPASS_ENV: &str = "DOPPEL_ASKPASS_PASSWORD_FILE";

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error(transparent)]
    Machine(#[from] MachineError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Password(#[from] PasswordFileError),
    #[error(
        "{0} has no stored login password; create it with `doppel create … --user`, or store one with `doppel set-password`"
    )]
    NoPassword(MachineName),
    #[error("{0} has no network device, so it can't be reached over SSH")]
    Offline(MachineName),
    #[error("{path}: {source}")]
    Leases {
        path: &'static str,
        #[source]
        source: io::Error,
    },
    #[error(
        "no DHCP lease for {mac} in {path} after {waited:?}: is the machine running and past its first boot?"
    )]
    NoAddress {
        mac: MacAddress,
        path: &'static str,
        waited: Duration,
    },
    #[error(
        "{address}:{port} didn't accept a connection within {waited:?}: is Remote Login on? (`doppel create --user` turns it on)"
    )]
    NoSsh {
        address: Ipv4Addr,
        port: u16,
        waited: Duration,
    },
    #[error("{archive}: {source}")]
    Archive {
        archive: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{step}: running {SSH}: {source}")]
    Spawn {
        step: StepName,
        #[source]
        source: io::Error,
    },
    #[error("{step}: ssh {status}{stderr}")]
    Remote {
        step: StepName,
        status: ExitStatus,
        /// `": <stderr>"`, or empty.
        stderr: String,
    },
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(
        "the agent opened over SSH didn't answer within {waited:?}; {last}. See ~/Library/Logs/doppel-agent.log in the guest"
    )]
    NoAgent { waited: Duration, last: String },
    #[error(transparent)]
    Install(#[from] AgentInstallError),
}

/// How far a bootstrap got, for progress output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// An agent answers already: no SSH, straight to the install.
    AgentAnswers,
    FindingAddress(MacAddress),
    AwaitingSsh(Ipv4Addr),
    Remote(StepName),
    AwaitingAgent,
    Install(agent_install::StepName),
}

impl std::fmt::Display for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AgentAnswers => f.write_str("an agent answers already; installing through it"),
            Self::FindingAddress(mac) => write!(f, "looking up {mac} in {}", dhcp_leases::PATH),
            Self::AwaitingSsh(address) => {
                write!(
                    f,
                    "waiting for Remote Login at {address}:{}",
                    SshTarget::PORT
                )
            }
            Self::Remote(step) => write!(f, "over SSH: {step}"),
            Self::AwaitingAgent => f.write_str("waiting for the agent to answer"),
            Self::Install(step) => step.fmt(f),
        }
    }
}

/// How the agent got there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    AlreadyRunning,
    OverSsh(Ipv4Addr),
}

#[derive(Debug)]
#[must_use]
pub struct Bootstrapped {
    pub started: Started,
    pub installed: Installed,
    /// A staging directory the cleanup couldn't remove, and why.
    pub leftover: Option<(GuestPath, BootstrapError)>,
}

/// The zipped app and its build, from `scripts/build.sh`.
#[derive(Debug, Clone)]
pub struct AgentArchive {
    pub path: PathBuf,
    pub build: AgentBuild,
}

impl<C: Clock, F: Filesystem> Machines<C, F> {
    /// Gets an agent into a machine that may have none, then installs
    /// `archive` as its LaunchAgent. With no agent answering, it logs in
    /// over SSH as `user` with the stored password and starts the app from
    /// a staging directory; with one, it only installs.
    ///
    /// `askpass` is a program that, run with [`ASKPASS_ENV`] set, behaves
    /// as [`Askpass::respond`] (in practice, `doppel` itself).
    ///
    /// # Errors
    /// [`BootstrapError`]. A failure after the staging directory exists
    /// still tries to remove it.
    pub(crate) fn bootstrap_agent(
        &self,
        machine: &MachineName,
        user: GuestUserName,
        archive: &AgentArchive,
        askpass: &Path,
        mut progress: impl FnMut(Progress),
    ) -> Result<Bootstrapped, BootstrapError> {
        let bytes = std::fs::read(&archive.path).map_err(|source| BootstrapError::Archive {
            archive: archive.path.clone(),
            source,
        })?;
        if self.agent_absent(machine)?.is_none() {
            progress(Progress::AgentAnswers);
            let installed = self.install_agent(machine, bytes, archive.build.clone(), |step| {
                progress(Progress::Install(step));
            })?;
            return Ok(Bootstrapped {
                started: Started::AlreadyRunning,
                installed,
                leftover: None,
            });
        }

        let bundle = self.store().bundle(machine);
        if PasswordFile.read(&bundle)?.is_none() {
            return Err(BootstrapError::NoPassword(machine.clone()));
        }
        let mac = match self.store().read_spec(&bundle)?.network() {
            NetworkSpec::Nat { mac } => mac,
            NetworkSpec::None => return Err(BootstrapError::Offline(machine.clone())),
        };
        progress(Progress::FindingAddress(mac));
        let address = self.await_address(mac)?;
        progress(Progress::AwaitingSsh(address));
        self.await_port(address)?;

        let ssh = Ssh {
            target: SshTarget { user, address },
            known_hosts: bundle.ssh_known_hosts(),
            askpass: askpass.to_owned(),
            password_file: bundle.login_password(),
            archive: archive.path.clone(),
        };
        let make = StagingDir::make();
        progress(Progress::Remote(make.name));
        let staging = StagingDir::parse(&ssh.run(&make)?.stdout)?;

        let result = self.start_and_install(machine, &ssh, &staging, bytes, archive, &mut progress);
        let cleanup = staging.cleanup();
        progress(Progress::Remote(cleanup.name));
        let leftover = ssh
            .run(&cleanup)
            .err()
            .map(|error| (staging.path().clone(), error));
        Ok(Bootstrapped {
            started: Started::OverSsh(address),
            installed: result?,
            leftover,
        })
    }

    fn start_and_install(
        &self,
        machine: &MachineName,
        ssh: &Ssh,
        staging: &StagingDir,
        bytes: Vec<u8>,
        archive: &AgentArchive,
        progress: &mut impl FnMut(Progress),
    ) -> Result<Installed, BootstrapError> {
        for step in staging.start_agent() {
            progress(Progress::Remote(step.name));
            ssh.run(&step)?;
        }
        progress(Progress::AwaitingAgent);
        self.await_agent(machine)?;
        Ok(
            self.install_agent(machine, bytes, archive.build.clone(), |step| {
                progress(Progress::Install(step));
            })?,
        )
    }

    /// Why no agent answers, or `None` when one does. Errors only when
    /// the machine isn't running or the agent answers wrongly.
    fn agent_absent(&self, machine: &MachineName) -> Result<Option<String>, MachineError> {
        match self.agent(machine, self.deadline()) {
            Ok(_) => Ok(None),
            Err(error @ MachineError::AgentUnreachable { .. }) => Ok(Some(error.to_string())),
            Err(MachineError::AgentHandshake { source, .. }) if source.is_transient() => {
                Ok(Some(source.to_string()))
            }
            Err(error) => Err(error),
        }
    }

    fn await_agent(&self, machine: &MachineName) -> Result<(), BootstrapError> {
        let started = self.clock().now();
        loop {
            let Some(last) = self.agent_absent(machine)? else {
                return Ok(());
            };
            let waited = self.since(started);
            if waited >= AGENT_TIMEOUT {
                return Err(BootstrapError::NoAgent { waited, last });
            }
            std::thread::sleep(POLL);
        }
    }

    fn await_address(&self, mac: MacAddress) -> Result<Ipv4Addr, BootstrapError> {
        let started = self.clock().now();
        loop {
            let text = match std::fs::read_to_string(dhcp_leases::PATH) {
                Ok(text) => text,
                Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
                Err(source) => {
                    return Err(BootstrapError::Leases {
                        path: dhcp_leases::PATH,
                        source,
                    });
                }
            };
            if let Some(address) = DhcpLeases::parse(&text).address_of(mac) {
                return Ok(address);
            }
            let waited = self.since(started);
            if waited >= ADDRESS_TIMEOUT {
                return Err(BootstrapError::NoAddress {
                    mac,
                    path: dhcp_leases::PATH,
                    waited,
                });
            }
            std::thread::sleep(POLL);
        }
    }

    /// Waits for a TCP connection to succeed, so a guest still booting
    /// isn't mistaken for a failed login.
    fn await_port(&self, address: Ipv4Addr) -> Result<(), BootstrapError> {
        let started = self.clock().now();
        let socket = SocketAddr::from((address, SshTarget::PORT));
        loop {
            if TcpStream::connect_timeout(&socket, Duration::from_secs(2)).is_ok() {
                return Ok(());
            }
            let waited = self.since(started);
            if waited >= SSH_TIMEOUT {
                return Err(BootstrapError::NoSsh {
                    address,
                    port: SshTarget::PORT,
                    waited,
                });
            }
            std::thread::sleep(POLL);
        }
    }

    fn since(&self, started: Instant) -> Duration {
        self.clock().now().saturating_duration_since(started)
    }
}

/// One login, used for every remote step.
struct Ssh {
    target: SshTarget,
    known_hosts: PathBuf,
    askpass: PathBuf,
    password_file: PathBuf,
    archive: PathBuf,
}

impl Ssh {
    /// Runs one step; it must exit 0.
    fn run(&self, step: &RemoteStep) -> Result<Output, BootstrapError> {
        let stdin = match step.stdin {
            Stdin::Null => Stdio::null(),
            Stdin::Archive => File::open(&self.archive)
                .map_err(|source| BootstrapError::Archive {
                    archive: self.archive.clone(),
                    source,
                })?
                .into(),
        };
        let output = Command::new(SSH)
            .args(self.target.ssh_args(&self.known_hosts, step))
            .env("SSH_ASKPASS", &self.askpass)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(ASKPASS_ENV, &self.password_file)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|source| BootstrapError::Spawn {
                step: step.name,
                source,
            })?;
        if output.status.success() {
            return Ok(output);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        Err(BootstrapError::Remote {
            step: step.name,
            status: output.status,
            stderr: if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            },
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AskpassError {
    #[error(transparent)]
    Password(#[from] PasswordFileError),
    #[error("{0}: no password stored there")]
    Missing(PathBuf),
    /// Never answered with the password.
    #[error("refusing to answer ssh's prompt {0:?}: only password prompts are answered")]
    Refused(String),
    #[error("writing the answer: {0}")]
    Write(#[source] io::Error),
}

/// `doppel` run by `ssh` as its `SSH_ASKPASS` program.
#[derive(Debug, Clone, Copy, Default)]
pub struct Askpass;

impl Askpass {
    /// Writes the password in `password_file` and a newline to `out` if
    /// `prompt` asks for a password; refuses any other prompt.
    ///
    /// # Errors
    /// [`AskpassError`]; none contains the password.
    pub fn respond(
        self,
        prompt: &str,
        password_file: &Path,
        out: &mut impl Write,
    ) -> Result<(), AskpassError> {
        match AskpassPrompt::parse(prompt) {
            AskpassPrompt::Password => {}
            AskpassPrompt::Other(prompt) => return Err(AskpassError::Refused(prompt)),
        }
        let password = PasswordFile
            .read_at(password_file)?
            .ok_or_else(|| AskpassError::Missing(password_file.to_owned()))?;
        out.write_all(password.keystrokes().as_bytes())
            .and_then(|()| out.write_all(b"\n"))
            .and_then(|()| out.flush())
            .map_err(AskpassError::Write)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use doppel_machine::login::LoginPassword;

    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn password_file(tag: &str) -> (Scratch, PathBuf) {
        let dir = std::env::temp_dir().join(format!("doppel-askpass-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("login-password");
        fs::write(
            &path,
            LoginPassword::parse(b"s3cret").unwrap().file_contents(),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        (Scratch(dir), path)
    }

    #[test]
    fn answers_password_prompts_only() {
        let (_scratch, path) = password_file("answer");
        let mut out = Vec::new();
        Askpass
            .respond("doppel@192.168.64.7's password: ", &path, &mut out)
            .unwrap();
        assert_eq!(out, b"s3cret\n");

        let mut out = Vec::new();
        let error = Askpass
            .respond(
                "Are you sure you want to continue connecting (yes/no)? ",
                &path,
                &mut out,
            )
            .unwrap_err();
        assert!(matches!(error, AskpassError::Refused(_)), "{error:?}");
        assert!(out.is_empty());
    }

    #[test]
    fn a_missing_file_is_an_error_without_an_answer() {
        let (_scratch, path) = password_file("missing");
        let gone = path.with_file_name("nothing");
        let mut out = Vec::new();
        let error = Askpass.respond("Password:", &gone, &mut out).unwrap_err();
        assert!(matches!(error, AskpassError::Missing(_)), "{error:?}");
        assert!(out.is_empty());
    }
}
