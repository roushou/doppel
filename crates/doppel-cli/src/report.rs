//! What a command did, as a value: [`crate::Cli::run`] returns one, and its
//! `Display` is what `doppel` prints on stdout. The wording lives here, in
//! one place, apart from the work. Progress and warnings along the way go
//! to stderr as they happen; a command that serves (the daemon, `mcp`,
//! `view`) reports [`Report::Nothing`] when it ends.

use std::ffi::OsString;
use std::fmt;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use doppel_client::{
    Listing, MachineFailure, Resumed, SessionInfo, SessionStatus, StopOutcome, StopReport,
    StopRequest, TokenEntry,
};
use doppel_desktop::guest_path::GuestPath;
use doppel_desktop::outcome::{Certainty, Failure as _};
use doppel_host::agent_install::Installed;
use doppel_host::guest_user::GuestUserName;
use doppel_host::vmm::RecordingSummary;
use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::lineage::Lineage;
use doppel_machine::login::Desktop;
use doppel_machine::machine::MachineName;
use doppel_machine::screen_probe::HostScreen;
use doppel_machine::vm::{NextStart, RunState, Status};

use crate::doctor::Diagnosis;
use crate::setup::Blocked;
use crate::tokens::{named, remaining};

/// What a command did.
#[derive(Debug)]
#[must_use]
pub enum Report {
    /// Nothing to print: a command that served until stopped, or printed
    /// as it went.
    Nothing,
    /// The program `exec` ran exited with this status, which `doppel`
    /// exits with too. Its output was passed through as it came.
    Exit(i32),
    /// Text another component wrote, printed as it is.
    Verbatim(String),
    /// `doctor`: whether an agent can be given a machine, and what to fix.
    /// Not ready exits 1.
    Doctor {
        diagnosis: Diagnosis,
        form: Form,
    },
    /// `setup`: a golden image to give agents, or why setup can't go on.
    /// Blocked exits 1.
    Setup {
        outcome: SetupOutcome,
        form: Form,
    },
    /// `ls`.
    Machines(Vec<Listing>),
    Created {
        name: MachineName,
        /// `None` until it is started and set up by a person.
        state: Option<RunState>,
        /// The account macOS makes on the first boot, when one was asked for.
        account: Option<GuestUserName>,
    },
    Imported(MachineName),
    Started {
        name: MachineName,
        state: RunState,
        next: NextStart,
    },
    /// An agent-ready desktop.
    Ready {
        name: MachineName,
        desktop: Desktop,
    },
    PasswordSaved {
        name: MachineName,
        path: PathBuf,
    },
    PasswordDeleted(MachineName),
    /// `delete`.
    Deleted(MachineName),
    NoPassword(MachineName),
    RecordingStarted {
        name: MachineName,
        path: PathBuf,
    },
    RecordingStopped {
        name: MachineName,
        summary: RecordingSummary,
    },
    /// A recording's video, with its overlay burned in.
    Exported {
        path: PathBuf,
        /// What the exporter said of it.
        summary: String,
    },
    Status {
        name: MachineName,
        status: Status,
    },
    Stopped {
        name: MachineName,
        report: StopReport,
    },
    Paused(MachineName),
    Resumed {
        name: MachineName,
        resumed: Resumed,
    },
    Checkpointed {
        name: MachineName,
        label: CheckpointLabel,
        memory_bytes: u64,
        /// How long the guest was paused for it, when the daemon said.
        paused_for: Option<Duration>,
        guest: GuestAfter,
    },
    Checkpoints(Vec<Result<CheckpointLabel, OsString>>),
    Forked {
        source: MachineName,
        label: CheckpointLabel,
        into: MachineName,
    },
    Restored {
        name: MachineName,
        label: CheckpointLabel,
    },
    CheckpointForgotten {
        name: MachineName,
        label: CheckpointLabel,
    },
    Screenshot {
        path: PathBuf,
        /// How image pixels map to input coordinates.
        note: String,
    },
    InputSent(MachineName),
    Uploaded {
        machine: MachineName,
        path: GuestPath,
        bytes: usize,
    },
    Downloaded {
        path: PathBuf,
        bytes: usize,
    },
    GoldenPrepared {
        name: MachineName,
        label: CheckpointLabel,
        memory_bytes: u64,
    },
    GoldenMarked {
        name: MachineName,
        label: CheckpointLabel,
        lineage: Lineage,
    },
    GoldenUnmarked(MachineName),
    AgentInstalled {
        name: MachineName,
        /// Where it was started over SSH first, when it wasn't running.
        over_ssh: Option<Ipv4Addr>,
        installed: Installed,
    },
    /// Accessibility and Screen Recording written into a stopped machine.
    AgentGranted {
        name: MachineName,
        user: GuestUserName,
    },
    /// The LaunchAgent's state, in its own words.
    Service(String),
    DaemonUninstalled,
    DaemonStopped(u32),
    DaemonNotRunning,
    WebOff,
    WebListening {
        port: u16,
        fingerprint: String,
        names: Vec<String>,
    },
    Tokens {
        tokens: Vec<TokenEntry>,
        now: SystemTime,
    },
    Revoked(TokenEntry),
    Sessions(Vec<SessionInfo>),
    SessionPaused(u64),
    SessionResumed(u64),
    /// Round trips through a guest vsock port.
    Echoed {
        count: usize,
        p50_ms: f64,
        p99_ms: f64,
    },
}

