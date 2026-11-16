//! doppeld as a launchd agent: launchd binds the daemon's socket at login
//! and starts `doppel daemon run --launchd` on the first connection to it,
//! so nothing runs until a client needs it.
//!
//! [`LaunchAgent`] is the job's description and renders its plist (pure);
//! [`Launchctl`] runs `launchctl`, and [`Launchd::socket`] takes the socket
//! launchd bound.

use std::ffi::{CString, OsStr, OsString};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The job: what launchd runs, the socket it binds for it, and its log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchAgent {
    /// `doppel`, then the arguments that make it run the daemon.
    pub(crate) program: Vec<OsString>,
    pub(crate) socket: PathBuf,
    pub(crate) log: PathBuf,
}

impl LaunchAgent {
    pub(crate) const LABEL: &'static str = "dev.doppel.doppeld";
    /// The name launchd and the daemon agree on for the socket.
    const SOCKETS: &'static str = "Listeners";

    /// Where a user's launch agents live.
    pub(crate) fn plist_path(home: &Path) -> PathBuf {
        home.join("Library/LaunchAgents")
            .join(format!("{}.plist", Self::LABEL))
    }

    /// The property list. No `KeepAlive` and no `RunAtLoad`: launchd starts
    /// the daemon only when a client connects. The socket is the user's
    /// own (mode 0600, written in decimal as plists take it).
    pub(crate) fn plist(&self) -> String {
        let arguments: String = self
            .program
            .iter()
            .map(|argument| format!("\t\t<string>{}</string>\n", Self::escaped(argument)))
            .collect();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
{arguments}	</array>
	<key>Sockets</key>
	<dict>
		<key>{sockets}</key>
		<dict>
			<key>SockPathName</key>
			<string>{socket}</string>
			<key>SockPathMode</key>
			<integer>384</integer>
		</dict>
	</dict>
	<key>StandardErrorPath</key>
	<string>{log}</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#,
            label = Self::LABEL,
            sockets = Self::SOCKETS,
            socket = Self::escaped(self.socket.as_os_str()),
            log = Self::escaped(self.log.as_os_str()),
        )
    }

    /// XML text: the five characters that markup reserves, escaped.
    fn escaped(text: &OsStr) -> String {
        text.to_string_lossy()
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }
}

/// `launchctl`, on the user's GUI domain.
#[derive(Debug)]
pub(crate) struct Launchctl {
    domain: String,
}

/// What launchd says of the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobState {
    NotLoaded,
    /// Loaded, waiting for a connection.
    Waiting,
    Running {
        pid: u32,
    },
}

impl Launchctl {
    pub(crate) fn for_user(uid: u32) -> Self {
        Self {
            domain: format!("gui/{uid}"),
        }
    }

    fn service(&self) -> String {
        format!("{}/{}", self.domain, LaunchAgent::LABEL)
    }

    /// Loads the job from its plist.
    pub(crate) fn bootstrap(&self, plist: &Path) -> Result<()> {
        Self::run(
            Command::new("launchctl")
                .arg("bootstrap")
                .arg(&self.domain)
                .arg(plist),
        )
    }

    /// Unloads the job, stopping the daemon if it runs. Not an error when
    /// it isn't loaded.
    pub(crate) fn bootout(&self) -> Result<()> {
        match self.state()? {
            JobState::NotLoaded => Ok(()),
            JobState::Waiting | JobState::Running { .. } => {
                Self::run(Command::new("launchctl").arg("bootout").arg(self.service()))
            }
        }
    }

    /// Stops the running daemon; launchd starts it again on the next
    /// connection.
    pub(crate) fn stop(&self) -> Result<()> {
        Self::run(
            Command::new("launchctl")
                .args(["kill", "SIGTERM"])
                .arg(self.service()),
        )
    }

