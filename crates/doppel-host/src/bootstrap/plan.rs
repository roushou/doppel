//! Starting the first agent in a guest that has none, over SSH.
//!
//! A freshly provisioned machine (see [`crate::provision`]) has Remote Login
//! on and no agent, so the agent install, which goes through an agent, can't
//! start. Over SSH the host copies the zipped app into a staging directory,
//! unpacks it and starts it with `open`, which runs it in the logged-in GUI
//! session. Once that bare agent answers, the regular install
//! ([`crate::agent_install::plan`]) makes it a LaunchAgent, and the staging
//! directory is removed.
//!
//! This module is the plan: the remote command lines, the `ssh` arguments,
//! and which prompts may be answered with the password. The shell runs them.

use std::ffi::OsString;
use std::fmt;
use std::net::Ipv4Addr;
use std::path::Path;

use crate::golden::plan::GuestUserName;
use doppel_machine::guest_path::GuestPath;

const STAGING_PREFIX: &str = "/tmp/doppel-bootstrap.";
const ARCHIVE: &str = "doppel-agent.zip";
const APP: &str = "Doppel Agent.app";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BootstrapError {
    #[error("mktemp printed {0:?}, not a staging directory under {STAGING_PREFIX}*")]
    StagingDir(String),
}

/// What a remote step does, for progress and errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepName {
    MakeStagingDir,
    Upload,
    Unpack,
    Open,
    Cleanup,
}

impl fmt::Display for StepName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MakeStagingDir => "making a staging directory",
            Self::Upload => "copying the app archive",
            Self::Unpack => "unpacking the app",
            Self::Open => "opening the app",
            Self::Cleanup => "removing the staging directory",
        })
    }
}

/// What the remote command reads on standard input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stdin {
    Null,
    /// The zipped app's bytes.
    Archive,
}

/// One command line for the guest account's login shell (`zsh` on macOS),
/// built here with every variable part single-quoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStep {
    pub name: StepName,
    command: String,
    pub stdin: Stdin,
}

impl RemoteStep {
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }
}

/// Single-quotes `text` for a POSIX-style shell (sh, bash, zsh).
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// A directory [`StagingDir::make`] made: `/tmp/doppel-bootstrap.` and
/// letters and digits, so removing it can't remove anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagingDir(GuestPath);

impl StagingDir {
    /// The first step: makes a fresh staging directory and prints its path.
    #[must_use]
    pub fn make() -> RemoteStep {
        RemoteStep {
            name: StepName::MakeStagingDir,
            command: format!(
                "/usr/bin/mktemp -d {}",
                quote(&format!("{STAGING_PREFIX}XXXXXX"))
            ),
            stdin: Stdin::Null,
        }
    }

    /// Parses `mktemp`'s standard output.
    ///
    /// # Errors
    /// [`BootstrapError::StagingDir`] for anything but one such path.
    pub fn parse(stdout: &[u8]) -> Result<Self, BootstrapError> {
        let text = String::from_utf8_lossy(stdout);
        let line = text.trim_end_matches('\n');
        let invalid = || BootstrapError::StagingDir(line.to_owned());
        let suffix = line.strip_prefix(STAGING_PREFIX).ok_or_else(invalid)?;
        if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(invalid());
        }
        line.parse().map(Self).map_err(|_| invalid())
    }

    #[must_use]
    pub fn path(&self) -> &GuestPath {
        &self.0
    }

    fn archive(&self) -> String {
        quote(&format!("{}/{ARCHIVE}", self.0))
    }

    /// Copy, unpack, open: the app is running once they all exit 0.
    #[must_use]
    pub fn start_agent(&self) -> [RemoteStep; 3] {
        let dir = quote(self.0.as_str());
        [
            RemoteStep {
                name: StepName::Upload,
                command: format!("/bin/cat > {}", self.archive()),
                stdin: Stdin::Archive,
            },
            RemoteStep {
                name: StepName::Unpack,
                command: format!("/usr/bin/ditto -x -k {} {dir}", self.archive()),
                stdin: Stdin::Null,
            },
            RemoteStep {
                name: StepName::Open,
                command: format!("/usr/bin/open {}", quote(&format!("{}/{APP}", self.0))),
                stdin: Stdin::Null,
            },
        ]
    }

    /// The running agent keeps working without its files: the install
    /// replaces it with the LaunchAgent's copy anyway.
    #[must_use]
    pub fn cleanup(&self) -> RemoteStep {
        RemoteStep {
            name: StepName::Cleanup,
            command: format!("/bin/rm -rf {}", quote(self.0.as_str())),
            stdin: Stdin::Null,
        }
    }
}

/// Who and where to log in: the provisioned account at the guest's address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    pub user: GuestUserName,
    pub address: Ipv4Addr,
}

impl SshTarget {
    /// The port Remote Login listens on.
    pub const PORT: u16 = 22;