/// How a completed checkpoint left the guest.
#[derive(Debug)]
pub enum GuestAfter {
    /// As asked; with why its disk wasn't synced first, if it wasn't, and
    /// whether screenshots show it as it is, if it was resumed.
    AsAsked {
        sync_skipped: Option<String>,
        screen: Option<HostScreen>,
    },
    /// It may still be paused, and why.
    LeftPaused(MachineFailure),
}

impl Report {
    /// The status `doppel` exits with when it isn't success.
    #[must_use]
    pub fn exit(&self) -> Option<i32> {
        if let Self::Exit(code) = self {
            return Some(*code);
        }
        if let Self::Doctor { diagnosis, .. } = self
            && !diagnosis.is_ready()
        {
            return Some(1);
        }
        if let Self::Setup {
            outcome: SetupOutcome::Blocked(_),
            ..
        } = self
        {
            return Some(1);
        }
        None
    }
}

/// Where `setup` left this Mac.
#[derive(Debug)]
pub enum SetupOutcome {
    /// This golden image is ready to fork, from this template.
    Ready {
        golden: MachineName,
        template: CheckpointLabel,
    },
    Blocked(Blocked),
}

impl SetupOutcome {
    /// `--json`'s document: `ready`, and the golden image and the command
    /// that serves an agent from it, or what blocked setup.
    fn json(&self) -> serde_json::Value {
        match self {
            Self::Ready { golden, template } => serde_json::json!({
                "ready": true,
                "golden_image": golden.as_str(),
                "template": template.as_str(),
                "mcp_command": ["doppel", "mcp", "--from", golden.as_str()],
            }),
            Self::Blocked(blocked) => serde_json::json!({
                "ready": false,
                "blocked": blocked.to_string(),
            }),
        }
    }
}

impl fmt::Display for SetupOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready { golden, template } => {
                writeln!(f, "{golden}: ready, a golden image (template '{template}')")?;
                writeln!(
                    f,
                    "Give an agent a machine of its own, a fresh fork per session:"
                )?;
                writeln!(f, "  doppel mcp --from {golden}")?;
                writeln!(f, "For Claude Code:")?;
                writeln!(f, "  claude mcp add doppel -- doppel mcp --from {golden}")
            }
            Self::Blocked(blocked) => writeln!(f, "setup can't go on: {blocked}"),
        }
    }
}

/// How a report that a program may read is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Text,
    /// One JSON document, for programs.
    Json,
}