    pub(crate) fn state(&self) -> Result<JobState> {
        let output = Command::new("launchctl")
            .arg("print")
            .arg(self.service())
            .output()
            .context("running launchctl")?;
        if !output.status.success() {
            return Ok(JobState::NotLoaded);
        }
        Ok(JobState::parse(&String::from_utf8_lossy(&output.stdout)))
    }

    fn run(command: &mut Command) -> Result<()> {
        let output = command.output().context("running launchctl")?;
        if !output.status.success() {
            bail!(
                "{:?} failed: {}",
                command,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}

impl JobState {
    /// From `launchctl print`: the job runs when it reports a pid.
    fn parse(printed: &str) -> Self {
        printed
            .lines()
            .filter_map(|line| line.trim().strip_prefix("pid = "))
            .find_map(|pid| pid.trim().parse().ok())
            .map_or(Self::Waiting, |pid| Self::Running { pid })
    }
}

/// The installed job: its plist on disk, and launchd's view of it.
#[derive(Debug)]
pub(crate) struct Service {
    agent: LaunchAgent,
    plist: PathBuf,
    launchctl: Launchctl,
}

impl Service {
    pub(crate) fn new(agent: LaunchAgent, user_home: &Path, uid: u32) -> Self {
        Self {
            agent,
            plist: LaunchAgent::plist_path(user_home),
            launchctl: Launchctl::for_user(uid),
        }
    }

    /// Writes the plist and loads it, replacing a job loaded before. A
    /// daemon run by hand on the socket must stop first.
    pub(crate) fn install(&self) -> Result<()> {
        if self.launchctl.state()? == JobState::NotLoaded
            && UnixStream::connect(&self.agent.socket).is_ok()
        {
            bail!(
                "a daemon is already answering on {}: stop it (the `doppel daemon run` process) first",
                self.agent.socket.display()
            );
        }
        self.launchctl.bootout()?;
        for dir in [self.plist.parent(), self.agent.socket.parent()]
            .into_iter()
            .flatten()
        {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        Self::remove_if_present(&self.agent.socket)?;
        std::fs::write(&self.plist, self.agent.plist())
            .with_context(|| format!("writing {}", self.plist.display()))?;
        self.launchctl.bootstrap(&self.plist)
    }

    /// Unloads the job, stopping the daemon, and removes its plist and
    /// socket. Machines it started keep running.
    pub(crate) fn uninstall(&self) -> Result<()> {
        self.launchctl.bootout()?;
        Self::remove_if_present(&self.plist)?;
        Self::remove_if_present(&self.agent.socket)
    }

    /// Stops the running daemon, if one runs; the next client starts it.
    pub(crate) fn stop(&self) -> Result<Option<u32>> {
        match self.launchctl.state()? {
            JobState::Running { pid } => {
                self.launchctl.stop()?;
                Ok(Some(pid))
            }
            JobState::NotLoaded | JobState::Waiting => Ok(None),
        }
    }

    /// What is installed and running, without starting the daemon:
    /// connecting to launchd's socket would.
    pub(crate) fn status(&self) -> Result<String> {
        let installed = if self.plist.exists() {
            format!("installed: {}", self.plist.display())
        } else {
            "not installed (run `doppel daemon install`)".to_owned()
        };
        let running = match self.launchctl.state()? {
            JobState::Running { pid } => format!("running (pid {pid})"),
            JobState::Waiting => "loaded; starts on the first connection".to_owned(),
            JobState::NotLoaded => {
                if UnixStream::connect(&self.agent.socket).is_ok() {
                    "not loaded; a daemon run by hand answers on the socket".to_owned()
                } else {
                    "not loaded".to_owned()
                }
            }
        };
        Ok(format!(
            "{installed}\nlaunchd: {running}\nsocket: {}\nlog: {}",
            self.agent.socket.display(),
            self.agent.log.display()
        ))
    }

    /// The last `lines` lines of the daemon's log.
    pub(crate) fn log_tail(&self, lines: usize) -> Result<String> {
        let text = match std::fs::read_to_string(&self.agent.log) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", self.agent.log.display()));
            }
        };
        let all: Vec<&str> = text.lines().collect();
        let from = all.len().saturating_sub(lines);
        Ok(all
            .iter()
            .skip(from)
            .map(|line| format!("{line}\n"))
            .collect())
    }

    fn remove_if_present(path: &Path) -> Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
        }
    }
}

