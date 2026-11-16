//! The built `doppel` and its daemon: machine commands and `doppel mcp` go
//! through `doppel daemon run`, and print what they printed when they ran in
//! the command's own process (the snapshots were taken then). Without a
//! daemon, they say how to start one. The cases need no VM (a stopped
//! machine, a missing checkpoint, a missing VMM binary).

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test helpers: a failed unwrap is a failed test"
)]

use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        // Short: the runtime directory holds unix socket paths.
        let dir = std::env::temp_dir().join(format!("dr-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn doppel(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_doppel"));
        command
            .arg("--home")
            .arg(self.0.join("data"))
            .arg("--runtime-dir")
            .arg(self.0.join("run"))
            .arg("--vmm")
            .arg(self.0.join("no-vmm"));
        command
    }

    /// A machine whose spec is the golden fixture: valid, never started.
    fn machine(&self, name: &str) {
        let dir = self.0.join("data/machines").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bundle/spec.v1.bin"),
            dir.join("spec.pb"),
        )
        .unwrap();
    }

    /// Each command's exit status and output, the scratch path redacted.
    fn transcript(&self, commands: &[&[&str]]) -> String {
        let mut transcript = String::new();
        for args in commands {
            let output = self.doppel().args(*args).output().unwrap();
            let _ = writeln!(transcript, "$ doppel {}", args.join(" "));
            let _ = writeln!(transcript, "exit: {}", output.status.code().unwrap_or(-1));
            for (stream, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
                let text = String::from_utf8_lossy(bytes)
                    .replace(&self.0.display().to_string(), "[scratch]");
                if !text.is_empty() {
                    let _ = write!(transcript, "{stream}:\n{text}");
                }
            }
            transcript.push('\n');
        }
        transcript
    }

    /// Tool calls through `doppel mcp --machine dev`, one at a time, and
    /// each result: whether it is an error, and its text. `meanwhile` runs
    /// while the session is open.
    fn mcp_transcript(
        &self,
        calls: &[(&str, serde_json::Value)],
        meanwhile: impl FnOnce(),
    ) -> String {
        let mut mcp = self
            .doppel()
            .args(["mcp", "--machine", "dev"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = mcp.stdin.take().unwrap();
        let mut stdout = BufReader::new(mcp.stdout.take().unwrap());
        let mut send = |message: serde_json::Value| {
            writeln!(stdin, "{message}").unwrap();
        };
        let mut receive = || {
            let mut line = String::new();
            stdout.read_line(&mut line).unwrap();
            serde_json::from_str::<serde_json::Value>(&line).unwrap()
        };
        send(
            serde_json::json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"routes test","version":"0"}}}),
        );
        receive();
        send(serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        let mut transcript = String::new();
        for (id, (name, arguments)) in calls.iter().enumerate() {
            send(
                serde_json::json!({"jsonrpc":"2.0","id":id + 1,"method":"tools/call",
                "params":{"name":name,"arguments":arguments}}),
            );
            let reply = receive();
            let result = &reply["result"];
            let _ = writeln!(transcript, "> {name} {arguments}");
            let _ = writeln!(
                transcript,
                "{}{}\n",
                if result["isError"] == true {
                    "error: "
                } else {
                    ""
                },
                result["content"][0]["text"].as_str().unwrap_or("?")
            );
        }
        meanwhile();
        drop(send);
        drop(stdin);
        assert!(mcp.wait().unwrap().success());
        transcript.replace(&self.0.display().to_string(), "[scratch]")
    }

    /// `doppel daemon run` serving `home`'s machines on this scratch's
    /// runtime directory, once it listens.
    fn daemon(&self, home: &Path) -> Daemon {
        let daemon = Daemon(
            Command::new(env!("CARGO_BIN_EXE_doppel"))
                .arg("--home")
                .arg(home)
                .arg("--runtime-dir")
                .arg(self.0.join("run"))
                .arg("--vmm")
                .arg(self.0.join("no-vmm"))
                .args(["daemon", "run"])
                .spawn()
                .unwrap(),
        );
        let socket = home.join("doppeld.socket");
        for _ in 0..200 {
            if UnixStream::connect(&socket).is_ok() {
                return daemon;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the daemon never listened on {}", socket.display());
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The running `doppel daemon run`, killed when dropped.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const COMMANDS: &[&[&str]] = &[
    &["ls"],
    &["status", "dev"],
    &["status", "nope"],
    &["start", "dev", "--no-wait"],
    &["login", "dev"],
    &["pause", "dev"],
    &["resume", "dev"],
    &["stop", "dev"],
    &["stop", "dev", "--force"],
    &["checkpoint", "dev", "clean"],
    &["restore", "dev", "clean", "--discard-current"],
    &["fork", "dev", "clean", "copy"],
    &["ls"],
];

#[test]
fn machine_commands_print_the_same_through_the_daemon() {
    let scratch = Scratch::new("same");
    scratch.machine("dev");
    let _daemon = scratch.daemon(&scratch.0.join("data"));
    insta::assert_snapshot!(scratch.transcript(COMMANDS));
}

/// Too many forks, none, or a checkpoint that isn't there: each run fails
/// without running its command or leaving a fork behind.
#[test]
fn fork_run_refuses_what_it_cant_run_before_running_anything() {
    let scratch = Scratch::new("forkrun");
    scratch.machine("dev");
    let _daemon = scratch.daemon(&scratch.0.join("data"));
    let ran = scratch.0.join("ran");
    for count in ["3", "0", "1"] {
        let status = scratch
            .doppel()
            .args(["fork-run", "dev", "clean", "--count", count, "--", "touch"])
            .arg(&ran)
            .output()
            .unwrap()
            .status;
        assert!(!status.success(), "--count {count} ran");
        assert!(!ran.exists(), "--count {count} ran its command");
    }
    let machines = fs::read_dir(scratch.0.join("data/machines"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| !name.to_string_lossy().starts_with('.'))
        .collect::<Vec<_>>();
    assert_eq!(machines, ["dev"]);
}

/// A token goes to stdout once and is known by its id after; revoked, it's
/// no longer listed. A lifetime out of range is refused before the daemon
/// is asked.
#[test]
fn tokens_are_issued_listed_and_revoked() {
    let scratch = Scratch::new("tokens");
    let _daemon = scratch.daemon(&scratch.0.join("data"));
    let run = |args: &[&str]| {
        let output = scratch.doppel().args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let printed = run(&["daemon", "token", "--label", "phone", "--ttl", "2h"]);
    let token: doppel_gateway::TokenSecret = printed.trim_end().parse().unwrap();
    let id = token.digest().id().to_string();
    assert!(run(&["daemon", "tokens"]).contains(&id));
    run(&["daemon", "revoke", &id]);
    assert!(!run(&["daemon", "tokens"]).contains(&id));

    let refused = scratch
        .doppel()
        .args(["daemon", "token", "--ttl", "31d"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
}

/// `web on` listens on the port asked and `web off` stops; a port that
/// isn't one is refused.
#[test]
fn web_turns_the_listener_on_and_off() {
    let scratch = Scratch::new("web");
    let _daemon = scratch.daemon(&scratch.0.join("data"));
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
        .to_string();
    let run = |args: &[&str]| scratch.doppel().args(args).output().unwrap();
    assert!(
        run(&["daemon", "web", "on", "--port", &port])
            .status
            .success()
    );
    assert!(run(&["daemon", "web", "status"]).status.success());
    assert!(std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok());
    assert!(scratch.0.join("data/tls/cert.pem").exists());
    assert!(run(&["daemon", "web", "off"]).status.success());
    assert!(std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_err());
    assert!(
        !run(&["daemon", "web", "on", "--port", "0"])
            .status
            .success()
    );
}

/// `fork-run` hands each command its machine and recording through the
/// environment, as `doppel mcp` reads them.
#[test]
fn mcp_takes_its_machine_and_recording_from_the_environment() {
    let scratch = Scratch::new("mcpenv");
    scratch.machine("dev");
    let _daemon = scratch.daemon(&scratch.0.join("data"));
    let mut child = scratch
        .doppel()
        .arg("mcp")
        .env("DOPPEL_MACHINE", "dev")
        .env("DOPPEL_RECORD", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The session opens when the client says hello; its stdin then closes.
    let hello = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"env test","version":"0"}}}"#;
    {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{hello}").unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("session 1, recorded into"), "{stderr}");
}

/// Each store has its own daemon socket, so a daemon given another
/// `--home` is never reached; with none for this store, commands fail and
/// say how to start one.
#[test]
fn without_a_daemon_commands_say_how_to_start_one() {
    let scratch = Scratch::new("none");
    scratch.machine("dev");
    let _daemon = scratch.daemon(&scratch.0.join("other"));
    for args in [&["ls"][..], &["status", "dev"], &["sessions"]] {
        let output = scratch.doppel().args(args).output().unwrap();
        assert!(!output.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("no daemon is running"), "{stderr}");
        assert!(stderr.contains("doppel daemon install"), "{stderr}");
    }
}

fn mcp_calls() -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;
    vec![
        ("status", json!({})),
        ("screenshot", json!({})),
        ("find_text", json!({ "text": "Save" })),
        ("key", json!({ "chord": "cmd+n" })),
        ("click", json!({ "x": 10, "y": 10, "via": "host" })),
        ("snapshot", json!({})),
        ("act", json!({ "ref": "s1e1", "action": "press" })),
        (
            "wait_for",
            json!({ "until": "appears", "text": "Save", "timeout_ms": 500 }),
        ),
        (
            "wait_for",
            json!({ "until": "text_appears", "text": "Save", "timeout_ms": 500 }),
        ),
        ("exec", json!({ "program": "/bin/echo", "args": ["hi"] })),
        ("read_file", json!({ "path": "/tmp/x" })),
        ("write_file", json!({ "path": "/tmp/x", "text": "x" })),
        ("checkpoint", json!({ "label": "clean" })),
        ("rollback", json!({ "checkpoint": "clean" })),
        ("fork", json!({ "checkpoint": "clean", "into": "copy" })),
        ("stop", json!({})),
        ("start", json!({})),
        ("status", json!({ "machine": "nope" })),
    ]
}

#[test]
fn mcp_calls_answer_the_same_through_the_daemon() {
    let scratch = Scratch::new("mcp");
    scratch.machine("dev");
    let _daemon = scratch.daemon(&scratch.0.join("data"));
    let mut sessions = String::new();
    let through = scratch.mcp_transcript(&mcp_calls(), || {
        sessions = scratch.transcript(&[&["sessions"]]);
    });
    insta::assert_snapshot!(through);
    // The session, while open: its machines and how many calls it made
    // (the stop and start before it failed; the act's ref was refused before
    // it reached the daemon).
    // Named as the MCP client named itself.
    assert!(sessions.contains("routes test"), "{sessions}");
    assert!(sessions.contains("dev, nope"), "{sessions}");
    assert!(sessions.contains(" answered"), "{sessions}");
}

/// `--host` reaches a daemon on any socket: here one serving another store,
/// as an `ssh -L` forward of another Mac's would. Commands that work on
/// this Mac's store refuse it, and a socket nothing listens on says to
/// forward one.
#[test]
fn host_points_daemon_commands_at_another_daemon() {
    let scratch = Scratch::new("host");
    let other = scratch.0.join("other");
    fs::create_dir_all(other.join("machines/far")).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/bundle/spec.v1.bin"),
        other.join("machines/far/spec.pb"),
    )
    .unwrap();
    scratch.machine("near");
    let _daemon = scratch.daemon(&other);
    let socket = other.join("doppeld.socket");

    let ls = scratch
        .doppel()
        .arg("--host")
        .arg(&socket)
        .arg("ls")
        .output()
        .unwrap();
    assert!(ls.status.success(), "{ls:?}");
    let listed = String::from_utf8_lossy(&ls.stdout);
    assert!(listed.contains("far"), "{listed}");
    assert!(!listed.contains("near"), "{listed}");

    let local = scratch
        .doppel()
        .args(["checkpoints", "near"])
        .arg("--host")
        .arg(&socket)
        .output()
        .unwrap();
    assert!(!local.status.success());
    let stderr = String::from_utf8_lossy(&local.stderr);
    assert!(
        stderr.contains("--host is for commands that go through a daemon"),
        "{stderr}"
    );

    let nowhere = scratch
        .doppel()
        .arg("--host")
        .arg(scratch.0.join("nothing.socket"))
        .arg("ls")
        .output()
        .unwrap();
    assert!(!nowhere.status.success());
    let stderr = String::from_utf8_lossy(&nowhere.stderr);
    assert!(stderr.contains("ssh -N -L"), "{stderr}");
}