impl fmt::Display for Report {
    #[expect(clippy::too_many_lines, reason = "one arm per kind of outcome")]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nothing | Self::Exit(_) => Ok(()),
            Self::Verbatim(text) => f.write_str(text),
            Self::Doctor {
                diagnosis,
                form: Form::Text,
            } => diagnosis.fmt(f),
            Self::Doctor {
                diagnosis,
                form: Form::Json,
            } => writeln!(f, "{:#}", diagnosis.json()),
            Self::Setup {
                outcome,
                form: Form::Text,
            } => outcome.fmt(f),
            Self::Setup {
                outcome,
                form: Form::Json,
            } => writeln!(f, "{:#}", outcome.json()),
            Self::Machines(listings) => {
                for listing in listings {
                    match listing {
                        Listing::Machine {
                            name,
                            status,
                            lineage,
                        } => {
                            let status = status.as_ref().map_or_else(
                                |failure| format!("error: {failure}"),
                                ToString::to_string,
                            );
                            // Nothing for a working machine of unrecorded origin.
                            let lineage = lineage.to_string();
                            if lineage.is_empty() {
                                writeln!(f, "{name:<32} {status}")?;
                            } else {
                                writeln!(f, "{name:<32} {status:<24} {lineage}")?;
                            }
                        }
                        Listing::Unrecognized(raw) => writeln!(
                            f,
                            "{:<32} (ignored: not a valid machine name)",
                            raw.to_string_lossy()
                        )?,
                    }
                }
                Ok(())
            }
            Self::Created {
                name, state: None, ..
            } => writeln!(f, "created {name}: start it and go through Setup Assistant"),
            Self::Created {
                name,
                state: Some(state),
                account: Some(user),
            } => writeln!(
                f,
                "created {name}: {state}, first boot creating '{}' (password stored: doppel login uses it). \
                 When its desktop shows, Remote Login is on",
                user.as_str()
            ),
            Self::Created {
                name,
                state: Some(state),
                account: None,
            } => writeln!(f, "created {name}: {state}"),
            Self::Imported(name) => writeln!(f, "imported {name}"),
            Self::Started { name, state, next } => {
                let how = match next {
                    NextStart::ColdBoot => "booted",
                    NextStart::ResumeSavedState => "resumed from saved state",
                };
                writeln!(f, "{name}: {state} ({how})")
            }
            Self::Ready { name, desktop } => writeln!(f, "{name}: ready ({desktop})"),
            Self::PasswordSaved { name, path } => {
                writeln!(f, "{name}: saved to {} (mode 600)", path.display())
            }
            Self::PasswordDeleted(name) => writeln!(f, "{name}: stored password deleted"),
            Self::Deleted(name) => writeln!(f, "{name}: deleted"),
            Self::NoPassword(name) => writeln!(f, "{name}: no stored password"),
            Self::RecordingStarted { name, path } => {
                writeln!(f, "{name}: recording to {}", path.display())
            }
            Self::Exported { path, summary } => {
                writeln!(f, "{}: {summary}", path.display())
            }
            Self::RecordingStopped { name, summary } => {
                #[expect(clippy::cast_precision_loss, reason = "megabytes, to one place")]
                let megabytes = summary.bytes as f64 / 1_000_000.0;
                writeln!(
                    f,
                    "{name}: recorded {} ({} frames, {:.1} s, {megabytes:.1} MB)",
                    summary.path.display(),
                    summary.frames,
                    summary.seconds,
                )
            }
            Self::Status { name, status } => writeln!(f, "{name}: {status}"),
            Self::Stopped { name, report } => stopped(f, name, report),
            Self::Paused(name) => writeln!(f, "{name}: paused"),
            Self::Resumed {
                name,
                resumed: Resumed::Resumed,
            } => writeln!(f, "{name}: running"),
            Self::Resumed {
                name,
                resumed: Resumed::AlreadyRunning,
            } => writeln!(f, "{name}: running (it already was)"),
            Self::Checkpointed {
                name,
                label,
                memory_bytes,
                paused_for,
                guest:
                    GuestAfter::AsAsked {
                        sync_skipped,
                        screen,
                    },
            } => {
                let paused = paused_for.map_or_else(String::new, |paused| {
                    format!(", guest paused {:.1}s", paused.as_secs_f64())
                });
                let screen = screen
                    .as_ref()
                    .map_or_else(String::new, |screen| format!("; {screen}"));
                writeln!(
                    f,
                    "checkpoint {name}/{label}: {} MiB memory state{paused}{}{screen}",
                    memory_bytes >> 20,
                    doppel_client::CheckpointResult::sync_caveat(sync_skipped.as_deref())
                )
            }
            Self::Checkpointed {
                name,
                label,
                memory_bytes,
                guest: GuestAfter::LeftPaused(why),
                ..
            } => {
                let left = match why.certainty() {
                    Certainty::NotDone => "guest left paused",
                    Certainty::MaybeDone => "guest may be left paused",
                };
                writeln!(
                    f,
                    "checkpoint {name}/{label}: {} MiB memory state; WARNING: {left}: {why}",
                    memory_bytes >> 20
                )
            }
            Self::Checkpoints(entries) => {
                for entry in entries {
                    match entry {
                        Ok(label) => writeln!(f, "{}", label.as_str())?,
                        Err(raw) => {
                            writeln!(f, "{} (ignored: not a valid label)", raw.to_string_lossy())?
                        }
                    }
                }
                Ok(())
            }
            Self::Forked {
                source,
                label,
                into,
            } => writeln!(
                f,
                "forked {source}/{label} → {into} (starts where the checkpoint left off; shares {source}'s MAC and identity)"
            ),
            Self::Restored { name, label } => {
                writeln!(f, "{name} restored to {label}; next start resumes there")
            }
            Self::CheckpointForgotten { name, label } => {
                writeln!(f, "{name}: checkpoint {label} deleted")
            }
            Self::Screenshot { path, note } => writeln!(f, "{}: {note}", path.display()),
            Self::InputSent(name) => writeln!(f, "{name}: sent"),
            Self::Uploaded {
                machine,
                path,
                bytes,
            } => writeln!(f, "{machine}:{path} ({bytes} bytes)"),
            Self::Downloaded { path, bytes } => {
                writeln!(f, "{} ({bytes} bytes)", path.display())
            }
            Self::GoldenPrepared {
                name,
                label,
                memory_bytes,
            } => writeln!(
                f,
                "{name}: golden checkpoint '{label}' ({} MiB of memory), recorded as its template. Fork it: doppel fork {name} {label} <name>",
                memory_bytes >> 20
            ),
            Self::GoldenMarked {
                name,
                label,
                lineage,
            } => writeln!(
                f,
                "{name}: {lineage}. Fork it: doppel fork {name} {label} <name>"
            ),
            Self::GoldenUnmarked(name) => writeln!(f, "{name}: a working machine again"),
            Self::AgentInstalled {
                name,
                over_ssh,
                installed,
            } => {
                if let Some(address) = over_ssh {
                    writeln!(f, "{name}: started the agent over SSH ({address})")?;
                }
                writeln!(
                    f,
                    "{name}: agent build {} answered {} ms after the restart",
                    installed.build,
                    installed.restart.as_millis()
                )
            }
            Self::AgentGranted { name, user } => writeln!(
                f,
                "{name}: the agent has Accessibility and Screen Recording for {}; they apply from its next start",
                user.as_str()
            ),
            Self::Service(status) => writeln!(f, "{status}"),
            Self::DaemonUninstalled => writeln!(f, "doppeld: uninstalled"),
            Self::DaemonStopped(pid) => writeln!(f, "doppeld: stopped (pid {pid})"),
            Self::DaemonNotRunning => writeln!(f, "doppeld: not running"),
            Self::WebOff => writeln!(f, "doppeld: web: off"),
            Self::WebListening {
                port,
                fingerprint,
                names,
            } => {
                writeln!(f, "doppeld: web: listening on port {port}")?;
                for name in names {
                    let host = if name.contains(':') {
                        format!("[{name}]")
                    } else {
                        name.clone()
                    };
                    writeln!(f, "  https://{host}:{port}/")?;
                }
                writeln!(f, "certificate SHA-256 {fingerprint}")
            }
            Self::Tokens { tokens, now } => {
                if tokens.is_empty() {
                    writeln!(f, "no tokens")?;
                }
                for token in tokens {
                    writeln!(
                        f,
                        "{}{}: expires in {}",
                        token.id,
                        named(token.label.as_ref()),
                        remaining(token.expires, *now)
                    )?;
                }
                Ok(())
            }
            Self::Revoked(token) => writeln!(
                f,
                "token {}{}: revoked",
                token.id,
                named(token.label.as_ref())
            ),
            Self::Sessions(sessions) => {
                if sessions.is_empty() {
                    writeln!(f, "no sessions")?;
                }
                for session in sessions {
                    session_line(f, session)?;
                }
                Ok(())
            }
            Self::SessionPaused(id) => writeln!(f, "session {id}: paused"),
            Self::SessionResumed(id) => writeln!(f, "session {id}: resumed"),
            Self::Echoed {
                count,
                p50_ms,
                p99_ms,
            } => writeln!(
                f,
                "vsock echo via handed-over fd: n={count} p50={p50_ms:.3}ms p99={p99_ms:.3}ms"
            ),
        }
    }
}

