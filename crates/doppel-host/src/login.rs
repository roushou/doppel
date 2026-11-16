//! Unattended login: the password file in a machine's bundle, and the shell
//! that runs the core's login machine (probing through the agent, typing
//! through the host keyboard).

use std::fs;
use std::io::{self, BufRead, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use doppel_machine::bundle::MachineBundle;
use doppel_machine::exec::ExecRequest;
use doppel_machine::input::{Input, Key, KeyChord, NamedKey};
use doppel_machine::login::{
    Action, Credentials, Event, LoginOutcome, LoginPassword, LoginPolicy, Observation,
    PasswordError, Unattended,
};
use doppel_machine::machine::MachineName;
use doppel_machine::outcome::{Certainty, Failure};

use crate::capabilities::Clock;
use crate::files::Filesystem;
use crate::machines::{MachineError, Machines};
use doppel_protocol::transport::{Cancel, Slept};

#[derive(Debug, thiserror::Error)]
pub enum PasswordFileError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path}: {source}")]
    Invalid {
        path: PathBuf,
        #[source]
        source: PasswordError,
    },
    #[error("{path} belongs to uid {owner}, not to you (uid {you})")]
    NotYours { path: PathBuf, owner: u32, you: u32 },
}

#[derive(Debug, thiserror::Error)]
pub enum AwaitDesktopError {
    #[error(transparent)]
    Password(#[from] PasswordFileError),
    #[error(transparent)]
    Machine(#[from] MachineError),
}

/// A bad password file stops the wait before it starts.
impl Failure for AwaitDesktopError {
    fn certainty(&self) -> Certainty {
        match self {
            Self::Password(_) => Certainty::NotDone,
            Self::Machine(error) => error.certainty(),
        }
    }
}

/// A machine's stored login password, on disk.
#[derive(Debug, Clone, Copy, Default)]
pub struct PasswordFile;

impl PasswordFile {
    /// `None` when no password is stored.
    ///
    /// # Errors
    /// [`PasswordFileError`]: unreadable, not private, not owned by this
    /// user, or not a valid password. Errors never contain the password.
    pub fn read(self, bundle: &MachineBundle) -> Result<Option<LoginPassword>, PasswordFileError> {
        self.read_at(&bundle.login_password())
    }

    /// [`Self::read`] for a password file given by its path.
    ///
    /// # Errors
    /// As [`Self::read`].
    pub fn read_at(self, path: &Path) -> Result<Option<LoginPassword>, PasswordFileError> {
        let path = path.to_owned();
        let io = |source| PasswordFileError::Io {
            path: path.clone(),
            source,
        };
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io(error)),
        };
        let invalid = |source| PasswordFileError::Invalid {
            path: path.clone(),
            source,
        };
        LoginPassword::check_file_mode(metadata.permissions().mode()).map_err(invalid)?;
        let you = nix::unistd::geteuid().as_raw();
        if metadata.uid() != you {
            return Err(PasswordFileError::NotYours {
                path: path.clone(),
                owner: metadata.uid(),
                you,
            });
        }
        let bytes = fs::read(&path).map_err(io)?;
        LoginPassword::parse(&bytes).map(Some).map_err(invalid)
    }

    /// Stores a password atomically: a temporary file created with mode 600,
    /// flushed, then renamed over the old one.
    ///
    /// # Errors
    /// [`PasswordFileError::Io`].
    pub fn write(
        self,
        bundle: &MachineBundle,
        password: &LoginPassword,
    ) -> Result<PathBuf, PasswordFileError> {
        let path = bundle.login_password();
        let temporary = bundle.dir().join(".login-password.tmp");
        let io = |at: &Path| {
            let at = at.to_owned();
            move |source| PasswordFileError::Io { path: at, source }
        };
        let result = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .and_then(|mut file| {
                file.write_all(&password.file_contents())?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&temporary, &path));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(io(&temporary))?;
        Ok(path)
    }

    /// # Errors
    /// [`PasswordFileError::Io`]; a missing file is not an error.
    pub fn forget(self, bundle: &MachineBundle) -> Result<bool, PasswordFileError> {
        let path = bundle.login_password();
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(PasswordFileError::Io { path, source }),
        }
    }
}

