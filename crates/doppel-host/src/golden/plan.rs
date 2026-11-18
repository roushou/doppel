//! Preparing a machine to be a golden image: after this, a fork boots or
//! resumes to an agent-ready desktop without anyone at the keyboard.
//!
//! Through the running agent, as data the shell executes in order:
//! - the screensaver never starts (per user, no admin);
//! - as root, through `sudo -S` with the stored login password on stdin:
//!   no system, display or disk sleep (`pmset`), and auto-login for the user
//!   (`/etc/kcpassword` plus `autoLoginUser`, what `sysadminctl -autologin`
//!   writes; `sysadminctl` would take the password as an argument);
//! - a check that the settings took.
//!
//! The password never appears in an argument: it reaches `sudo` on stdin and
//! auto-login through a file. The screen lock itself is left alone (turning it
//! off also needs the password as an argument); with nothing going idle, it
//! only shows on resume, where `doppel login` handles it.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use doppel_desktop::exec::{ExecError, ExecOutput, ExecRequest, Termination};
use doppel_desktop::files::{FileError, FileMode, WritePolicy, WriteRequest};
use doppel_desktop::guest_path::{GuestPath, GuestPathError};
use doppel_machine::login::LoginPassword;

/// A macOS account short name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestUserName(String);

impl FromStr for GuestUserName {
    type Err = GoldenError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid = (1..=64).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
            && !s.starts_with('-');
        if valid {
            Ok(Self(s.to_owned()))
        } else {
            Err(GoldenError::Probe(format!(
                "{s:?} is not an account short name"
            )))
        }
    }
}

impl GuestUserName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Prints the user's short name.
    ///
    /// # Errors
    /// Never in practice: the request is fixed.
    pub fn probe() -> Result<ExecRequest, GoldenError> {
        Ok(ExecRequest::new(
            "/usr/bin/id".parse()?,
            vec!["-un".into()],
            Duration::from_secs(10),
            1024,
        )?)
    }

    /// # Errors
    /// [`GoldenError::Probe`] unless `id -un` exited 0 with a short name.
    pub fn parse(output: &ExecOutput) -> Result<Self, GoldenError> {
        if output.termination != Termination::Exited(0) {
            return Err(GoldenError::Probe(output.termination.to_string()));
        }
        std::str::from_utf8(&output.stdout)
            .map_err(|_| GoldenError::Probe("not UTF-8".into()))?
            .trim()
            .parse()
    }
}

/// The obfuscated password file macOS auto-login reads (`/etc/kcpassword`):
/// the password XORed with a fixed key, padded with NULs to a multiple of 12.
/// Not encryption; it is the password, and only root may read it.
#[derive(PartialEq, Eq)]
pub struct KcPassword(Vec<u8>);

impl KcPassword {
    const KEY: [u8; 11] = [
        0x7d, 0x89, 0x52, 0x23, 0xd2, 0xbc, 0xdd, 0xea, 0xa3, 0xb9, 0x1f,
    ];