fn stopped(f: &mut fmt::Formatter<'_>, name: &MachineName, report: &StopReport) -> fmt::Result {
    let how = match &report.request {
        StopRequest::Forced => "powered off".to_owned(),
        StopRequest::Session => "shut down from the logged-in session".to_owned(),
        StopRequest::SessionAfterUnlock { unlocked_after } => format!(
            "shut down from the logged-in session, after unlocking its screen with the stored password in {} ms",
            unlocked_after.as_millis()
        ),
        StopRequest::PowerButton { session: why } => {
            format!("power button pressed (the session couldn't be asked: {why})")
        }
    };
    let why_pending = match &report.request {
        StopRequest::Forced => "",
        StopRequest::Session | StopRequest::SessionAfterUnlock { .. } => {
            " Something in the guest may be holding it up: check a screenshot."
        }
        StopRequest::PowerButton { .. } => {
            " The login window ignores the power button, and a logged-in desktop asks to confirm it."
        }
    };
    match report.outcome {
        StopOutcome::Stopped => writeln!(f, "{name}: stopped ({how})"),
        StopOutcome::ShutdownPending => writeln!(
            f,
            "{name}: {how}; the guest hasn't finished shutting down.{why_pending} `stop --force` powers it off."
        ),
    }
}

fn session_line(f: &mut fmt::Formatter<'_>, session: &SessionInfo) -> fmt::Result {
    let machines = session
        .machines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let state = match &session.state {
        SessionStatus::Active => String::new(),
        state @ (SessionStatus::Paused | SessionStatus::Unknown(_)) => format!(" ({state})"),
    };
    writeln!(
        f,
        "{:<4} {:<32} {:<24} {} answered{state}{}",
        session.id,
        session.client,
        if machines.is_empty() {
            "-".into()
        } else {
            machines
        },
        session.answered,
        if session.running.is_empty() {
            String::new()
        } else {
            format!("; now: {}", session.running.join(", "))
        }
    )
}
