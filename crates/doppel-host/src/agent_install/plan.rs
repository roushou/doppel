//! Installing `Doppel Agent.app` in a guest as a LaunchAgent, through a
//! running agent's exec and file operations.
//!
//! The plan is data: the shell runs its steps in order and then waits for an
//! agent whose `Hello` names the new build. The app lives in
//! `~/Applications`, launchd starts it in the GUI session and restarts it if it
//! exits (`KeepAlive`). Accessibility and Screen Recording grants belong to the
//! app's code identity: with ad-hoc signing every new build needs them again.

use std::fmt;
use std::time::Duration;

use doppel_desktop::exec::{ExecError, ExecOutput, ExecRequest, Termination};
use doppel_desktop::files::{FileError, FileMode, WritePolicy, WriteRequest};
use doppel_desktop::guest_path::{GuestPath, GuestPathError};

use crate::guest_step::plan::{GuestAction, GuestStep, RunFailed};

/// The LaunchAgent's label, also the app's bundle identifier.
pub const LABEL: &str = "dev.doppel.agent";
const APP: &str = "Doppel Agent.app";
const SH: &str = "/bin/sh";

pub use doppel_desktop::agent::{AgentBuild, AgentBuildError};

/// The guest user the agent runs as: whose GUI domain and home it installs into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestAccount {
    uid: u32,
    home: GuestPath,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    #[error("couldn't read the guest account: {0}")]
    Probe(String),
    #[error("guest home: {0}")]
    Home(#[from] GuestPathError),
    #[error(transparent)]
    File(#[from] FileError),
    #[error(transparent)]
    Exec(#[from] ExecError),
    #[error("{step} failed: {failed}")]
    StepFailed { step: StepName, failed: RunFailed },
}

impl GuestAccount {
    /// Prints the uid, a newline, then `$HOME`.
    ///
    /// # Errors
    /// Never in practice: the request is fixed and within limits.
    pub fn probe() -> Result<ExecRequest, InstallError> {
        Ok(ExecRequest::new(
            SH.parse()?,
            vec!["-c".into(), r#"id -u && printf %s "$HOME""#.into()],
            Duration::from_secs(10),
            4096,
        )?)
    }

    /// # Errors
    /// [`InstallError::Probe`] unless the probe exited 0 with a uid and an absolute home.
    pub fn parse(output: &ExecOutput) -> Result<Self, InstallError> {
        let probe = |why: &str| InstallError::Probe(why.to_owned());
        if output.termination != Termination::Exited(0) {
            return Err(InstallError::Probe(output.termination.to_string()));
        }
        let text = std::str::from_utf8(&output.stdout).map_err(|_| probe("not UTF-8"))?;
        let (uid, home) = text.split_once('\n').ok_or_else(|| probe("no home line"))?;
        Ok(Self {
            uid: uid
                .trim()
                .parse()
                .map_err(|_| probe("uid is not a number"))?,
            home: home.parse()?,
        })
    }
}

/// What a step is, for progress and errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepName {
    UploadArchive,
    Unpack,
    WriteLaunchAgent,
    Restart,
}

impl fmt::Display for StepName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UploadArchive => "uploading the app archive",
            Self::Unpack => "unpacking the app",
            Self::WriteLaunchAgent => "writing the LaunchAgent",
            Self::Restart => "scheduling the restart",
        })
    }
}

/// The steps that install one build, and the build to wait for afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct InstallPlan {
    pub steps: Vec<GuestStep<StepName>>,
    pub build: AgentBuild,
    /// Where the app ends up.
    pub app: GuestPath,
}

/// Unpacks into a fresh staging directory beside the app, checks the
/// signature, then swaps it in. `$1` archive, `$2` ~/Applications, `$3` ~/Library/LaunchAgents.
const UNPACK: &str = r#"set -eu
zip=$1 apps=$2 agents=$3 app="Doppel Agent.app"
mkdir -p "$apps" "$agents"
staging=$(mktemp -d "$apps/.doppel-agent.XXXXXX")
trap 'rm -rf "$staging"; rm -f "$zip"' EXIT
/usr/bin/ditto -x -k "$zip" "$staging"
test -x "$staging/$app/Contents/MacOS/doppel-agent"
/usr/bin/codesign --verify --strict "$staging/$app"
if [ -e "$apps/$app" ]; then mv "$apps/$app" "$staging/previous.app"; fi
mv "$staging/$app" "$apps/$app""#;