/// The daemon's side of launchd.
#[derive(Debug)]
pub(crate) struct Launchd;

mod ffi {
    use std::ffi::{c_char, c_int};

    #[expect(unsafe_code, reason = "declaring launchd's C API")]
    unsafe extern "C" {
        /// libSystem: the descriptors launchd bound for the socket `name`
        /// of the calling job, in a buffer the caller frees.
        pub(super) fn launch_activate_socket(
            name: *const c_char,
            fds: *mut *mut c_int,
            count: *mut usize,
        ) -> c_int;
    }
}

impl Launchd {
    /// The socket launchd bound for this job.
    pub(crate) fn socket() -> Result<UnixListener> {
        let name = CString::new(LaunchAgent::SOCKETS).context("socket name")?;
        let mut fds: *mut std::ffi::c_int = std::ptr::null_mut();
        let mut count: usize = 0;
        #[expect(unsafe_code, reason = "calling launchd's C API")]
        // SAFETY: `name` is a valid C string; `fds` and `count` are valid
        // out-pointers for the call.
        let status =
            unsafe { ffi::launch_activate_socket(name.as_ptr(), &raw mut fds, &raw mut count) };
        if status != 0 {
            bail!(
                "launchd has no socket for this process ({}): run `doppel daemon install`, or `doppel daemon run` without --launchd",
                std::io::Error::from_raw_os_error(status)
            );
        }
        #[expect(unsafe_code, reason = "reading and freeing launchd's buffer")]
        // SAFETY: on success launchd allocated `count` descriptors at `fds`,
        // fresh in this process and owned by no one else; the buffer is
        // ours to free with `free`.
        let owned: Vec<OwnedFd> = unsafe {
            let raw = std::slice::from_raw_parts(fds, count).to_vec();
            nix::libc::free(fds.cast());
            raw.into_iter().map(|fd| OwnedFd::from_raw_fd(fd)).collect()
        };
        let mut owned = owned.into_iter();
        match (owned.next(), owned.next()) {
            (Some(fd), None) => Ok(UnixListener::from(fd)),
            (None, _) => bail!("launchd passed no socket"),
            (Some(_), Some(_)) => bail!("launchd passed more than one socket"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_starts_the_daemon_on_demand_on_a_private_socket() {
        let agent = LaunchAgent {
            program: [
                "/opt/doppel/doppel",
                "--home",
                "/Users/a & b/Doppel",
                "daemon",
                "run",
                "--launchd",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
            socket: "/Users/a & b/Doppel/doppeld.socket".into(),
            log: "/Users/a & b/Doppel/doppeld.log".into(),
        };
        let plist = agent.plist();
        assert!(plist.contains("<string>dev.doppel.doppeld</string>"));
        assert!(plist.contains("<string>/Users/a &amp; b/Doppel</string>"));
        assert!(plist.contains("<string>--launchd</string>"));
        assert!(plist.contains("<key>SockPathMode</key>\n\t\t\t<integer>384</integer>"));
        assert!(!plist.contains("KeepAlive") && !plist.contains("RunAtLoad"));
    }

    #[test]
    fn a_running_job_reports_its_pid() {
        let printed = "gui/501/dev.doppel.doppeld = {\n\tactive count = 1\n\tstate = running\n\tpid = 4242\n}";
        assert_eq!(JobState::parse(printed), JobState::Running { pid: 4242 });
        let waiting = "gui/501/dev.doppel.doppeld = {\n\tstate = not running\n}";
        assert_eq!(JobState::parse(waiting), JobState::Waiting);
    }
}
