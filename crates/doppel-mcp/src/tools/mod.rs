//! The tools Doppel exposes: definitions, typed calls parsed from arguments,
//! and rendering of outcomes into MCP results.

use std::fmt::Write as _;
use std::time::Duration;

use base64::Engine as _;
use doppel_desktop::agent::AgentReport;
use doppel_desktop::ax::compact::CompactionPolicy;
use doppel_desktop::ax::refs::{Ref, RefTarget};
use doppel_desktop::ax::tree::{ActOn, AxSnapshot, SnapshotSpec};
use doppel_desktop::exec::{Completeness, ExecOutput, ExecRequest, Termination};
use doppel_desktop::files::{ReadRequest, WritePolicy, WriteRequest};
use doppel_desktop::guest_path::GuestPath;
use doppel_desktop::input::{Button, ClickCount, Input, Via};
use doppel_desktop::mark::{Lasts, Mark, MarkAt};
use doppel_desktop::outcome::Unconfirmed;
use doppel_desktop::screen::{Screenshot, ScreenshotSize};
use doppel_desktop::screen_text::{ScreenText, TextQuery};
use doppel_desktop::wait::{Condition, TextCondition, WaitOutcome, WaitSpec};
use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::checkpoint::{GuestAfterFailure, GuestSync, Step};
use doppel_machine::login::Desktop;
use doppel_machine::machine::MachineName;
use doppel_machine::screen_probe::HostScreen;
use rmcp::model::{CallToolResult, ContentBlock};

use crate::refs::Refs;

mod catalog;

pub(crate) use catalog::ToolName;

/// Output a model sees per stream, at most: more would swamp its context.
const MCP_MAX_OUTPUT: u32 = 1 << 20;

/// A file's size through MCP, at most, either way; `doppel cp` handles more.
const MCP_MAX_FILE: u32 = 1 << 20;

/// A snapshot as a tool asks for it: what to take, and how much of it to
/// list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotListing {
    pub spec: SnapshotSpec,
    pub policy: CompactionPolicy,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolAction {
    Status,
    Screenshot {
        via: Via,
        size: ScreenshotSize,
    },
    /// Text recognition on the host; `None` reads everything.
    FindText(Option<TextQuery>),
    Input {
        via: Via,
        input: Input,
    },
    Snapshot(SnapshotListing),
    /// A ref already resolved against the session's latest snapshot.
    Act {
        r: Ref,
        target: RefTarget,
        act: ActOn,
    },
    WaitFor {
        wait: WaitSpec<Condition>,
        snapshot: SnapshotListing,
    },
    /// Polls text recognition on the host.
    WaitForText(WaitSpec<TextCondition>),
    Exec(ExecRequest),
    ReadFile(ReadRequest),
    WriteFile(WriteRequest),
    Checkpoint(CheckpointLabel),
    /// Discards the guest's current state.
    Rollback(CheckpointLabel),
    /// The call's machine is the source.
    Fork {
        checkpoint: CheckpointLabel,
        into: MachineName,
    },
    Start,
    /// Powers off: the guest's memory state is lost.
    Stop,
    /// Points something out to the people watching. `on` is the ref it
    /// was given, resolved against the session's latest snapshot.
    Mark {
        mark: Mark,
        on: Option<(Ref, RefTarget)>,
    },
}

