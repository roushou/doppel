//! The install plan's guest scripts, run for real on this Mac (the guest is
//! macOS too) against a tiny ad-hoc-signed bundle. The restart script talks
//! to launchd, so it only gets a syntax check here; it is verified live.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test helpers: a failed unwrap is a failed test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use doppel_host::agent_install::plan::{AgentBuild, GuestAccount, InstallPlan, StepAction};
use doppel_machine::exec::{Completeness, ExecOutput, ExecRequest, Termination};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("doppel-unpack-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(program: &str, args: &[&str]) {
    let status = Command::new(program).args(args).status().unwrap();
    assert!(status.success(), "{program} {args:?}: {status}");
}

/// A zipped `Doppel Agent.app` whose build-id says `marker`; `tamper`
/// changes the binary after signing.
fn archive(dir: &Path, marker: &str, tamper: bool) -> Vec<u8> {
    let src = dir.join(format!("src-{marker}"));
    let app = src.join("Doppel Agent.app");
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    fs::create_dir_all(app.join("Contents/Resources")).unwrap();
    fs::copy("/usr/bin/true", app.join("Contents/MacOS/doppel-agent")).unwrap();
    fs::write(app.join("Contents/Resources/build-id"), marker).unwrap();
    fs::write(
        app.join("Contents/Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>dev.doppel.agent</string><key>CFBundleExecutable</key><string>doppel-agent</string></dict></plist>"#,
    )
    .unwrap();
    let app_str = app.to_str().unwrap();
    run(
        "/usr/bin/codesign",
        &[
            "--force",
            "--sign",
            "-",
            "--identifier",
            "dev.doppel.agent",
            app_str,
        ],
    );
    if tamper {
        fs::write(
            app.join("Contents/Resources/build-id"),
            format!("{marker}-tampered"),
        )
        .unwrap();
    }
    let zip = dir.join(format!("{marker}.zip"));
    run(
        "/usr/bin/ditto",
        &["-c", "-k", "--keepParent", app_str, zip.to_str().unwrap()],
    );
    fs::read(zip).unwrap()
}

/// Uploads (as the agent would) and runs the unpack step; returns its exit status.
fn install(home: &Path, build: &str, archive: Vec<u8>) -> std::process::Output {
    let account = GuestAccount::parse(&ExecOutput {
        termination: Termination::Exited(0),
        stdout: format!("501\n{}", home.display()).into_bytes(),
        stderr: Vec::new(),
        completeness: Completeness::Full,
    })
    .unwrap();
    let build: AgentBuild = build.parse().unwrap();
    let plan = InstallPlan::new(archive, build, &account).unwrap();
    let StepAction::Write(upload) = &plan.steps[0].action else {
        panic!("upload first")
    };
    fs::write(upload.path.as_str(), upload.data()).unwrap();
    let StepAction::Run(unpack) = &plan.steps[1].action else {
        panic!("then unpack")
    };
    let output = command(unpack).output().unwrap();
    assert!(
        !Path::new(upload.path.as_str()).exists(),
        "the archive is removed either way"
    );
    output
}

fn command(request: &ExecRequest) -> Command {
    let mut command = Command::new(request.program().as_str());
    command.args(request.arguments());
    command
}

fn leftovers(apps: &Path) -> Vec<String> {
    fs::read_dir(apps)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name != "Doppel Agent.app")
        .collect()
}

#[test]
fn unpack_installs_replaces_and_refuses_a_tampered_bundle() {
    let scratch = Scratch::new("main");
    let home = scratch.0.join("home");
    fs::create_dir_all(&home).unwrap();
    let apps = home.join("Applications");
    let build_id = apps.join("Doppel Agent.app/Contents/Resources/build-id");
    let tag = std::process::id();

    // First install: creates ~/Applications and ~/Library/LaunchAgents.
    let out = install(
        &home,
        &format!("t{tag}-a"),
        archive(&scratch.0, "v1", false),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_to_string(&build_id).unwrap(), "v1");
    assert!(home.join("Library/LaunchAgents").is_dir());
    assert_eq!(leftovers(&apps), Vec::<String>::new());

    // Update: the new bundle replaces the old one.
    let out = install(
        &home,
        &format!("t{tag}-b"),
        archive(&scratch.0, "v2", false),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_to_string(&build_id).unwrap(), "v2");
    assert_eq!(leftovers(&apps), Vec::<String>::new());

    // A bundle changed after signing is refused, and the installed one stays.
    let out = install(&home, &format!("t{tag}-c"), archive(&scratch.0, "v3", true));
    assert!(!out.status.success(), "a tampered bundle was installed");
    assert_eq!(fs::read_to_string(&build_id).unwrap(), "v2");
    assert_eq!(leftovers(&apps), Vec::<String>::new());
}

#[test]
fn the_restart_script_is_valid_sh() {
    let account = GuestAccount::parse(&ExecOutput {
        termination: Termination::Exited(0),
        stdout: b"501\n/Users/x".to_vec(),
        stderr: Vec::new(),
        completeness: Completeness::Full,
    })
    .unwrap();
    let plan = InstallPlan::new(Vec::new(), "b".parse().unwrap(), &account).unwrap();
    let StepAction::Run(restart) = &plan.steps[3].action else {
        panic!("restart last")
    };
    let script = &restart.arguments()[1];
    let status = Command::new("/bin/sh")
        .args(["-n", "-c", script])
        .status()
        .unwrap();
    assert!(status.success());
    // The inner script is a quoted argument; check it on its own too.
    let inner = script.split('\'').nth(1).unwrap();
    let status = Command::new("/bin/sh")
        .args(["-n", "-c", inner])
        .status()
        .unwrap();
    assert!(status.success());
    // And a broken script does fail the check, so it isn't vacuous.
    let status = Command::new("/bin/sh")
        .args(["-n", "-c", "if then"])
        .status()
        .unwrap();
    assert!(!status.success());
}