/// Returns at once; a detached shell restarts the job a second later, so the
/// agent running this can reply first. That agent is stopped too when it
/// isn't the job itself (the first install, from Terminal), but only if the
/// pid still names a doppel-agent. `$1` uid, `$2` plist.
const RESTART: &str = r#"set -eu
nohup /bin/sh -c '
  sleep 1
  launchctl bootout "gui/$1/dev.doppel.agent" 2>/dev/null || true
  case "$(ps -p "$3" -o comm= 2>/dev/null)" in *doppel-agent*) kill "$3" ;; esac
  sleep 1
  launchctl bootstrap "gui/$1" "$2"
' restart "$1" "$2" "$PPID" </dev/null >/dev/null 2>&1 &"#;

impl InstallPlan {
    /// # Errors
    /// [`InstallError`] for an archive over the file limit or an unusable home.
    pub fn new(
        archive: Vec<u8>,
        build: AgentBuild,
        account: &GuestAccount,
    ) -> Result<Self, InstallError> {
        let home = &account.home;
        let zip: GuestPath = format!("/tmp/{LABEL}-{build}.zip").parse()?;
        let apps = home.join("Applications")?;
        let agents = home.join("Library/LaunchAgents")?;
        let plist = agents.join(&format!("{LABEL}.plist"))?;
        let app = apps.join(APP)?;
        let program = app.join("Contents/MacOS/doppel-agent")?;
        let log = home.join("Library/Logs/doppel-agent.log")?;
        let sh: GuestPath = SH.parse()?;
        let script = |body: &str, args: &[&str]| {
            let mut all = vec!["-c".to_owned(), body.to_owned(), "install".to_owned()];
            all.extend(args.iter().map(|a| (*a).to_owned()));
            ExecRequest::new(sh.clone(), all, Duration::from_secs(60), 64 * 1024)
        };
        let steps = vec![
            GuestStep {
                name: StepName::UploadArchive,
                action: GuestAction::Write(WriteRequest::new(
                    zip.clone(),
                    archive,
                    FileMode::new(0o600)?,
                    WritePolicy::Replace,
                )?),
            },
            GuestStep {
                name: StepName::Unpack,
                action: GuestAction::Run(script(
                    UNPACK,
                    &[zip.as_str(), apps.as_str(), agents.as_str()],
                )?),
            },
            GuestStep {
                name: StepName::WriteLaunchAgent,
                action: GuestAction::Write(WriteRequest::new(
                    plist.clone(),
                    LaunchAgent {
                        program: &program,
                        log: &log,
                    }
                    .render()
                    .into_bytes(),
                    FileMode::DEFAULT,
                    WritePolicy::Replace,
                )?),
            },
            GuestStep {
                name: StepName::Restart,
                action: GuestAction::Run(script(
                    RESTART,
                    &[&account.uid.to_string(), plist.as_str()],
                )?),
            },
        ];
        Ok(Self { steps, build, app })
    }
}

/// The agent's LaunchAgent: the program launchd runs, and where its
/// output goes.
struct LaunchAgent<'a> {
    program: &'a GuestPath,
    log: &'a GuestPath,
}