impl ToolAction {
    /// Whether the call may replace the guest the machine's refs point into:
    /// after it, an old ref could name a different element.
    #[must_use]
    pub(crate) const fn replaces_guest(&self) -> bool {
        match self {
            Self::Rollback(_) | Self::Start | Self::Stop => true,
            Self::Status
            | Self::Screenshot { .. }
            | Self::FindText(_)
            | Self::Input { .. }
            | Self::Snapshot(_)
            | Self::Act { .. }
            | Self::WaitFor { .. }
            | Self::WaitForText(_)
            | Self::Exec(_)
            | Self::ReadFile(_)
            | Self::WriteFile(_)
            | Self::Checkpoint(_)
            | Self::Fork { .. }
            | Self::Mark { .. } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub machine: MachineName,
    pub action: ToolAction,
    /// Why the model makes it, in its words, for the people watching.
    pub why: Option<Why>,
}

/// Why a model makes a call, for the people watching: one line, trimmed,
/// of 1 to [`Why::MAX_CHARS`] characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Why(String);

impl Why {
    pub const MAX_CHARS: usize = 120;

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for Why {
    type Err = String;

    /// Blank reads as no reason: callers skip it.
    fn from_str(raw: &str) -> Result<Self, String> {
        let line = raw.trim();
        if line.is_empty() {
            return Err("why is empty: leave it out instead".into());
        }
        if line.contains(['\n', '\r']) {
            return Err("why is one line".into());
        }
        let chars = line.chars().count();
        if chars > Self::MAX_CHARS {
            return Err(format!(
                "why is {chars} characters; at most {}",
                Self::MAX_CHARS
            ));
        }
        Ok(Self(line.to_owned()))
    }
}

impl ToolCall {
    /// One line on what the call does, for a recording's captions. Leaves
    /// out what may be secret: typed text, values set, exec arguments and
    /// file contents.
    #[must_use]
    pub fn caption(&self) -> String {
        match &self.action {
            ToolAction::Status => "status".into(),
            ToolAction::Screenshot { via, .. } => format!("screenshot (via {})", via.as_str()),
            ToolAction::FindText(None) => "find_text".into(),
            ToolAction::FindText(Some(query)) => format!("find_text {:?}", query.as_str()),
            ToolAction::Input { via, input } => format!(
                "{} (via {})",
                ToolOutcome::<std::convert::Infallible>::describe(input),
                via.as_str()
            ),
            ToolAction::Snapshot(_) => "snapshot".into(),
            ToolAction::Act { r, target, act } => format!("{} {r} ({target})", act.action),
            ToolAction::WaitFor { .. } | ToolAction::WaitForText(_) => "wait_for".into(),
            ToolAction::Exec(request) => format!("exec {}", request.program()),
            ToolAction::ReadFile(request) => format!("read_file {}", request.path),
            ToolAction::WriteFile(request) => format!("write_file {}", request.path),
            ToolAction::Checkpoint(label) => format!("checkpoint {label}"),
            ToolAction::Rollback(label) => format!("rollback to {label}"),
            ToolAction::Fork { checkpoint, into } => format!("fork {checkpoint} into {into}"),
            ToolAction::Start => "start".into(),
            ToolAction::Stop => "stop".into(),
            ToolAction::Mark { mark, on } => {
                let mut caption = match (on, mark.at()) {
                    (Some((r, target)), _) => format!("mark {r} ({target})"),
                    (None, MarkAt::Point(at)) => format!("mark ({}, {})", at.x(), at.y()),
                    (None, MarkAt::Area(area)) => {
                        let rect = area.rect();
                        format!(
                            "mark {}×{} at ({}, {})",
                            rect.width(),
                            rect.height(),
                            rect.left(),
                            rect.top()
                        )
                    }
                    (None, MarkAt::Element(_)) => "mark an element".into(),
                };
                if let Some(note) = mark.note() {
                    let _ = write!(caption, ": {:?}", note.as_str());
                }
                caption
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ArgumentError {
    #[error("invalid arguments: {0}")]
    Shape(String),
    #[error("no machine given, and the server has no default (start it with --machine)")]
    NoMachine,
    #[error("{0}")]
    Value(String),
}

/// What the shell found about a machine for the status tool.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusReport {
    pub machine: MachineName,
    /// The machine's run state as the shell describes it.
    pub vm: String,
    pub agent: AgentReport,
    pub checkpoints: Vec<CheckpointLabel>,
}

/// How a started machine came up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartedFrom {
    ColdBoot,
    SavedState,
}

/// Which observation a finished wait renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitMoment {
    Satisfied,
    TimedOut,
}

/// How a checkpoint ended, as the model is told.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckpointEnd<E> {
    Completed {
        memory_bytes: u64,
        /// Why the guest wasn't synced first, if it wasn't.
        sync_skipped: Option<String>,
        /// The checkpoint is kept, but the guest may be left paused.
        left_paused: Option<Unconfirmed<E>>,
        /// Set when the guest was resumed: whether screenshots show it as it is.
        screen: Option<HostScreen>,
    },
    Failed {
        at: Step,
        error: E,
        guest: GuestAfterFailure,
    },
    /// Neither success nor failure can be claimed.
    Unknown { at: Step, error: E },
    /// The procedure ended before a terminal state (a bug).
    InProgress,
}

/// What running a [`ToolCall`] produced. `E` is the runner's error, shown
/// to the model as its message.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome<E> {
    Status(StatusReport),
    Screenshot {
        shot: Screenshot,
        via: Via,
    },
    TextFound {
        text: ScreenText,
        query: Option<TextQuery>,
    },
    Sent {
        input: Input,
        via: Via,
    },
    Snapshot {
        machine: MachineName,
        snapshot: AxSnapshot,
        policy: CompactionPolicy,
    },
    Acted {
        r: Ref,
        target: RefTarget,
        act: ActOn,
    },
    Executed(ExecOutput),
    FileRead {
        path: GuestPath,
        data: Vec<u8>,
    },
    FileWritten {
        path: GuestPath,
        bytes: usize,
        policy: WritePolicy,
    },
    Waited {
        machine: MachineName,
        condition: Condition,
        outcome: WaitOutcome<AxSnapshot>,
        policy: CompactionPolicy,
    },
    WaitedForText {
        condition: TextCondition,
        outcome: WaitOutcome<ScreenText>,
    },
    /// The checkpoint procedure's terminal state.
    Checkpointed {
        machine: MachineName,
        label: CheckpointLabel,
        result: CheckpointEnd<E>,
        paused_for: Option<Duration>,
    },
    Forked {
        source: MachineName,
        checkpoint: CheckpointLabel,
        into: MachineName,
    },
    Started {
        machine: MachineName,
        from: StartedFrom,
        desktop: Desktop,
    },
    Stopped {
        machine: MachineName,
    },
    Marked {
        mark: Mark,
        on: Option<(Ref, RefTarget)>,
    },
    RolledBack {
        machine: MachineName,
        checkpoint: CheckpointLabel,
        desktop: Desktop,
    },
    /// The call failed. When its outcome is unknown the model is told to
    /// look before retrying: a click may have landed, a guest may be paused.
    Failed(Unconfirmed<E>),
}

impl<E: std::fmt::Display> ToolOutcome<E> {
    /// The `tools/call` result. Failures are tool errors (`isError`), so the
    /// model sees them and can correct itself. Snapshots shown here become the
    /// machine's latest refs.
    #[must_use]
    pub(crate) fn into_result(self, refs: &mut Refs) -> CallToolResult {
        match self {
            Self::Snapshot {
                machine,
                snapshot,
                policy,
            } => Self::text(&refs.show(&machine, &snapshot, policy).text, false),
            Self::Acted { r, target, act } => Self::text(
                &format!(
                    "{} {r} ({target}). The effect is asynchronous: wait_for or snapshot to see it.",
                    act.action
                ),
                false,
            ),
            Self::Waited {
                machine,
                condition,
                outcome,
                policy,
            } => {
                let what = match &condition {
                    Condition::Appears(t) => format!("{t:?} appeared"),
                    Condition::Disappears(t) => format!("{t:?} disappeared"),
                    Condition::Frontmost(t) => format!("{t} is frontmost"),
                };
                Self::render_wait(&what, outcome, |snapshot, moment| {
                    let listing = refs.show(&machine, &snapshot, policy).text;
                    match moment {
                        WaitMoment::Satisfied => listing,
                        WaitMoment::TimedOut => format!(" The UI now:\n{listing}"),
                    }
                })
            }
            Self::WaitedForText { condition, outcome } => {
                let query = condition.query().as_str();
                let what = match &condition {
                    TextCondition::Appears(_) => format!("{query:?} appeared on screen"),
                    TextCondition::Disappears(_) => format!("{query:?} is gone from the screen"),
                };
                Self::render_wait(&what, outcome, |text, moment| match (&condition, moment) {
                    // Where it appeared, with points to click.
                    (TextCondition::Appears(q), WaitMoment::Satisfied) => text.render(Some(q)),
                    (TextCondition::Disappears(_), WaitMoment::Satisfied) => String::new(),
                    (TextCondition::Appears(_), WaitMoment::TimedOut) => {
                        format!(" The screen now:\n{}", text.render(None))
                    }
                    (TextCondition::Disappears(q), WaitMoment::TimedOut) => {
                        format!(" Still on screen:\n{}", text.render(Some(q)))
                    }
                })
            }
            Self::Status(report) => Self::text(&report.render(), false),
            Self::Screenshot { shot, via } => CallToolResult::success(vec![
                ContentBlock::image(
                    base64::engine::general_purpose::STANDARD.encode(&shot.data),
                    shot.format.mime_type(),
                ),
                ContentBlock::text(format!("via {}: {}", via.as_str(), shot.coordinate_note())),
            ]),
            Self::TextFound { text, query } => Self::text(&text.render(query.as_ref()), false),
            Self::Sent { input, via } => Self::text(
                &format!("{} via {}", Self::describe(&input), via.as_str()),
                false,
            ),
            Self::Executed(output) => Self::text(
                &Self::render_exec(&output),
                output.termination == Termination::TimedOut,
            ),
            Self::FileRead { path, data } => {
                let text = match String::from_utf8(data) {
                    Ok(text) => format!("{path} ({} bytes, UTF-8):\n{text}", text.len()),
                    Err(error) => {
                        let data = error.into_bytes();
                        format!(
                            "{path} ({} bytes, not UTF-8; base64):\n{}",
                            data.len(),
                            base64::engine::general_purpose::STANDARD.encode(&data)
                        )
                    }
                };
                Self::text(&text, false)
            }
            Self::FileWritten {
                path,
                bytes,
                policy,
            } => {
                let how = match policy {
                    WritePolicy::CreateNew => "created",
                    WritePolicy::Replace => "wrote",
                };
                Self::text(&format!("{how} {path} ({bytes} bytes)"), false)
            }
            Self::Checkpointed {
                machine,
                label,
                result,
                paused_for,
            } => Self::render_checkpoint(&machine, &label, &result, paused_for),
            Self::Forked {
                source,
                checkpoint,
                into,
            } => Self::text(
                &format!(
                    "created {into} from {source}/{checkpoint}; it is stopped. start it to resume exactly at the checkpoint (stop {source} and any other fork of it first)."
                ),
                false,
            ),
            Self::Started {
                machine,
                from,
                desktop,
            } => {
                let how = match from {
                    StartedFrom::ColdBoot => "booted",
                    StartedFrom::SavedState => "resumed its saved state",
                };
                Self::desktop(&format!("{machine} {how}"), &desktop)
            }
            Self::Stopped { machine } => Self::text(&format!("{machine} is powered off"), false),
            Self::Marked { mark, on } => {
                let what = match &on {
                    Some((r, target)) => format!("{r} ({target})"),
                    None => "it".into(),
                };
                let how_long = match mark.lasts() {
                    Lasts::For(time) => format!("for {} s", time.as_secs()),
                    Lasts::UntilActed(time) => {
                        format!(
                            "until you next act on the screen, {} s at most",
                            time.as_secs()
                        )
                    }
                };
                Self::text(
                    &format!(
                        "marked {what} {how_long}. The people watching see it; the machine doesn't."
                    ),
                    false,
                )
            }
            Self::RolledBack {
                machine,
                checkpoint,
                desktop,
            } => Self::desktop(
                &format!(
                    "{machine} rolled back to {checkpoint}; everything since was discarded. Take a new snapshot: earlier refs are gone"
                ),
                &desktop,
            ),
            Self::Failed(Unconfirmed::Failed(error)) => Self::text(&error.to_string(), true),
            Self::Failed(Unconfirmed::Unknown(error)) => Self::text(
                &format!(
                    "{error}. The outcome is unknown: it may have taken effect. Check (status, snapshot or screenshot) before retrying."
                ),
                true,
            ),
        }
    }

    /// A start's result, then whether the desktop is ready. A machine that runs
    /// without a usable desktop is an error the model must see.
    fn desktop(started: &str, desktop: &Desktop) -> CallToolResult {
        if desktop.is_ready() {
            Self::text(&format!("{started}; desktop ready ({desktop})."), false)
        } else {
            Self::text(
                &format!(
                    "{started}, but the desktop isn't ready: {desktop}. via=\"host\" screenshot and input still work."
                ),
                true,
            )
        }
    }

    fn render_checkpoint(
        machine: &MachineName,
        label: &CheckpointLabel,
        result: &CheckpointEnd<E>,
        paused_for: Option<Duration>,
    ) -> CallToolResult {
        let paused = paused_for.map_or_else(String::new, |d| {
            format!("; the guest was paused {:.1} s", d.as_secs_f64())
        });
        match result {
            CheckpointEnd::Completed {
                memory_bytes,
                sync_skipped,
                left_paused,
                screen,
            } => {
                let size = format!("{} MiB of memory", memory_bytes >> 20);
                let caveat = sync_skipped.as_ref().map_or_else(String::new, |why| {
                    GuestSync::<std::convert::Infallible>::skipped_caveat(why)
                });
                let screen = screen
                    .as_ref()
                    .map_or_else(String::new, |screen| format!("; {screen}"));
                match left_paused {
                    None => Self::text(
                        &format!(
                            "checkpoint {machine}/{label} saved ({size}){paused}{caveat}; the machine keeps running{screen}."
                        ),
                        false,
                    ),
                    Some(Unconfirmed::Failed(error)) => Self::text(
                        &format!(
                            "checkpoint {machine}/{label} saved ({size}), but the guest is still paused: {error}"
                        ),
                        true,
                    ),
                    Some(Unconfirmed::Unknown(error)) => Self::text(
                        &format!(
                            "checkpoint {machine}/{label} saved ({size}), but the guest may still be paused ({error}); check status"
                        ),
                        true,
                    ),
                }
            }
            CheckpointEnd::Failed { at, error, guest } => {
                let guest = match guest {
                    GuestAfterFailure::Untouched => "the guest was never paused",
                    GuestAfterFailure::ResumeRequested => "a resume of the guest was requested",
                };
                Self::text(
                    &format!(
                        "checkpoint {machine}/{label} failed while {at:?}: {error}; {guest}. No checkpoint was kept."
                    ),
                    true,
                )
            }
            CheckpointEnd::Unknown { at, error } => Self::text(
                &format!(
                    "checkpoint {machine}/{label}: outcome UNKNOWN, the VMM stopped answering while {at:?} ({error}). Check status before relying on it."
                ),
                true,
            ),
            CheckpointEnd::InProgress => Self::text(
                &format!("checkpoint {machine}/{label}: the procedure ended without a result"),
                true,
            ),
        }
    }

    /// Status first, then each non-empty stream. Output that isn't UTF-8 is
    /// shown lossily and says so.
    fn render_exec(output: &ExecOutput) -> String {
        let mut text = output.termination.to_string();
        match output.completeness {
            Completeness::Full => {}
            Completeness::Truncated => text.push_str(" (output truncated at max_output_bytes)"),
            Completeness::Unreported(_) => text.push_str(" (output completeness unreported)"),
        }
        for (name, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
            if bytes.is_empty() {
                continue;
            }
            let shown = String::from_utf8_lossy(bytes);
            let note = match std::str::from_utf8(bytes) {
                Ok(_) => "",
                Err(_) => " (not UTF-8; shown lossily)",
            };
            let _ = write!(
                text,
                "\n--- {name}{note} ---\n{}",
                shown.trim_end_matches('\n')
            );
        }
        text
    }

    #[must_use]
    /// A finished wait: satisfied succeeds, a timeout or failure is an error.
    /// `show` renders the observation that ended it.
    fn render_wait<S>(
        what: &str,
        outcome: WaitOutcome<S>,
        show: impl FnOnce(S, WaitMoment) -> String,
    ) -> CallToolResult {
        match outcome {
            WaitOutcome::Satisfied {
                seen,
                elapsed,
                polls,
            } => Self::text(
                &format!(
                    "{what} after {} ms ({polls} polls).\n{}",
                    elapsed.as_millis(),
                    show(seen, WaitMoment::Satisfied)
                ),
                false,
            ),
            WaitOutcome::TimedOut {
                last,
                last_error,
                elapsed,
                polls,
            } => {
                let mut text = format!(
                    "timed out after {} ms ({polls} polls) waiting until {what}.",
                    elapsed.as_millis()
                );
                if let Some(error) = last_error {
                    let _ = write!(text, " Last poll failed: {error}.");
                }
                if let Some(seen) = last {
                    text.push_str(&show(seen, WaitMoment::TimedOut));
                }
                Self::text(&text, true)
            }
            WaitOutcome::Failed { reason, polls } => Self::text(
                &format!("wait_for failed after {polls} polls: {reason}"),
                true,
            ),
        }
    }

    pub(crate) fn rejected(error: &ArgumentError) -> CallToolResult {
        Self::text(&error.to_string(), true)
    }

    fn text(text: &str, is_error: bool) -> CallToolResult {
        let content = vec![ContentBlock::text(text)];
        if is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        }
    }

    fn describe(input: &Input) -> String {
        match input {
            Input::Click { at, button, count } => {
                let button = match button {
                    Button::Left => "",
                    Button::Right => "right-",
                };
                let count = match count {
                    ClickCount::Single => "",
                    ClickCount::Double => "double-",
                    ClickCount::Triple => "triple-",
                };
                format!("{count}{button}clicked ({}, {})", at.x(), at.y())
            }
            Input::MoveTo(at) => format!("moved to ({}, {})", at.x(), at.y()),
            Input::Drag { from, to } => format!(
                "dragged ({}, {}) → ({}, {})",
                from.x(),
                from.y(),
                to.x(),
                to.y()
            ),
            Input::Scroll { dx, dy, .. } => format!("scrolled ({dx}, {dy})"),
            Input::Key(_) => "pressed the key".into(),
            Input::Type(text) => format!("typed {} characters", text.chars().count()),
            Input::Pointer(pointer) => {
                format!("pointer at ({}, {})", pointer.at.x(), pointer.at.y())
            }
        }
    }
}

impl StatusReport {
    fn render(&self) -> String {
        let mut out = format!("{}: {}\n", self.machine, self.vm);
        match &self.agent {
            AgentReport::NotRunning => {}
            AgentReport::Unreachable(why) => {
                let _ = writeln!(out, "agent: unreachable ({why})");
                out.push_str("via=\"host\" still works for screenshot, click, type and key.\n");
            }
            AgentReport::Connected(status) => {
                let _ = writeln!(
                    out,
                    "agent: connected; session {}; display {}",
                    status.session, status.display
                );
                if let Some(app) = &status.frontmost {
                    let id = app.bundle_id.as_deref().unwrap_or("no bundle id");
                    let _ = writeln!(out, "frontmost app: {} ({id}, pid {})", app.name, app.pid);
                }
                let missing = status.missing();
                if missing.is_empty() {
                    out.push_str("permissions: all granted\n");
                } else {
                    let names: Vec<String> = missing.iter().map(ToString::to_string).collect();
                    let _ = writeln!(
                        out,
                        "permissions missing in the guest: {} (grant them to the app that runs doppel-agent in System Settings → Privacy & Security)",
                        names.join(", ")
                    );
                }
            }
        }
        if self.checkpoints.is_empty() {
            out.push_str("checkpoints: none\n");
        } else {
            let labels: Vec<&str> = self
                .checkpoints
                .iter()
                .map(CheckpointLabel::as_str)
                .collect();
            let _ = writeln!(out, "checkpoints: {}", labels.join(", "));
        }
        out.trim_end().to_owned()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use doppel_desktop::agent::{Permission, Session};
    use doppel_desktop::ax::tree::ElementAction;
    use doppel_desktop::files::FileMode;
    use doppel_desktop::geometry::Point;
    use doppel_desktop::geometry::Size;
    use doppel_desktop::mark::{DEFAULT_LASTS, Shape};
    use doppel_desktop::screen::ImageFormat;
    use serde_json::{Map, Value, json};

    use super::*;
    use doppel_desktop::agent::AgentStatus;

    type TestOutcome = ToolOutcome<String>;

    impl<E: std::fmt::Display> ToolOutcome<E> {
        /// The result as it goes on the wire.
        fn into_wire(self, refs: &mut Refs) -> Value {
            serde_json::to_value(self.into_result(refs)).unwrap()
        }
    }

    fn args(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other @ (Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_)) => {
                panic!("not an object: {other}")
            }
        }
    }