/// One line from standard input, with echo off when it's a terminal.
#[derive(Debug, Clone, Copy, Default)]
pub struct SecretPrompt;

impl SecretPrompt {
    /// # Errors
    /// Reading stdin, or changing the terminal's echo.
    pub fn read_line(self, prompt: &str) -> io::Result<Vec<u8>> {
        let stdin = io::stdin();
        let terminal = nix::unistd::isatty(stdin.as_fd()).unwrap_or(false);
        let saved = if terminal {
            eprint!("{prompt}");
            let saved = nix::sys::termios::tcgetattr(stdin.as_fd())?;
            let mut quiet = saved.clone();
            quiet
                .local_flags
                .remove(nix::sys::termios::LocalFlags::ECHO);
            nix::sys::termios::tcsetattr(
                stdin.as_fd(),
                nix::sys::termios::SetArg::TCSANOW,
                &quiet,
            )?;
            Some(saved)
        } else {
            None
        };
        let mut line = Vec::new();
        let read = stdin.lock().read_until(b'\n', &mut line);
        if let Some(saved) = saved {
            nix::sys::termios::tcsetattr(
                stdin.as_fd(),
                nix::sys::termios::SetArg::TCSANOW,
                &saved,
            )?;
            eprintln!();
        }
        read?;
        Ok(line)
    }
}

impl<C: Clock, F: Filesystem> Machines<C, F> {
    /// [`Machines::sign_in`] with the password stored in the machine's bundle, if any.
    ///
    /// # Errors
    /// [`AwaitDesktopError`]: a bad password file, or see `sign_in`.
    pub(crate) fn await_desktop(
        &self,
        name: &MachineName,
        policy: LoginPolicy,
        cancel: &Cancel,
    ) -> Result<LoginOutcome, AwaitDesktopError> {
        let password = PasswordFile.read(&self.store().bundle(name))?;
        Ok(self.sign_in(name, password.as_ref(), policy, cancel)?)
    }

    /// Brings the guest to an active session: waits for the agent and, if
    /// the session is locked, types the stored password once. See
    /// [`doppel_machine::login`] for when it types and when it refuses to.
    ///
    /// # Errors
    /// [`MachineError`] when the machine isn't running, host input fails or
    /// the caller cancels; every other result, ready or not, is a
    /// [`LoginOutcome`].
    fn sign_in(
        &self,
        name: &MachineName,
        password: Option<&LoginPassword>,
        policy: LoginPolicy,
        cancel: &Cancel,
    ) -> Result<LoginOutcome, MachineError> {
        let machine = Unattended {
            policy,
            credentials: match password {
                Some(_) => Credentials::Stored,
                None => Credentials::None,
            },
        };
        let clock = self.clock();
        let (mut state, mut pending) = machine.start(clock.now());
        loop {
            let mut events = Vec::new();
            for action in pending {
                match action {
                    Action::Finish(outcome) => return Ok(outcome),
                    Action::ProbeAt(at) => {
                        match cancel.sleep(at.saturating_duration_since(clock.now())) {
                            Slept::Elapsed => {}
                            Slept::Cancelled => return Err(MachineError::Cancelled),
                        }
                        let observation = self.observe_session(name)?;
                        events.push(Event::Observed {
                            observation,
                            now: clock.now(),
                        });
                    }
                    Action::Wake => {
                        // Best effort: a display that stays dark only delays the next probe.
                        if let Some(wake) = Self::wake_request() {
                            let _ = self
                                .agent(name, self.deadline())
                                .and_then(|mut agent| Ok(agent.exec(&wake, cancel)?));
                        }
                        events.push(Event::Woke { now: clock.now() });
                    }
                    Action::TypePassword => {
                        // The core only types with Credentials::Stored, which
                        // means a password was given; this arm can't be reached otherwise.
                        let Some(password) = password else {
                            return Ok(LoginOutcome::NeedsPassword);
                        };
                        // Built here and dropped at once: `Input` derives Debug,
                        // so it must never reach a log or an error.
                        let by = self.deadline();
                        let mut running = self.machine(name).running(by)?;
                        running.host_input(&Input::Type(password.keystrokes().to_owned()), by)?;
                        running.host_input(&Input::Key(Self::return_key()), by)?;
                        events.push(Event::Typed { now: clock.now() });
                    }
                }
            }
            pending = Vec::new();
            for event in events {
                let (next, actions) = machine.step(state, event).map_err(MachineError::Login)?;
                state = next;
                pending.extend(actions);
            }
        }
    }