    /// Arguments for `/usr/bin/ssh` to run `step` with the password from
    /// `SSH_ASKPASS` and no other authentication: no user config, no keys,
    /// no agent, one password attempt (a wrong password isn't retried into a
    /// lockout). Host keys go to the machine's own `known_hosts`, trusted on
    /// first use and checked after.
    #[must_use]
    pub fn ssh_args(&self, known_hosts: &Path, step: &RemoteStep) -> Vec<OsString> {
        let mut known = OsString::from("UserKnownHostsFile=");
        known.push(known_hosts);
        let mut args: Vec<OsString> = vec!["-F".into(), "none".into(), "-T".into()];
        for option in [
            known,
            "GlobalKnownHostsFile=/dev/null".into(),
            "StrictHostKeyChecking=accept-new".into(),
            "PreferredAuthentications=keyboard-interactive,password".into(),
            "PubkeyAuthentication=no".into(),
            "IdentityAgent=none".into(),
            "NumberOfPasswordPrompts=1".into(),
            "ConnectTimeout=10".into(),
            "ServerAliveInterval=5".into(),
            "ServerAliveCountMax=3".into(),
            "LogLevel=ERROR".into(),
        ] {
            args.push("-o".into());
            args.push(option);
        }
        args.push("-p".into());
        args.push(Self::PORT.to_string().into());
        args.push(format!("{}@{}", self.user.as_str(), self.address).into());
        args.push("--".into());
        args.push(step.command.clone().into());
        args
    }
}

/// What `ssh` asks its `SSH_ASKPASS` program, by its prompt argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskpassPrompt {
    /// `user@host's password:` or `(user@host) Password:`: answered with
    /// the stored password.
    Password,
    /// Anything else (a host key confirmation, a passphrase, a second
    /// factor) is refused, never answered with the password.
    Other(String),
}

impl AskpassPrompt {
    #[must_use]
    pub fn parse(prompt: &str) -> Self {
        let trimmed = prompt.trim_end();
        let lower = trimmed.to_ascii_lowercase();
        if lower.ends_with("password:") && !lower.contains("passphrase") {
            Self::Password
        } else {
            Self::Other(prompt.to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> StagingDir {
        StagingDir::parse(b"/tmp/doppel-bootstrap.Ab12Cd\n").unwrap()
    }

    #[test]
    fn staging_dirs_are_only_what_mktemp_makes() {
        assert_eq!(dir().path().as_str(), "/tmp/doppel-bootstrap.Ab12Cd");
        for bad in [
            &b""[..],
            b"/tmp/doppel-bootstrap.\n",
            b"/tmp/other.Ab12Cd\n",
            b"/tmp/doppel-bootstrap.Ab/../x\n",
            b"/tmp/doppel-bootstrap.a b\n",
            b"/tmp/doppel-bootstrap.a\n/tmp/doppel-bootstrap.b\n",
            b"tmp/doppel-bootstrap.Ab12Cd",
        ] {
            assert!(
                matches!(StagingDir::parse(bad), Err(BootstrapError::StagingDir(_))),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn the_plan_copies_unpacks_opens_and_cleans_up() {
        assert_eq!(
            StagingDir::make().command(),
            "/usr/bin/mktemp -d '/tmp/doppel-bootstrap.XXXXXX'"
        );
        let steps = dir().start_agent();
        let lines: Vec<_> = steps
            .iter()
            .map(|s| (s.name, s.command(), s.stdin))
            .collect();
        assert_eq!(
            lines,
            [
                (
                    StepName::Upload,
                    "/bin/cat > '/tmp/doppel-bootstrap.Ab12Cd/doppel-agent.zip'",
                    Stdin::Archive
                ),
                (
                    StepName::Unpack,
                    "/usr/bin/ditto -x -k '/tmp/doppel-bootstrap.Ab12Cd/doppel-agent.zip' '/tmp/doppel-bootstrap.Ab12Cd'",
                    Stdin::Null
                ),
                (
                    StepName::Open,
                    "/usr/bin/open '/tmp/doppel-bootstrap.Ab12Cd/Doppel Agent.app'",
                    Stdin::Null
                ),
            ]
        );
        assert_eq!(
            dir().cleanup().command(),
            "/bin/rm -rf '/tmp/doppel-bootstrap.Ab12Cd'"
        );
    }

    #[test]
    fn quoting_survives_single_quotes() {
        assert_eq!(quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn ssh_arguments_allow_only_the_password_and_end_with_the_command() {
        let target = SshTarget {
            user: "doppel".parse().unwrap(),
            address: Ipv4Addr::new(192, 168, 64, 7),
        };
        let step = dir().cleanup();
        let args = target.ssh_args(Path::new("/b/ssh-known-hosts"), &step);
        let args: Vec<_> = args.iter().map(|a| a.to_str().unwrap()).collect();
        let tail = &args[args.len() - 3..];
        assert_eq!(
            tail,
            [
                "doppel@192.168.64.7",
                "--",
                "/bin/rm -rf '/tmp/doppel-bootstrap.Ab12Cd'"
            ]
        );
        for wanted in [
            "UserKnownHostsFile=/b/ssh-known-hosts",
            "StrictHostKeyChecking=accept-new",
            "PubkeyAuthentication=no",
            "NumberOfPasswordPrompts=1",
        ] {
            assert!(args.contains(&wanted), "{wanted} missing from {args:?}");
        }
        assert_eq!(&args[..2], ["-F", "none"]);
    }

    #[test]
    fn only_password_prompts_get_the_password() {
        for prompt in [
            "doppel@192.168.64.7's password: ",
            "(doppel@192.168.64.7) Password:",
            "Password:",
        ] {
            assert_eq!(
                AskpassPrompt::parse(prompt),
                AskpassPrompt::Password,
                "{prompt}"
            );
        }
        for prompt in [
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
            "Enter passphrase for key '/x': ",
            "Verification code:",
            "",
        ] {
            assert_eq!(
                AskpassPrompt::parse(prompt),
                AskpassPrompt::Other(prompt.to_owned()),
                "{prompt}"
            );
        }
    }
}
