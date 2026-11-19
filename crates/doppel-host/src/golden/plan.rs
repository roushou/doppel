//! Preparing a machine to be a golden image: after this, a fork boots or
//! resumes to an agent-ready desktop without anyone at the keyboard.
//!
//! Through the running agent, as data the shell executes in order:
//! - the screensaver never starts (per user, no admin);
//! - as root, through `sudo -S` with the stored login password on stdin:
//!   no system, display or disk sleep (`pmset`), and auto-login for the user
//!   (`/etc/kcpassword` plus `autoLoginUser`, what `sysadminctl -autologin`
//!   writes; `sysadminctl` would take the password as an argument);
//! - a wait until every setting is in its file on disk, then a flush: a
//!   fork cold-boots from the checkpoint's disk, and the daemons behind
//!   `pmset` and `defaults` write their files some seconds after the change.
//!
//! The password never appears in an argument: it reaches `sudo` on stdin and
//! auto-login through a file. The screen lock itself is left alone (turning it
//! off also needs the password as an argument); with nothing going idle, it
//! only shows on resume, where `doppel login` handles it.

use std::fmt;
use std::time::Duration;

use doppel_desktop::exec::{ExecError, ExecRequest};
use doppel_desktop::files::{FileError, FileMode, WritePolicy, WriteRequest};
use doppel_desktop::guest_path::{GuestPath, GuestPathError};
use doppel_machine::login::LoginPassword;

use crate::guest_step::plan::{GuestAction, GuestStep, RunFailed};
use crate::guest_user::GuestUserError;
pub use crate::guest_user::GuestUserName;

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
    User(#[from] GuestUserError),
    #[error(transparent)]
    Path(#[from] GuestPathError),
    #[error(transparent)]
    File(#[from] FileError),
    #[error(transparent)]
    Exec(#[from] ExecError),
    #[error("{step} failed: {failed}")]
    StepFailed {
        step: GoldenStepName,
        failed: RunFailed,
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
#[must_use]
pub struct GoldenPlan {
    pub steps: Vec<GuestStep<GoldenStepName>>,
}

/// As root. `$1` user, `$2` the uploaded kcpassword (removed either way).
const ADMIN: &str = r#"set -eu
user=$1 kc=$2
trap 'rm -f "$kc"' EXIT
/usr/bin/pmset -a sleep 0 displaysleep 0 disksleep 0
/usr/bin/install -m 600 -o root -g wheel "$kc" /etc/kcpassword
/usr/bin/defaults write /Library/Preferences/com.apple.loginwindow autoLoginUser -string "$user""#;

/// As the user. `$1` user. Waits until every setting is in its file on disk,
/// then flushes the disk; fails, naming the setting, if one never gets there.
///
/// A fork cold-boots from the checkpoint's disk, so only what reached the
/// disk counts. `pmset` and `defaults` take effect at once but their daemons
/// (powerd, cfprefsd) write the files some seconds later, and the checkpoint
/// that follows powers the machine off.
const VERIFY: &str = r#"set -eu
user=$1
fail() { echo "$1" >&2; exit 1; }
value() { [ -f "$1" ] && /usr/libexec/PlistBuddy -c "Print '$2'" "$1" 2>/dev/null || true; }
uuid=$(/usr/sbin/ioreg -rd1 -c IOPlatformExpertDevice | /usr/bin/awk -F'"' '/IOPlatformUUID/ {print $4}')
power=/Library/Preferences/com.apple.PowerManagement.plist
saver="$HOME/Library/Preferences/ByHost/com.apple.screensaver.$uuid.plist"
unsaved() {
  [ "$(value /Library/Preferences/com.apple.loginwindow.plist :autoLoginUser)" = "$user" ] || { echo "autoLoginUser is not $user on disk"; return; }
  [ -f /etc/kcpassword ] || { echo "/etc/kcpassword is missing"; return; }
  for timer in "System Sleep Timer" "Display Sleep Timer" "Disk Sleep Timer"; do
    [ "$(value "$power" ":AC Power:$timer")" = 0 ] || { echo "pmset's $timer is not 0 on disk"; return; }
  done
  [ "$(value "$saver" :idleTime)" = 0 ] || { echo "screensaver idleTime is not 0 on disk"; return; }
}
waited=0
while missing=$(unsaved); [ -n "$missing" ]; do
  [ "$waited" -lt 45 ] || fail "$missing"
  waited=$((waited + 1))
  sleep 1
done
/bin/sync"#;

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
                GuestStep {
                    name: GoldenStepName::Screensaver,
                    action: GuestAction::Run(run(
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
                GuestStep {
                    name: GoldenStepName::UploadAutoLogin,
                    action: GuestAction::Write(WriteRequest::new(
                        kc,
                        KcPassword::encode(password).into_bytes(),
                        FileMode::new(0o600)?,
                        WritePolicy::Replace,
                    )?),
                },
                GuestStep {
                    name: GoldenStepName::AdminSettings,
                    action: GuestAction::Run(
                        run("/usr/bin/sudo".parse()?, sudo_args)?.with_stdin(stdin),
                    ),
                },
                GuestStep {
                    name: GoldenStepName::Verify,
                    action: GuestAction::Run(run(sh, script(VERIFY, &[user.as_str()]))?),
                },
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doppel_desktop::exec::{Completeness, ExecOutput, Termination};

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
                GuestAction::Run(request) => {
                    for argument in request.arguments() {
                        assert!(
                            !argument.contains("s3cret"),
                            "{:?} has the password in an argument",
                            step.name
                        );
                    }
                    assert!(!format!("{request:?}").contains("s3cret"));
                }
                GuestAction::Write(write) => {
                    assert!(!format!("{write:?}").contains("s3cret"));
                    assert!(
                        !write.data().windows(6).any(|w| w == b"s3cret"),
                        "kcpassword is encoded"
                    );
                    assert_eq!(write.mode.bits(), 0o600);
                }
            }
        }
        let GuestAction::Run(admin) = &plan.steps[2].action else {
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
        let failed = RunFailed::judge(&ExecOutput {
            termination: Termination::Exited(1),
            stdout: Vec::new(),
            stderr: b"pmset sleep is not 0\n".to_vec(),
            completeness: Completeness::Full,
        })
        .unwrap_err();
        let error = GoldenError::StepFailed {
            step: plan.steps[3].name,
            failed,
        };
        assert_eq!(
            error.to_string(),
            "checking the settings failed: exited with status 1: pmset sleep is not 0"
        );
    }

    #[test]
    fn scripts_parse_as_sh() {
        for (name, body) in [("ADMIN", ADMIN), ("VERIFY", VERIFY)] {
            let status = std::process::Command::new("/bin/sh")
                .args(["-n", "-c", body])
                .status()
                .unwrap();
            assert!(status.success(), "{name} does not parse");
        }
    }
}