    /// The agent's session state; any failure to get it is an observation,
    /// except the machine not running or an agent that answers wrongly.
    fn observe_session(&self, name: &MachineName) -> Result<Observation, MachineError> {
        let by = self.deadline();
        match self.agent(name, by) {
            Ok(mut agent) => Ok(match agent.status(by) {
                Ok(status) => Observation::Session(status.session),
                Err(error) => Observation::AgentUnreachable(error.to_string()),
            }),
            Err(error @ MachineError::AgentUnreachable { .. }) => {
                Ok(Observation::AgentUnreachable(error.to_string()))
            }
            Err(MachineError::AgentHandshake { source, .. }) if source.is_transient() => {
                Ok(Observation::AgentUnreachable(source.to_string()))
            }
            Err(error) => Err(error),
        }
    }

    /// `caffeinate -u` declares user activity for a second, which lights the display.
    fn wake_request() -> Option<ExecRequest> {
        ExecRequest::new(
            "/usr/bin/caffeinate".parse().ok()?,
            vec!["-u".into(), "-t".into(), "1".into()],
            Duration::from_secs(5),
            1024,
        )
        .ok()
    }

    const fn return_key() -> KeyChord {
        KeyChord {
            modifiers: Vec::new(),
            key: Key::Named(NamedKey::Return),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use doppel_machine::bundle::{DataRoot, MachineBundle};
    use doppel_machine::login::{LoginPassword, PasswordError};

    use super::{PasswordFile, PasswordFileError};

    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn bundle(tag: &str) -> (Scratch, MachineBundle) {
        let root = std::env::temp_dir().join(format!("doppel-login-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let bundle = DataRoot::new(root.clone())
            .unwrap()
            .machine(&"dev".parse().unwrap());
        fs::create_dir_all(bundle.dir()).unwrap();
        (Scratch(root), bundle)
    }

    #[test]
    fn stored_passwords_are_private_and_round_trip() {
        let (_scratch, bundle) = bundle("round");
        assert!(
            PasswordFile.read(&bundle).unwrap().is_none(),
            "nothing stored yet"
        );
        let password = LoginPassword::parse(b"correct horse").unwrap();
        let path = PasswordFile.write(&bundle, &password).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read(&path).unwrap(), b"correct horse\n");
        assert_eq!(PasswordFile.read(&bundle).unwrap(), Some(password));
        // Replacing works and leaves no temporary file.
        PasswordFile
            .write(&bundle, &LoginPassword::parse(b"second").unwrap())
            .unwrap();
        assert_eq!(
            PasswordFile.read(&bundle).unwrap().unwrap().keystrokes(),
            "second"
        );
        let names: Vec<_> = fs::read_dir(bundle.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["login-password"]);
        assert!(PasswordFile.forget(&bundle).unwrap());
        assert!(!PasswordFile.forget(&bundle).unwrap());
        assert!(PasswordFile.read(&bundle).unwrap().is_none());
    }

    #[test]
    fn exposed_or_invalid_files_are_refused_without_showing_them() {
        let (_scratch, bundle) = bundle("bad");
        let path = bundle.login_password();
        fs::write(&path, "hunter2-secret\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let error = PasswordFile.read(&bundle).unwrap_err();
        assert!(
            matches!(
                error,
                PasswordFileError::Invalid {
                    source: PasswordError::Exposed(0o640),
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(!error.to_string().contains("hunter2"), "{error}");
        assert!(!format!("{error:?}").contains("hunter2"), "{error:?}");

        fs::write(&path, "two\nlines\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let error = PasswordFile.read(&bundle).unwrap_err();
        assert!(
            matches!(
                error,
                PasswordFileError::Invalid {
                    source: PasswordError::Multiline,
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(!error.to_string().contains("lines"), "{error}");
    }
}