impl LaunchAgent<'_> {
    /// Its property list.
    fn render(&self) -> String {
        let program = Self::escape(self.program.as_str());
        let log = Self::escape(self.log.as_str());
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{program}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
  <key>ProcessType</key><string>Interactive</string>
  <!-- Programs the agent runs, and the installer's restart, outlive the agent. -->
  <key>AbandonProcessGroup</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#
        )
    }

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doppel_desktop::exec::Completeness;

    fn output(termination: Termination, stdout: &str, stderr: &str) -> ExecOutput {
        ExecOutput {
            termination,
            stdout: stdout.into(),
            stderr: stderr.into(),
            completeness: Completeness::Full,
        }
    }

    fn account() -> GuestAccount {
        GuestAccount::parse(&output(Termination::Exited(0), "501\n/Users/a & <b>", "")).unwrap()
    }

    #[test]
    fn the_probe_needs_a_uid_and_an_absolute_home() {
        assert_eq!(account().uid, 501);
        assert_eq!(account().home.as_str(), "/Users/a & <b>");
        for bad in [
            output(Termination::Exited(1), "501\n/Users/a", ""),
            output(Termination::Exited(0), "501", ""),
            output(Termination::Exited(0), "x\n/Users/a", ""),
            output(Termination::Exited(0), "501\nUsers/a", ""),
            output(Termination::TimedOut, "", ""),
        ] {
            assert!(GuestAccount::parse(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_plan_uploads_unpacks_writes_the_job_and_restarts() {
        let build: AgentBuild = "b1".parse().unwrap();
        let plan = InstallPlan::new(b"zip".to_vec(), build.clone(), &account()).unwrap();
        let names: Vec<_> = plan.steps.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                StepName::UploadArchive,
                StepName::Unpack,
                StepName::WriteLaunchAgent,
                StepName::Restart
            ]
        );
        assert_eq!(plan.build, build);
        assert_eq!(
            plan.app.as_str(),
            "/Users/a & <b>/Applications/Doppel Agent.app"
        );

        let GuestAction::Write(upload) = &plan.steps[0].action else {
            panic!("upload is a write")
        };
        assert_eq!(upload.path.as_str(), "/tmp/dev.doppel.agent-b1.zip");
        assert_eq!(upload.data(), b"zip");

        // Paths reach the scripts as arguments, never spliced into the script text.
        let GuestAction::Run(unpack) = &plan.steps[1].action else {
            panic!("unpack runs")
        };
        assert_eq!(unpack.program().as_str(), "/bin/sh");
        assert_eq!(unpack.arguments()[1], UNPACK);
        assert_eq!(
            &unpack.arguments()[3..],
            [
                "/tmp/dev.doppel.agent-b1.zip",
                "/Users/a & <b>/Applications",
                "/Users/a & <b>/Library/LaunchAgents"
            ]
        );

        let GuestAction::Write(job) = &plan.steps[2].action else {
            panic!("job is a write")
        };
        assert_eq!(
            job.path.as_str(),
            "/Users/a & <b>/Library/LaunchAgents/dev.doppel.agent.plist"
        );
        assert_eq!(job.policy, WritePolicy::Replace);
        let plist = std::str::from_utf8(job.data()).unwrap();
        assert!(plist.contains("<string>/Users/a &amp; &lt;b&gt;/Applications/Doppel Agent.app/Contents/MacOS/doppel-agent</string>"), "{plist}");
        assert!(!plist.contains("a & <b>"), "unescaped path in {plist}");
        assert!(plist.contains("<key>KeepAlive</key><true/>"));

        let GuestAction::Run(restart) = &plan.steps[3].action else {
            panic!("restart runs")
        };
        assert_eq!(&restart.arguments()[3..], ["501", job.path.as_str()]);
    }

    #[test]
    fn a_failed_step_names_itself_and_its_stderr() {
        let plan = InstallPlan::new(Vec::new(), "b".parse().unwrap(), &account()).unwrap();
        let step = plan.steps[1].name;
        let failed = |termination, stderr| InstallError::StepFailed {
            step,
            failed: RunFailed::judge(&output(termination, "", stderr)).unwrap_err(),
        };
        let error = failed(Termination::Exited(1), "codesign: invalid signature\n");
        assert_eq!(
            error.to_string(),
            "unpacking the app failed: exited with status 1: codesign: invalid signature"
        );
        let error = failed(Termination::Signaled(9), "");
        assert_eq!(
            error.to_string(),
            "unpacking the app failed: killed by signal 9"
        );
    }
}