    fn dev() -> MachineName {
        "dev".parse().unwrap()
    }

    #[test]
    fn the_mcp_doc_names_every_tool() {
        let doc = include_str!("../../../../docs/mcp.md");
        for tool in ToolName::ALL {
            let name = tool.as_str();
            assert!(
                doc.contains(&format!("`{name}`")),
                "docs/mcp.md doesn't mention `{name}`: add it to its Tools table"
            );
        }
    }

    #[test]
    fn definitions_cover_every_tool_with_closed_schemas() {
        let definitions = serde_json::to_value(ToolName::definitions()).unwrap();
        let tools = definitions.as_array().unwrap();
        assert_eq!(tools.len(), ToolName::ALL.len());
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            assert!(name.parse::<ToolName>().is_ok(), "{name}");
            assert_eq!(
                tool["inputSchema"]["additionalProperties"],
                json!(false),
                "{name}"
            );
            // Derived, then kept to what a model needs: no Rust names, no
            // numeric formats, no null for an argument that may be left out.
            let schema = tool["inputSchema"].to_string();
            for noise in ["\"title\"", "\"format\"", "\"null\"", "$ref", "$defs"] {
                assert!(!schema.contains(noise), "{name}: {noise} in {schema}");
            }
        }
        let click = &tools[ToolName::ALL
            .iter()
            .position(|t| *t == ToolName::Click)
            .unwrap()];
        assert_eq!(click["inputSchema"]["required"], json!(["x", "y"]));
        assert_eq!(
            click["inputSchema"]["properties"]["count"],
            json!({
                "type": "integer", "minimum": 1, "maximum": 3,
                "description": "2 for a double click. Default 1."
            })
        );
    }

    #[test]
    fn click_parses_into_domain_input() {
        let call = ToolCall::parse(
            ToolName::Click,
            args(json!({ "x": 10, "y": 20.5, "count": 2, "via": "host" })),
            Some(&dev()),
            &Refs::default(),
        )
        .unwrap();
        assert_eq!(call.machine, dev());
        assert_eq!(
            call.action,
            ToolAction::Input {
                via: Via::Host,
                input: Input::Click {
                    at: Point::new(10.0, 20.5).unwrap(),
                    button: Button::Left,
                    count: ClickCount::Double
                }
            }
        );
    }

    #[test]
    fn captions_say_what_a_call_does_without_what_it_typed_or_ran_with() {
        let caption = |tool, value| {
            ToolCall::parse(tool, args(value), Some(&dev()), &Refs::default())
                .unwrap()
                .caption()
        };
        assert_eq!(
            caption(
                ToolName::Click,
                json!({ "x": 10, "y": 20, "count": 2, "via": "host" })
            ),
            "double-clicked (10, 20) (via host)"
        );
        assert_eq!(
            caption(ToolName::Type, json!({ "text": "hunter2" })),
            "typed 7 characters (via agent)"
        );
        assert_eq!(
            caption(
                ToolName::Exec,
                json!({ "program": "/usr/bin/curl", "args": ["-u", "me:secret"] })
            ),
            "exec /usr/bin/curl"
        );
        assert_eq!(
            caption(ToolName::Checkpoint, json!({ "label": "clean" })),
            "checkpoint clean"
        );
    }

    #[test]
    fn bad_arguments_are_rejected() {
        let reject = |tool, value| {
            ToolCall::parse(tool, args(value), Some(&dev()), &Refs::default()).unwrap_err()
        };
        assert!(matches!(
            reject(ToolName::Click, json!({ "x": 1 })),
            ArgumentError::Shape(_)
        ));
        assert!(matches!(
            reject(ToolName::Click, json!({ "x": -1, "y": 0 })),
            ArgumentError::Value(_)
        ));
        assert!(matches!(
            reject(ToolName::Click, json!({ "x": 1, "y": 1, "count": 4 })),
            ArgumentError::Value(_)
        ));
        assert!(matches!(
            reject(ToolName::Key, json!({ "chord": "hyper+x" })),
            ArgumentError::Value(_)
        ));
        assert!(matches!(
            reject(ToolName::Type, json!({ "text": "" })),
            ArgumentError::Value(_)
        ));
        assert!(matches!(
            reject(ToolName::Type, json!({ "text": "a", "extra": 1 })),
            ArgumentError::Shape(_)
        ));
        assert!(matches!(
            reject(ToolName::Screenshot, json!({ "via": "telepathy" })),
            ArgumentError::Shape(_)
        ));
        assert!(matches!(
            reject(ToolName::Status, json!({ "machine": "Not A Name" })),
            ArgumentError::Value(_)
        ));
        assert_eq!(
            ToolCall::parse(ToolName::Status, Map::new(), None, &Refs::default()).unwrap_err(),
            ArgumentError::NoMachine
        );
    }

    #[test]
    fn screenshot_defaults_to_point_matching() {
        let call = ToolCall::parse(
            ToolName::Screenshot,
            Map::new(),
            Some(&dev()),
            &Refs::default(),
        )
        .unwrap();
        assert_eq!(
            call.action,
            ToolAction::Screenshot {
                via: Via::Host,
                size: ScreenshotSize::MatchPoints
            },
            "screenshots default to the host"
        );
        let call = ToolCall::parse(
            ToolName::Screenshot,
            args(json!({ "via": "agent" })),
            Some(&dev()),
            &Refs::default(),
        )
        .unwrap();
        assert_eq!(
            call.action,
            ToolAction::Screenshot {
                via: Via::Agent,
                size: ScreenshotSize::MatchPoints
            }
        );
    }

    #[test]
    fn find_text_reads_everything_or_parses_its_query() {
        let call = |arguments: Value| {
            ToolCall::parse(
                ToolName::FindText,
                args(arguments),
                Some(&dev()),
                &Refs::default(),
            )
        };
        assert_eq!(call(json!({})).unwrap().action, ToolAction::FindText(None));
        assert_eq!(
            call(json!({ "text": " Save " })).unwrap().action,
            ToolAction::FindText(Some("Save".parse().unwrap()))
        );
        assert!(matches!(
            call(json!({ "text": "  " })),
            Err(ArgumentError::Value(_))
        ));
        assert!(matches!(
            call(json!({ "via": "agent" })),
            Err(ArgumentError::Shape(_))
        ));
    }

    #[test]
    fn found_text_renders_click_points_and_misses_are_not_errors() {
        use doppel_desktop::geometry::Rect;
        use doppel_desktop::screen_text::{Confidence, TextLine};
        let region = |x, w| Rect::new(x, 100.0, w, 20.0).unwrap();
        let line = TextLine::new(
            "Cancel Save".into(),
            region(10.0, 110.0),
            Confidence::new(0.9).unwrap(),
            vec![
                ("Cancel".into(), region(10.0, 60.0)),
                ("Save".into(), region(80.0, 40.0)),
            ],
        )
        .unwrap();
        let text = ScreenText::new(Size::new(1280.0, 800.0).unwrap(), vec![line]);
        let found = TestOutcome::TextFound {
            text: text.clone(),
            query: Some("save".parse().unwrap()),
        }
        .into_wire(&mut Refs::default());
        assert_eq!(found["isError"], false);
        assert!(
            found["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains(r#"- "Save" at 100, 110 in "Cancel Save""#),
            "{found}"
        );
        let missed = TestOutcome::TextFound {
            text,
            query: Some("Open".parse().unwrap()),
        }
        .into_wire(&mut Refs::default());
        assert_eq!(missed["isError"], false);
        assert!(
            missed["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("not found")
        );
    }

    #[test]
    fn wait_for_text_parses_without_snapshot_options() {
        let call = |arguments: Value| {
            ToolCall::parse(
                ToolName::WaitFor,
                args(arguments),
                Some(&dev()),
                &Refs::default(),
            )
        };
        let action = call(json!({ "until": "text_appears", "text": "Saved", "timeout_ms": 900 }))
            .unwrap()
            .action;
        let ToolAction::WaitForText(wait) = action else {
            panic!("expected a text wait, got {action:?}")
        };
        assert_eq!(
            wait.condition(),
            &TextCondition::Appears("Saved".parse().unwrap())
        );
        assert_eq!(wait.timeout(), Duration::from_millis(900));
        assert!(matches!(
            call(json!({ "until": "text_disappears", "text": "x", "scope": "app" })),
            Err(ArgumentError::Value(_))
        ));
        assert!(call(json!({ "until": "text_appears", "text": " " })).is_err());
    }

    #[test]
    fn text_waits_show_where_it_appeared_or_where_it_still_is() {
        use doppel_desktop::geometry::Rect;
        use doppel_desktop::screen_text::{Confidence, TextLine};
        let screen = |line: &str| {
            let region = Rect::new(100.0, 200.0, 80.0, 20.0).unwrap();
            let line =
                TextLine::new(line.into(), region, Confidence::new(1.0).unwrap(), vec![]).unwrap();
            ScreenText::new(Size::new(1280.0, 800.0).unwrap(), vec![line])
        };
        let text = |outcome: TestOutcome| {
            let wire = outcome.into_wire(&mut Refs::default());
            (
                wire["isError"].as_bool().unwrap(),
                wire["content"][0]["text"].as_str().unwrap().to_owned(),
            )
        };
        let (error, appeared) = text(TestOutcome::WaitedForText {
            condition: TextCondition::Appears("saved".parse().unwrap()),
            outcome: WaitOutcome::Satisfied {
                seen: screen("Saved"),
                elapsed: Duration::from_millis(700),
                polls: 4,
            },
        });
        assert!(!error);
        assert!(
            appeared.starts_with("\"saved\" appeared on screen after 700 ms (4 polls).")
                && appeared.contains("- \"Saved\" at 140, 210"),
            "{appeared}"
        );
        let (error, stuck) = text(TestOutcome::WaitedForText {
            condition: TextCondition::Disappears("loading".parse().unwrap()),
            outcome: WaitOutcome::TimedOut {
                last: Some(screen("Loading…")),
                last_error: None,
                elapsed: Duration::from_secs(5),
                polls: 9,
            },
        });
        assert!(error);
        assert!(
            stuck.contains("waiting until \"loading\" is gone from the screen. Still on screen:")
                && stuck.contains("- \"Loading\" at 140, 210 in \"Loading…\""),
            "{stuck}"
        );
    }

    #[test]
    fn screenshot_result_carries_image_and_scale() {
        let result = TestOutcome::Screenshot {
            shot: Screenshot {
                format: ImageFormat::Png,
                width_pixels: 4,
                height_pixels: 2,
                display: Size::new(4.0, 2.0).unwrap(),
                data: vec![0xff, 0x00],
            },
            via: Via::Agent,
        }
        .into_wire(&mut Refs::default());
        assert_eq!(result["content"][0]["type"], "image");
        assert_eq!(result["content"][0]["data"], "/wA=");
        assert_eq!(result["content"][0]["mimeType"], "image/png");
        assert!(
            result["content"][1]["text"]
                .as_str()
                .unwrap()
                .contains("are display points")
        );
        assert_eq!(result["isError"], false);
    }

    #[test]
    fn status_names_missing_permissions_and_fallbacks() {
        let connected = StatusReport {
            machine: dev(),
            vm: "running".into(),
            agent: AgentReport::Connected(AgentStatus {
                granted: vec![Permission::Accessibility],
                session: Session::Active,
                display: Size::new(1280.0, 800.0).unwrap(),
                frontmost: None,
            }),
            checkpoints: vec!["ready".parse().unwrap(), "before-install".parse().unwrap()],
        }
        .render();
        assert!(
            connected.contains("missing in the guest: Screen Recording"),
            "{connected}"
        );
        assert!(
            connected.ends_with("checkpoints: ready, before-install"),
            "{connected}"
        );
        let unreachable = StatusReport {
            machine: dev(),
            vm: "running".into(),
            agent: AgentReport::Unreachable("x".into()),
            checkpoints: Vec::new(),
        }
        .render();
        assert!(unreachable.contains("via=\"host\""), "{unreachable}");
        assert!(unreachable.ends_with("checkpoints: none"), "{unreachable}");
        let failed =
            TestOutcome::Failed(Unconfirmed::Failed("boom".into())).into_wire(&mut Refs::default());
        assert_eq!(failed["isError"], true);
        assert_eq!(failed["content"][0]["text"], "boom");
    }

    #[test]
    fn an_unknown_outcome_tells_the_model_to_look_before_retrying() {
        let unknown = TestOutcome::Failed(Unconfirmed::Unknown(
            "timed out waiting for the agent".into(),
        ))
        .into_wire(&mut Refs::default());
        assert_eq!(unknown["isError"], true);
        let text = unknown["content"][0]["text"].as_str().unwrap();
        assert!(
            text.starts_with("timed out waiting for the agent. The outcome is unknown"),
            "{text}"
        );
    }

    mod lifecycle {
        use std::time::Duration;

        use super::*;

        fn parse(tool: ToolName, arguments: Value) -> Result<ToolCall, ArgumentError> {
            ToolCall::parse(tool, args(arguments), Some(&dev()), &Refs::default())
        }

        fn text(result: &Value) -> &str {
            result["content"][0]["text"].as_str().unwrap()
        }

        fn checkpointed(result: CheckpointEnd<String>) -> Value {
            TestOutcome::Checkpointed {
                machine: dev(),
                label: "ready".parse().unwrap(),
                result,
                paused_for: Some(Duration::from_millis(4_700)),
            }
            .into_wire(&mut Refs::default())
        }

        #[test]
        fn any_tool_takes_a_why_for_the_people_watching() {
            let call = parse(
                ToolName::Checkpoint,
                json!({ "label": "ready", "why": "  before the install " }),
            )
            .unwrap();
            assert_eq!(
                call.why.as_ref().map(Why::as_str),
                Some("before the install")
            );
            // Blank says nothing; a long, split or non-string one is refused.
            assert_eq!(
                parse(ToolName::Stop, json!({ "why": " " })).unwrap().why,
                None
            );
            assert!(
                parse(
                    ToolName::Stop,
                    json!({ "why": "x".repeat(Why::MAX_CHARS + 1) })
                )
                .is_err()
            );
            assert!(parse(ToolName::Stop, json!({ "why": "one\ntwo" })).is_err());
            assert!(parse(ToolName::Stop, json!({ "why": 3 })).is_err());
            let definitions = serde_json::to_value(ToolName::definitions()).unwrap();
            for tool in definitions.as_array().unwrap() {
                assert_eq!(
                    tool["inputSchema"]["properties"]["why"]["type"], "string",
                    "{}",
                    tool["name"]
                );
            }
        }

        #[test]
        fn lifecycle_calls_parse_into_typed_actions() {
            assert_eq!(
                parse(ToolName::Checkpoint, json!({ "label": "before-install" })).unwrap(),
                ToolCall {
                    machine: dev(),
                    action: ToolAction::Checkpoint("before-install".parse().unwrap()),
                    why: None,
                }
            );
            assert_eq!(
                parse(ToolName::Rollback, json!({ "checkpoint": "ready" }))
                    .unwrap()
                    .action,
                ToolAction::Rollback("ready".parse().unwrap())
            );
            assert_eq!(
                parse(
                    ToolName::Fork,
                    json!({ "checkpoint": "ready", "into": "try-1" })
                )
                .unwrap()
                .action,
                ToolAction::Fork {
                    checkpoint: "ready".parse().unwrap(),
                    into: "try-1".parse().unwrap(),
                }
            );
            assert_eq!(
                parse(ToolName::Start, json!({})).unwrap().action,
                ToolAction::Start
            );
            assert_eq!(
                parse(ToolName::Stop, json!({ "machine": "dev" }))
                    .unwrap()
                    .action,
                ToolAction::Stop
            );
        }

        #[test]
        fn bad_lifecycle_arguments_are_rejected() {
            for (tool, arguments, expected) in [
                (
                    ToolName::Checkpoint,
                    json!({ "label": "Before Install" }),
                    "label:",
                ),
                (ToolName::Checkpoint, json!({}), "missing field `label`"),
                (
                    ToolName::Rollback,
                    json!({ "checkpoint": "../x" }),
                    "checkpoint:",
                ),
                (
                    ToolName::Rollback,
                    json!({ "label": "ready" }),
                    "unknown field `label`",
                ),
                (
                    ToolName::Fork,
                    json!({ "checkpoint": "ready", "into": "dev" }),
                    "new machine",
                ),
                (
                    ToolName::Fork,
                    json!({ "checkpoint": "ready", "into": "No Good" }),
                    "into:",
                ),
                (
                    ToolName::Stop,
                    json!({ "force": true }),
                    "unknown field `force`",
                ),
            ] {
                let error = parse(tool, arguments).unwrap_err().to_string();
                assert!(error.contains(expected), "{tool:?}: {error}");
            }
        }

        #[test]
        fn only_calls_that_replace_the_guest_invalidate_refs() {
            let label: CheckpointLabel = "ready".parse().unwrap();
            for replaces in [
                ToolAction::Rollback(label.clone()),
                ToolAction::Start,
                ToolAction::Stop,
            ] {
                assert!(replaces.replaces_guest(), "{replaces:?}");
            }
            for keeps in [
                ToolAction::Checkpoint(label.clone()),
                ToolAction::Fork {
                    checkpoint: label,
                    into: "try-1".parse().unwrap(),
                },
                ToolAction::Status,
            ] {
                assert!(!keeps.replaces_guest(), "{keeps:?}");
            }
        }

        #[test]
        fn checkpoint_results_say_whether_one_was_kept() {
            let saved = checkpointed(CheckpointEnd::Completed {
                memory_bytes: 1_248 << 20,
                sync_skipped: None,
                left_paused: None,
                screen: None,
            });
            assert_eq!(saved["isError"], false);
            assert_eq!(
                text(&saved),
                "checkpoint dev/ready saved (1248 MiB of memory); the guest was paused 4.7 s; the machine keeps running."
            );
            let failed = checkpointed(CheckpointEnd::Failed {
                at: Step::Cloning,
                error: "disk full".into(),
                guest: GuestAfterFailure::ResumeRequested,
            });
            assert_eq!(failed["isError"], true);
            assert!(
                text(&failed).contains("No checkpoint was kept"),
                "{}",
                text(&failed)
            );
            // Never rounded up to success.
            let unknown = checkpointed(CheckpointEnd::Unknown {
                at: Step::SavingMemory,
                error: "VMM closed the connection".into(),
            });
            assert_eq!(unknown["isError"], true);
            assert!(text(&unknown).contains("UNKNOWN"), "{}", text(&unknown));
            assert!(
                text(&unknown).contains("VMM closed the connection"),
                "{}",
                text(&unknown)
            );
            let maybe_paused = checkpointed(CheckpointEnd::Completed {
                memory_bytes: 1 << 20,
                sync_skipped: None,
                left_paused: Some(Unconfirmed::Unknown("timed out".into())),
                screen: None,
            });
            assert_eq!(maybe_paused["isError"], true);
            assert!(
                text(&maybe_paused).contains("may still be paused (timed out)"),
                "{}",
                text(&maybe_paused)
            );
            let unsynced = checkpointed(CheckpointEnd::Completed {
                memory_bytes: 1 << 20,
                sync_skipped: Some("sync timed out".into()),
                left_paused: None,
                screen: None,
            });
            assert!(
                text(&unsynced).contains("the guest wasn't synced (sync timed out)"),
                "{}",
                text(&unsynced)
            );
        }

        #[test]
        fn a_start_without_a_desktop_is_an_error() {
            let ready = TestOutcome::RolledBack {
                machine: dev(),
                checkpoint: "ready".parse().unwrap(),
                desktop: Desktop {
                    login: doppel_machine::login::LoginOutcome::AlreadyActive {
                        after: Duration::from_millis(22),
                    },
                    screen: Some(HostScreen::Current {
                        after: Duration::from_millis(3_480),
                    }),
                },
            }
            .into_wire(&mut Refs::default());
            assert_eq!(ready["isError"], false);
            assert!(
                text(&ready).contains("earlier refs are gone"),
                "{}",
                text(&ready)
            );
            let locked = TestOutcome::Started {
                machine: dev(),
                from: StartedFrom::SavedState,
                desktop: Desktop {
                    login: doppel_machine::login::LoginOutcome::NeedsPassword,
                    screen: None,
                },
            }
            .into_wire(&mut Refs::default());
            assert_eq!(locked["isError"], true);
            assert!(
                text(&locked)
                    .starts_with("dev resumed its saved state, but the desktop isn't ready"),
                "{}",
                text(&locked)
            );
        }
    }

    pub(crate) mod ax {
        use doppel_desktop::agent::{ProcessId, RunningApp};
        use doppel_desktop::ax::tree::{
            AxNode, AxText, AxTree, NodeIndex, Role, SnapshotId, States, Truncation,
        };

        use super::*;

        /// A window holding one button titled `title`; shared with the server tests.
        pub(crate) fn snapshot(id: u64, title: &str) -> AxSnapshot {
            let named = |role: &str, title: &str| AxNode {
                role: Role::parse(role),
                subrole: None,
                title: AxText::new(title),
                description: None,
                identifier: None,
                value: None,
                frame: None,
                states: States::default(),
                actions: Vec::new(),
            };
            AxSnapshot {
                id: SnapshotId::new(id),
                app: RunningApp {
                    pid: ProcessId::new(5),
                    bundle_id: None,
                    name: "TextEdit".into(),
                },
                tree: AxTree::new(vec![
                    (None, named("AXWindow", "Untitled")),
                    (Some(NodeIndex::ROOT), named("AXButton", title)),
                ])
                .unwrap(),
                truncation: Truncation::Complete,
            }
        }

        fn show(refs: &mut Refs, id: u64, title: &str) -> String {
            let result = TestOutcome::Snapshot {
                machine: dev(),
                snapshot: snapshot(id, title),
                policy: CompactionPolicy::default(),
            }
            .into_wire(refs);
            assert_eq!(result["isError"], false);
            result["content"][0]["text"].as_str().unwrap().to_owned()
        }

        fn act(refs: &Refs, arguments: Value) -> Result<ToolCall, ArgumentError> {
            ToolCall::parse(ToolName::Act, args(arguments), Some(&dev()), refs)
        }

        #[test]
        fn act_resolves_refs_from_the_latest_snapshot_only() {
            let mut refs = Refs::default();
            assert!(
                matches!(act(&refs, json!({ "ref": "s1e2", "action": "press" })), Err(ArgumentError::Value(m)) if m.contains("snapshot first"))
            );
            let listing = show(&mut refs, 900, "Bold");
            assert!(listing.contains("- button \"Bold\" [s1e2]"), "{listing}");
            let call = act(&refs, json!({ "ref": "s1e2", "action": "press" })).unwrap();
            let ToolAction::Act {
                act: on, target, ..
            } = call.action
            else {
                panic!("expected Act")
            };
            assert_eq!(on.target.snapshot, SnapshotId::new(900));
            assert_eq!(on.target.node, NodeIndex::new(1));
            assert_eq!(target.name.as_deref(), Some("Bold"));

            // A new snapshot makes s1 refs stale, even though s2e2 is "the same" button.
            let listing = show(&mut refs, 901, "Italic");
            assert!(listing.contains("[s2e2]"), "{listing}");
            let stale = act(&refs, json!({ "ref": "s1e2", "action": "press" })).unwrap_err();
            assert!(stale.to_string().contains("older snapshot"), "{stale}");
            let unknown = act(&refs, json!({ "ref": "s2e9", "action": "press" })).unwrap_err();
            assert!(
                unknown.to_string().contains("not in the latest"),
                "{unknown}"
            );
            // Refs belong to a machine.
            let other = ToolCall::parse(
                ToolName::Act,
                args(json!({ "ref": "s2e2", "action": "press", "machine": "dev2" })),
                Some(&dev()),
                &refs,
            )
            .unwrap_err();
            assert!(other.to_string().contains("snapshot first"), "{other}");
        }

        fn mark(refs: &Refs, arguments: Value) -> Result<ToolCall, ArgumentError> {
            ToolCall::parse(ToolName::Mark, args(arguments), Some(&dev()), refs)
        }

        #[test]
        fn marks_take_a_ref_a_point_or_an_area() {
            let mut refs = Refs::default();
            let _ = show(&mut refs, 900, "Save");
            let on_ref = mark(
                &refs,
                json!({ "ref": "s1e2", "note": " this\none ", "until": "next_action" }),
            )
            .unwrap();
            assert_eq!(
                on_ref.caption(),
                "mark s1e2 (button \"Save\"): \"this one\""
            );
            let ToolAction::Mark { mark: on, .. } = on_ref.action else {
                panic!("expected Mark")
            };
            assert_eq!(
                on.at(),
                MarkAt::Element(doppel_desktop::ax::tree::ElementTarget {
                    snapshot: SnapshotId::new(900),
                    node: NodeIndex::new(1),
                })
            );
            assert_eq!(on.shape(), Shape::Box);
            assert_eq!(on.lasts(), Lasts::UntilActed(DEFAULT_LASTS));

            let ToolAction::Mark { mark: dot, .. } =
                mark(&refs, json!({ "x": 4, "y": 5, "seconds": 20 }))
                    .unwrap()
                    .action
            else {
                panic!("expected Mark")
            };
            assert_eq!(dot.shape(), Shape::Dot);
            assert_eq!(dot.lasts(), Lasts::For(Duration::from_secs(20)));

            let area = mark(
                &refs,
                json!({ "x": 1, "y": 2, "width": 3, "height": 4, "shape": "arrow", "from_x": 9, "from_y": 9 }),
            )
            .unwrap();
            assert_eq!(area.caption(), "mark 3×4 at (1, 2)");
            let ToolAction::Mark { mark: arrow, .. } = area.action else {
                panic!("expected Mark")
            };
            assert_eq!(
                arrow.shape(),
                Shape::Arrow {
                    from: Some(Point::new(9.0, 9.0).unwrap())
                }
            );
        }

        #[test]
        fn mark_arguments_are_checked() {
            let mut refs = Refs::default();
            let _ = show(&mut refs, 1, "OK");
            for (arguments, expect) in [
                (json!({}), "give ref, or x and y"),
                (
                    json!({ "ref": "s1e2", "x": 1, "y": 1 }),
                    "give ref, or x and y",
                ),
                (
                    json!({ "x": 1, "y": 1, "width": 3 }),
                    "give ref, or x and y",
                ),
                (
                    json!({ "x": 1, "y": 1, "width": 0, "height": 3 }),
                    "positive width",
                ),
                (json!({ "ref": "s1e9" }), "not in the latest"),
                (
                    json!({ "x": 1, "y": 1, "from_x": 3, "from_y": 3 }),
                    "only for an arrow",
                ),
                (
                    json!({ "x": 1, "y": 1, "shape": "arrow", "from_x": 3 }),
                    "together",
                ),
                (json!({ "x": 1, "y": 1, "seconds": 61 }), "1 to 60 seconds"),
                (
                    json!({ "x": 1, "y": 1, "note": "x".repeat(201) }),
                    "at most 200",
                ),
            ] {
                let error = mark(&refs, arguments.clone()).unwrap_err().to_string();
                assert!(error.contains(expect), "{arguments}: {error}");
            }
        }

        #[test]
        fn a_mark_says_who_sees_it() {
            let result = TestOutcome::Marked {
                mark: Mark::new(
                    MarkAt::Point(Point::new(1.0, 1.0).unwrap()),
                    Shape::Dot,
                    None,
                    Lasts::UntilActed(Duration::from_secs(8)),
                )
                .unwrap(),
                on: None,
            }
            .into_wire(&mut Refs::default());
            assert_eq!(result["isError"], false);
            assert_eq!(
                result["content"][0]["text"],
                "marked it until you next act on the screen, 8 s at most. The people watching see it; the machine doesn't."
            );
        }

        #[test]
        fn act_arguments_are_checked() {
            let mut refs = Refs::default();
            let _ = show(&mut refs, 1, "OK");
            for (arguments, expect) in [
                (
                    json!({ "ref": "s1e2", "action": "set_value" }),
                    "needs a value",
                ),
                (
                    json!({ "ref": "s1e2", "action": "press", "value": "x" }),
                    "only for set_value",
                ),
                (json!({ "ref": "e2", "action": "press" }), "not a ref"),
            ] {
                let error = act(&refs, arguments).unwrap_err();
                assert!(error.to_string().contains(expect), "{error}");
            }
            assert!(matches!(
                act(&refs, json!({ "ref": "s1e2", "action": "explode" })),
                Err(ArgumentError::Shape(_))
            ));
            let call = act(
                &refs,
                json!({ "ref": "s1e2", "action": "set_value", "value": "é" }),
            )
            .unwrap();
            assert!(matches!(
                call.action,
                ToolAction::Act { act: ActOn { action: ElementAction::SetValue(ref v), .. }, .. } if v == "é"
            ));
        }

        #[test]
        fn wait_for_parses_and_validates() {
            let refs = Refs::default();
            let parse = |arguments| {
                ToolCall::parse(ToolName::WaitFor, args(arguments), Some(&dev()), &refs)
            };
            let call = parse(json!({ "until": "disappears", "text": "Loading", "timeout_ms": 2500, "max_tokens": 900 })).unwrap();
            let ToolAction::WaitFor { wait, snapshot } = call.action else {
                panic!("expected WaitFor")
            };
            assert_eq!(wait.condition(), &Condition::Disappears("Loading".into()));
            assert_eq!(wait.timeout(), Duration::from_millis(2500));
            assert_eq!(snapshot.policy.budget.get(), 900);
            assert!(parse(json!({ "until": "appears", "text": "" })).is_err());
            assert!(parse(json!({ "until": "appears", "text": "x", "timeout_ms": 0 })).is_err());
            assert!(
                parse(json!({ "until": "appears", "text": "x", "timeout_ms": 600_000 })).is_err()
            );
            assert!(parse(json!({ "until": "appears", "text": "x", "max_tokens": 10 })).is_err());
            assert!(parse(json!({ "until": "soon", "text": "x" })).is_err());
        }

        #[test]
        fn wait_results_show_a_snapshot_and_timeouts_are_errors() {
            let mut refs = Refs::default();
            let waited = |outcome| TestOutcome::Waited {
                machine: dev(),
                condition: Condition::Appears("Saved".into()),
                outcome,
                policy: CompactionPolicy::default(),
            };
            let ok = waited(WaitOutcome::Satisfied {
                seen: snapshot(3, "Saved"),
                elapsed: Duration::from_millis(250),
                polls: 3,
            })
            .into_wire(&mut refs);
            assert_eq!(ok["isError"], false);
            let text = ok["content"][0]["text"].as_str().unwrap();
            assert!(
                text.starts_with("\"Saved\" appeared after 250 ms (3 polls)"),
                "{text}"
            );
            assert!(refs.resolve(&dev(), "s1e2".parse().unwrap()).is_ok());

            let late = waited(WaitOutcome::TimedOut {
                last: Some(snapshot(4, "Saving…")),
                last_error: None,
                elapsed: Duration::from_secs(5),
                polls: 9,
            })
            .into_wire(&mut refs);
            assert_eq!(late["isError"], true);
            let text = late["content"][0]["text"].as_str().unwrap();
            assert!(
                text.contains("timed out after 5000 ms") && text.contains("[s2e2]"),
                "{text}"
            );
            // The timeout's snapshot is now the latest: its refs work, older ones are stale.
            assert!(refs.resolve(&dev(), "s2e2".parse().unwrap()).is_ok());
            assert!(refs.resolve(&dev(), "s1e2".parse().unwrap()).is_err());

            let failed = waited(WaitOutcome::Failed {
                reason: "no agent".into(),
                polls: 1,
            })
            .into_wire(&mut refs);
            assert_eq!(failed["isError"], true);
        }

        #[test]
        fn a_listing_of_changes_keeps_the_refs_of_what_is_as_it_was() {
            let mut refs = Refs::default();
            let changes = CompactionPolicy {
                show: doppel_desktop::ax::compact::Show::Changes,
                ..CompactionPolicy::default()
            };
            let shown = |refs: &mut Refs, id, title| {
                TestOutcome::Snapshot {
                    machine: dev(),
                    snapshot: snapshot(id, title),
                    policy: changes,
                }
                .into_wire(refs)["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            };
            // The first listing has nothing to compare with: it lists all.
            assert!(shown(&mut refs, 1, "Save").contains("[s1e2]"));
            let text = shown(&mut refs, 2, "Saved");
            assert!(text.contains("+ button \"Saved\" [s2e1]"), "{text}");
            assert!(text.contains("- button \"Save\" (was s1e2)"), "{text}");
            // The window is as it was: its first ref still works. The button went.
            assert!(refs.resolve(&dev(), "s1e1".parse().unwrap()).is_ok());
            assert!(refs.resolve(&dev(), "s2e1".parse().unwrap()).is_ok());
            assert!(refs.resolve(&dev(), "s1e2".parse().unwrap()).is_err());
        }
    }

    mod exec {
        use super::*;

        #[test]
        fn exec_parses_and_validates() {
            let parse = |arguments| {
                ToolCall::parse(
                    ToolName::Exec,
                    args(arguments),
                    Some(&dev()),
                    &Refs::default(),
                )
            };
            let call = parse(json!({ "program": "/bin/sh", "args": ["-c", "echo hi"], "stdin": "x", "cwd": "/tmp", "timeout_ms": 2000 })).unwrap();
            let ToolAction::Exec(request) = call.action else {
                panic!("expected Exec")
            };
            assert_eq!(request.program().as_str(), "/bin/sh");
            assert_eq!(request.arguments(), ["-c", "echo hi"]);
            assert_eq!(request.stdin(), b"x");
            assert_eq!(request.working_directory().unwrap().as_str(), "/tmp");
            assert_eq!(request.timeout(), Duration::from_secs(2));
            assert_eq!(request.max_output(), ExecRequest::DEFAULT_OUTPUT);
            for (bad, expect) in [
                (json!({ "program": "sw_vers" }), "absolute"),
                (json!({ "program": "/bin/ls", "cwd": "tmp" }), "absolute"),
                (json!({ "program": "/bin/ls", "timeout_ms": 0 }), "timeout"),
                (
                    json!({ "program": "/bin/ls", "max_output_bytes": 2_000_000 }),
                    "at most",
                ),
            ] {
                let error = parse(bad).unwrap_err();
                assert!(error.to_string().contains(expect), "{error}");
            }
            assert!(matches!(
                parse(json!({ "program": "/bin/ls", "shell": true })),
                Err(ArgumentError::Shape(_))
            ));
        }

        #[test]
        fn exec_results_show_status_and_streams() {
            let render = |output| {
                let v = TestOutcome::Executed(output).into_wire(&mut Refs::default());
                (
                    v["content"][0]["text"].as_str().unwrap().to_owned(),
                    v["isError"].as_bool().unwrap(),
                )
            };
            let (text, error) = render(ExecOutput {
                termination: Termination::Exited(1),
                stdout: b"a\n".to_vec(),
                stderr: vec![0xff, b'x'],
                completeness: Completeness::Truncated,
            });
            assert!(!error, "a nonzero exit is a result, not a tool error");
            assert!(
                text.starts_with("exited with status 1 (output truncated"),
                "{text}"
            );
            assert!(text.contains("--- stdout ---\na"), "{text}");
            assert!(
                text.contains("--- stderr (not UTF-8; shown lossily) ---"),
                "{text}"
            );
            let (text, error) = render(ExecOutput {
                termination: Termination::TimedOut,
                stdout: Vec::new(),
                stderr: Vec::new(),
                completeness: Completeness::Full,
            });
            assert!(error);
            assert_eq!(text, "timed out and was stopped");
        }
    }

    mod files {
        use super::*;

        fn parse(tool: ToolName, arguments: Value) -> Result<ToolCall, ArgumentError> {
            ToolCall::parse(tool, args(arguments), Some(&dev()), &Refs::default())
        }

        #[test]
        fn read_file_parses_and_caps() {
            let call = parse(ToolName::ReadFile, json!({ "path": "/etc/hosts" })).unwrap();
            let ToolAction::ReadFile(request) = call.action else {
                panic!("expected ReadFile")
            };
            assert_eq!(request.path.as_str(), "/etc/hosts");
            assert_eq!(request.max_bytes(), MCP_MAX_FILE);
            for (bad, expect) in [
                (json!({ "path": "etc/hosts" }), "absolute"),
                (json!({ "path": "/etc/../hosts" }), "."),
                (json!({ "path": "/x", "max_bytes": 0 }), "at least 1"),
                (
                    json!({ "path": "/x", "max_bytes": MCP_MAX_FILE + 1 }),
                    "doppel cp",
                ),
            ] {
                let error = parse(ToolName::ReadFile, bad).unwrap_err();
                assert!(error.to_string().contains(expect), "{error}");
            }
        }

        #[test]
        fn write_file_takes_exactly_one_content_and_defaults_to_never_replacing() {
            let call = parse(
                ToolName::WriteFile,
                json!({ "path": "/tmp/a", "text": "hi" }),
            )
            .unwrap();
            let ToolAction::WriteFile(request) = call.action else {
                panic!("expected WriteFile")
            };
            assert_eq!(request.data(), b"hi");
            assert_eq!(request.policy, WritePolicy::CreateNew);
            assert_eq!(request.mode, FileMode::DEFAULT);

            let call = parse(
                ToolName::WriteFile,
                json!({ "path": "/tmp/a", "base64": "AP8=", "mode": "0o755", "if_exists": "replace" }),
            )
            .unwrap();
            let ToolAction::WriteFile(request) = call.action else {
                panic!("expected WriteFile")
            };
            assert_eq!(request.data(), [0x00, 0xff]);
            assert_eq!(request.policy, WritePolicy::Replace);
            assert_eq!(request.mode.bits(), 0o755);

            for (bad, expect) in [
                (json!({ "path": "/tmp/a" }), "exactly one"),
                (
                    json!({ "path": "/tmp/a", "text": "x", "base64": "eA==" }),
                    "exactly one",
                ),
                (json!({ "path": "/tmp/a", "base64": "!!" }), "base64"),
                (
                    json!({ "path": "/tmp/a", "text": "x", "mode": "rwx" }),
                    "not octal",
                ),
                (
                    json!({ "path": "/tmp/a", "text": "x", "mode": "4755" }),
                    "0o777",
                ),
                (json!({ "path": "tmp/a", "text": "x" }), "absolute"),
                (
                    json!({ "path": "/tmp/a", "text": "x".repeat(MCP_MAX_FILE as usize + 1) }),
                    "doppel cp",
                ),
            ] {
                let error = parse(ToolName::WriteFile, bad).unwrap_err();
                assert!(error.to_string().contains(expect), "{error}");
            }
            assert!(matches!(
                parse(
                    ToolName::WriteFile,
                    json!({ "path": "/tmp/a", "text": "x", "if_exists": "append" })
                ),
                Err(ArgumentError::Shape(_))
            ));
        }

        #[test]
        fn file_results_show_text_or_base64() {
            let render = |outcome: TestOutcome| {
                let v = outcome.into_wire(&mut Refs::default());
                assert_eq!(v["isError"], false);
                v["content"][0]["text"].as_str().unwrap().to_owned()
            };
            let path: GuestPath = "/tmp/a".parse().unwrap();
            let text = render(TestOutcome::FileRead {
                path: path.clone(),
                data: "é\n".into(),
            });
            assert_eq!(text, "/tmp/a (3 bytes, UTF-8):\né\n");
            let text = render(TestOutcome::FileRead {
                path: path.clone(),
                data: vec![0, 0xff],
            });
            assert_eq!(text, "/tmp/a (2 bytes, not UTF-8; base64):\nAP8=");
            let text = render(TestOutcome::FileWritten {
                path,
                bytes: 2,
                policy: WritePolicy::CreateNew,
            });
            assert_eq!(text, "created /tmp/a (2 bytes)");
        }
    }
}
