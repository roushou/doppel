//! What the built `doppel` command line accepts and rejects, by exit
//! status: scripts that run it rely on these. Usage errors exit 2 (clap's),
//! failed commands 1.

#![allow(
    clippy::unwrap_used,
    reason = "integration test helpers: a failed unwrap is a failed test"
)]

use std::process::{Command, Output};

fn doppel(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_doppel"))
        .args(args)
        // Nothing here may touch a real store.
        .env("DOPPEL_HOME", std::env::temp_dir().join("doppel-cli-test"))
        .output()
        .unwrap()
}

fn status(args: &[&str]) -> Option<i32> {
    doppel(args).status.code()
}

#[test]
fn help_and_version_succeed() {
    assert_eq!(status(&["--help"]), Some(0));
    assert_eq!(status(&["--version"]), Some(0));
    assert_eq!(status(&["daemon", "--help"]), Some(0));
}

#[test]
fn version_names_the_program() {
    let out = String::from_utf8(doppel(&["--version"]).stdout).unwrap();
    assert!(out.starts_with("doppel "), "{out:?}");
}

#[test]
fn usage_errors_exit_2() {
    assert_eq!(status(&[]), Some(2));
    assert_eq!(status(&["nosuchcmd"]), Some(2));
    // A missing machine name.
    assert_eq!(status(&["start"]), Some(2));
    // A value that doesn't parse.
    assert_eq!(
        status(&["exec", "dev", "--timeout-ms", "x", "--", "ls"]),
        Some(2)
    );
    assert_eq!(status(&["create", "Not A Name", "--ipsw", "x"]), Some(2));
    // `--machine` and `--from` name the default machine two ways.
    assert_eq!(status(&["mcp", "--machine", "a", "--from", "b"]), Some(2));
}

#[test]
fn host_with_a_this_mac_command_fails_before_doing_anything() {
    let out = doppel(&["--host", "/nonexistent.socket", "checkpoints", "dev"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.starts_with("Error: --host is for commands"), "{err:?}");
}

#[test]
fn doctor_on_an_empty_store_says_not_ready_in_json_and_exits_1() {
    let home = std::env::temp_dir().join(format!("doppel-doctor-{}", std::process::id()));
    let out = Command::new(env!("CARGO_BIN_EXE_doppel"))
        .arg("--home")
        .arg(&home)
        .args(["--vmm", "/nonexistent/doppel-vmm", "doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["ready"], false);
    let statuses: Vec<_> = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| {
            (
                check["check"].as_str().unwrap(),
                check["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        [
            ("vmm", "fail"),
            // Beside the test binary, built or not.
            ("agent-archive", statuses[1].1),
            ("daemon", "fail"),
            ("golden-image", "skipped"),
            ("room", "skipped"),
        ]
    );
    let _ = std::fs::remove_dir_all(home);
}