    #[must_use]
    pub fn encode(password: &LoginPassword) -> Self {
        let mut bytes = password.keystrokes().as_bytes().to_vec();
        let remainder = bytes.len() % 12;
        if remainder != 0 {
            bytes.resize(bytes.len() + 12 - remainder, 0);
        }
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte ^= Self::KEY[i % Self::KEY.len()];
        }
        Self(bytes)
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl fmt::Debug for KcPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KcPassword(<{} bytes, redacted>)", self.0.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GoldenError {
    #[error("couldn't read the guest user: {0}")]
    Probe(String),
    #[error(transparent)]
    Path(#[from] GuestPathError),
    #[error(transparent)]
    File(#[from] FileError),
    #[error(transparent)]
    Exec(#[from] ExecError),
    #[error("{step} failed: {termination}{stderr}")]
    StepFailed {
        step: GoldenStepName,
        termination: Termination,
        /// `": <stderr>"`, or empty.
        stderr: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoldenStepName {
    Screensaver,
    UploadAutoLogin,
    AdminSettings,
    Verify,
}

impl fmt::Display for GoldenStepName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Screensaver => "turning the screensaver off",
            Self::UploadAutoLogin => "uploading the auto-login file",
            Self::AdminSettings => "applying sleep and auto-login settings as root",
            Self::Verify => "checking the settings",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoldenAction {
    Write(WriteRequest),
    /// Must exit 0.
    Run(ExecRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoldenStep {
    pub name: GoldenStepName,
    pub action: GoldenAction,
}

impl GoldenStep {
    /// # Errors
    /// [`GoldenError::StepFailed`] unless it exited 0.
    pub fn check(&self, output: &ExecOutput) -> Result<(), GoldenError> {
        match output.termination {
            Termination::Exited(0) => Ok(()),
            termination @ (Termination::Exited(_)
            | Termination::Signaled(_)
            | Termination::TimedOut) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stderr = stderr.trim();
                Err(GoldenError::StepFailed {
                    step: self.name,
                    termination,
                    stderr: if stderr.is_empty() {
                        String::new()
                    } else {
                        format!(": {stderr}")
                    },
                })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct GoldenPlan {
    pub steps: Vec<GoldenStep>,
}

/// As root. `$1` user, `$2` the uploaded kcpassword (removed either way).
const ADMIN: &str = r#"set -eu
user=$1 kc=$2
trap 'rm -f "$kc"' EXIT
/usr/bin/pmset -a sleep 0 displaysleep 0 disksleep 0
/usr/bin/install -m 600 -o root -g wheel "$kc" /etc/kcpassword
/usr/bin/defaults write /Library/Preferences/com.apple.loginwindow autoLoginUser -string "$user""#;

/// As the user. `$1` user. Fails, naming the setting, unless all took.
const VERIFY: &str = r#"set -eu
fail() { echo "$1" >&2; exit 1; }
[ "$(/usr/bin/defaults read /Library/Preferences/com.apple.loginwindow autoLoginUser)" = "$1" ] || fail "autoLoginUser is not $1"
[ -f /etc/kcpassword ] || fail "/etc/kcpassword is missing"
settings=$(/usr/bin/pmset -g custom)
for key in sleep displaysleep disksleep; do
  echo "$settings" | grep -Eq "^ *$key +0$" || fail "pmset $key is not 0"
done
[ "$(/usr/bin/defaults -currentHost read com.apple.screensaver idleTime)" = 0 ] || fail "screensaver idleTime is not 0""#;

impl GoldenPlan {
    /// # Errors
    /// [`GoldenError`] only for limits that a fixed plan never reaches.
    pub fn new(user: &GuestUserName, password: &LoginPassword) -> Result<Self, GoldenError> {
        let sh: GuestPath = "/bin/sh".parse()?;
        let kc: GuestPath = "/tmp/dev.doppel.kcpassword".parse()?;
        let run = |program: GuestPath, args: Vec<String>| {
            ExecRequest::new(program, args, Duration::from_secs(60), 64 * 1024)
        };
        let script = |body: &str, args: &[&str]| {
            let mut all = vec!["-c".to_owned(), body.to_owned(), "golden".to_owned()];
            all.extend(args.iter().map(|a| (*a).to_owned()));
            all
        };
        let mut sudo_args = vec![
            "-S".to_owned(),
            "-k".to_owned(),
            "-p".to_owned(),
            String::new(),
            sh.as_str().to_owned(),
        ];
        sudo_args.extend(script(ADMIN, &[user.as_str(), kc.as_str()]));
        let mut stdin = password.keystrokes().as_bytes().to_vec();
        stdin.push(b'\n');
        Ok(Self {
            steps: vec![
                GoldenStep {
                    name: GoldenStepName::Screensaver,
                    action: GoldenAction::Run(run(
                        "/usr/bin/defaults".parse()?,
                        [
                            "-currentHost",
                            "write",
                            "com.apple.screensaver",
                            "idleTime",
                            "-int",
                            "0",
                        ]
                        .map(str::to_owned)
                        .to_vec(),
                    )?),
                },
                GoldenStep {
                    name: GoldenStepName::UploadAutoLogin,
                    action: GoldenAction::Write(WriteRequest::new(
                        kc,
                        KcPassword::encode(password).into_bytes(),
                        FileMode::new(0o600)?,
                        WritePolicy::Replace,
                    )?),
                },
                GoldenStep {
                    name: GoldenStepName::AdminSettings,
                    action: GoldenAction::Run(
                        run("/usr/bin/sudo".parse()?, sudo_args)?.with_stdin(stdin),
                    ),
                },
                GoldenStep {
                    name: GoldenStepName::Verify,
                    action: GoldenAction::Run(run(sh, script(VERIFY, &[user.as_str()]))?),
                },
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doppel_desktop::exec::Completeness;

    fn password() -> LoginPassword {
        LoginPassword::parse(b"s3cret-pass").unwrap()
    }

    #[test]
    fn kcpassword_matches_the_known_encoding() {
        // "password": 8 bytes, padded to 12, XORed with the fixed key.
        let encoded = KcPassword::encode(&LoginPassword::parse(b"password").unwrap()).into_bytes();
        let expected: Vec<u8> = b"password\0\0\0\0"
            .iter()
            .zip(KcPassword::KEY.iter().cycle())
            .map(|(b, k)| b ^ k)
            .collect();
        assert_eq!(encoded, expected);
        assert_eq!(encoded[..4], [0x0d, 0xe8, 0x21, 0x50]);
        // Exactly 12 bytes: no extra block; 13 bytes: two blocks.
        assert_eq!(
            KcPassword::encode(&LoginPassword::parse(&[b'a'; 12]).unwrap())
                .0
                .len(),
            12
        );
        assert_eq!(
            KcPassword::encode(&LoginPassword::parse(&[b'a'; 13]).unwrap())
                .0
                .len(),
            24
        );
        assert_eq!(
            format!("{:?}", KcPassword::encode(&password())),
            "KcPassword(<12 bytes, redacted>)"
        );
    }

    #[test]
    fn user_names_parse_from_the_probe() {
        let out = |code, text: &str| ExecOutput {
            termination: Termination::Exited(code),
            stdout: text.into(),
            stderr: Vec::new(),
            completeness: Completeness::Full,
        };
        assert_eq!(
            GuestUserName::parse(&out(0, "doppelsandbox\n"))
                .unwrap()
                .as_str(),
            "doppelsandbox"
        );
        for bad in [
            out(1, "x\n"),
            out(0, ""),
            out(0, "a b\n"),
            out(0, "-x\n"),
            out(0, "a;rm\n"),
        ] {
            assert!(GuestUserName::parse(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_password_reaches_sudo_on_stdin_and_never_an_argument() {
        let user: GuestUserName = "doppelsandbox".parse().unwrap();
        let plan = GoldenPlan::new(&user, &password()).unwrap();
        let names: Vec<_> = plan.steps.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                GoldenStepName::Screensaver,
                GoldenStepName::UploadAutoLogin,
                GoldenStepName::AdminSettings,
                GoldenStepName::Verify
            ]
        );
        for step in &plan.steps {
            match &step.action {
                GoldenAction::Run(request) => {
                    for argument in request.arguments() {
                        assert!(
                            !argument.contains("s3cret"),
                            "{:?} has the password in an argument",
                            step.name
                        );
                    }
                    assert!(!format!("{request:?}").contains("s3cret"));
                }
                GoldenAction::Write(write) => {
                    assert!(!format!("{write:?}").contains("s3cret"));
                    assert!(
                        !write.data().windows(6).any(|w| w == b"s3cret"),
                        "kcpassword is encoded"
                    );
                    assert_eq!(write.mode.bits(), 0o600);
                }
            }
        }
        let GoldenAction::Run(admin) = &plan.steps[2].action else {
            panic!("admin runs")
        };
        assert_eq!(admin.program().as_str(), "/usr/bin/sudo");
        assert_eq!(&admin.arguments()[..5], ["-S", "-k", "-p", "", "/bin/sh"]);
        assert_eq!(admin.stdin(), b"s3cret-pass\n");
        assert_eq!(
            &admin.arguments()[8..],
            ["doppelsandbox", "/tmp/dev.doppel.kcpassword"]
        );
    }

    #[test]
    fn failed_steps_name_themselves() {
        let plan = GoldenPlan::new(&"u".parse().unwrap(), &password()).unwrap();
        let error = plan.steps[3]
            .check(&ExecOutput {
                termination: Termination::Exited(1),
                stdout: Vec::new(),
                stderr: b"pmset sleep is not 0\n".to_vec(),
                completeness: Completeness::Full,
            })
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "checking the settings failed: exited with status 1: pmset sleep is not 0"
        );
    }
}
