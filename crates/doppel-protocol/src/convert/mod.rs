//! The boundary: protobuf DTOs ⇄ core domain types.
//!
//! Inbound conversions are `TryFrom` (parse, don't validate). Outbound ones
//! are `From` because a valid domain value always has a wire form.

use doppel_desktop::agent::AgentReport;
use doppel_desktop::agent::Permission;
use doppel_desktop::ax::tree::{SnapshotScope, SnapshotSpec};
use doppel_desktop::input::Via;
use doppel_desktop::outcome::{Certainty, Failure};
use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::checkpoint::{AfterCheckpoint, GuestAfterFailure, Step};
use doppel_machine::lineage::{Lineage, Origin, RecordedLineage, Role, TemplateAgent};
use doppel_machine::login::{Desktop, LoginOutcome};
use doppel_machine::machine::SpecError;
use doppel_machine::screen_probe::{HostScreen, Probe, Rgb, WhyUnconfirmed};
use doppel_machine::stop::{StopMode, StopOutcome};
use doppel_machine::vm::{MachineReport, NextStart, Resumed, RunState, Status, Window};

use crate::pb::agent::v1 as agent;
use crate::pb::common::v1 as common;
use crate::pb::daemon::v1 as daemon;
use crate::pb::vmm::v1 as vmm;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("missing field `{0}`")]
    Missing(&'static str),
    #[error("field `{field}` has unknown enum value {value}")]
    UnknownEnum { field: &'static str, value: i32 },
    #[error("field `{field}` is invalid: {reason}")]
    Invalid { field: &'static str, reason: String },
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error(transparent)]
    ScreenText(#[from] doppel_desktop::screen_text::ScreenTextError),
    #[error(transparent)]
    Mark(#[from] doppel_desktop::mark::MarkError),
}

impl ParseError {
    /// A field whose value the domain refused, and why.
    pub fn invalid(field: &'static str, error: impl std::fmt::Display) -> Self {
        Self::Invalid {
            field,
            reason: error.to_string(),
        }
    }
}

// --- Via ---------------------------------------------------------------------

impl From<Via> for daemon::Via {
    fn from(via: Via) -> Self {
        match via {
            Via::Agent => Self::Agent,
            Via::Host => Self::Host,
        }
    }
}

impl From<SnapshotSpec> for daemon::SnapshotSpec {
    fn from(spec: SnapshotSpec) -> Self {
        Self {
            scope: match spec.scope {
                SnapshotScope::FocusedWindow => daemon::SnapshotScope::FocusedWindow,
                SnapshotScope::FrontmostApp => daemon::SnapshotScope::FrontmostApp,
            } as i32,
            menus: crate::pb::agent::v1::MenuPolicy::from(spec.menus) as i32,
        }
    }
}

// --- MachineReport -----------------------------------------------------------

impl TryFrom<daemon::MachineReport> for MachineReport {
    type Error = ParseError;
    fn try_from(report: daemon::MachineReport) -> Result<Self, ParseError> {
        use daemon::machine_report::Agent;
        Ok(Self {
            status: report
                .status
                .ok_or(ParseError::Missing("machine_report.status"))?
                .try_into()?,
            agent: match report
                .agent
                .ok_or(ParseError::Missing("machine_report.agent"))?
            {
                Agent::Connected(status) => AgentReport::Connected(status.try_into()?),
                Agent::Unreachable(why) => AgentReport::Unreachable(why),
                Agent::NotRunning(_) => AgentReport::NotRunning,
            },
            checkpoints: report
                .checkpoints
                .iter()
                .map(|label| label.parse())
                .collect::<Result<_, _>>()
                .map_err(|error| ParseError::Invalid {
                    field: "machine_report.checkpoints",
                    reason: format!("{error}"),
                })?,
        })
    }
}

// --- Run state ------------------------------------------------------------------

/// What a VMM says about its VM.
impl vmm::VmState {
    /// How it runs. A value this build doesn't know is kept, as
    /// [`RunState::Unrecognized`].
    #[must_use]
    pub fn state(&self) -> RunState {
        run_state(self.run_state)
    }

    /// How it's shown, when the VMM says: `None` from a VMM that doesn't,
    /// and for a value unknown here, whose number isn't kept.
    #[must_use]
    pub fn window_shown(&self) -> Option<Window> {
        match vmm::WindowMode::try_from(self.window) {
            Ok(vmm::WindowMode::Hidden) => Some(Window::Hidden),
            Ok(vmm::WindowMode::Shown) => Some(Window::Shown),
            Ok(vmm::WindowMode::Unspecified) | Err(_) => None,
        }
    }
}

/// Infallible: unknown values are kept as [`RunState::Unrecognized`].
fn run_state(raw: i32) -> RunState {
    match vmm::VmRunState::try_from(raw) {
        Ok(vmm::VmRunState::Starting) => RunState::Starting,
        Ok(vmm::VmRunState::Running) => RunState::Running,
        Ok(vmm::VmRunState::Pausing) => RunState::Pausing,
        Ok(vmm::VmRunState::Paused) => RunState::Paused,
        Ok(vmm::VmRunState::Resuming) => RunState::Resuming,
        Ok(vmm::VmRunState::Saving) => RunState::Saving,
        Ok(vmm::VmRunState::Restoring) => RunState::Restoring,
        Ok(vmm::VmRunState::Stopping) => RunState::Stopping,
        Ok(vmm::VmRunState::Stopped) => RunState::Stopped,
        Ok(vmm::VmRunState::Error) => RunState::Error,
        Ok(vmm::VmRunState::Unspecified) | Err(_) => RunState::Unrecognized(raw),
    }
}

impl From<RunState> for vmm::VmState {
    fn from(state: RunState) -> Self {
        let run_state = match state {
            RunState::Starting => vmm::VmRunState::Starting as i32,
            RunState::Running => vmm::VmRunState::Running as i32,
            RunState::Pausing => vmm::VmRunState::Pausing as i32,
            RunState::Paused => vmm::VmRunState::Paused as i32,
            RunState::Resuming => vmm::VmRunState::Resuming as i32,
            RunState::Saving => vmm::VmRunState::Saving as i32,
            RunState::Restoring => vmm::VmRunState::Restoring as i32,
            RunState::Stopping => vmm::VmRunState::Stopping as i32,
            RunState::Stopped => vmm::VmRunState::Stopped as i32,
            RunState::Error => vmm::VmRunState::Error as i32,
            // Passed on as received, so a newer VMM's state reaches a newer client.
            RunState::Unrecognized(raw) => raw,
        };
        Self {
            run_state,
            window: vmm::WindowMode::Unspecified as i32,
        }
    }
}

// --- Machine status (daemon) ---------------------------------------------------------

impl From<Window> for daemon::Window {
    fn from(window: Window) -> Self {
        match window {
            Window::Hidden => Self::Hidden,
            Window::Shown => Self::Shown,
        }
    }
}

impl From<StopMode> for vmm::StopMode {
    fn from(mode: StopMode) -> Self {
        match mode {
            StopMode::Graceful => Self::Graceful,
            StopMode::Force => Self::Force,
        }
    }
}

impl From<StopOutcome> for daemon::StopOutcome {
    fn from(outcome: StopOutcome) -> Self {
        match outcome {
            StopOutcome::Stopped => Self::Stopped,
            StopOutcome::ShutdownPending => Self::ShutdownPending,
        }
    }
}

impl From<Resumed> for daemon::ResumeOutcome {
    fn from(resumed: Resumed) -> Self {
        match resumed {
            Resumed::Resumed => Self::Resumed,
            Resumed::AlreadyRunning => Self::AlreadyRunning,
        }
    }
}

impl From<NextStart> for daemon::NextStart {
    fn from(next: NextStart) -> Self {
        match next {
            NextStart::ColdBoot => Self::ColdBoot,
            NextStart::ResumeSavedState => Self::ResumeSavedState,
        }
    }
}

impl ParseError {
    /// A prost enum field's raw value as one of our enums: `parse` maps the
    /// values it knows, and the rest (including UNSPECIFIED) are errors.
    fn enumeration<W: TryFrom<i32>, T>(
        field: &'static str,
        raw: i32,
        parse: impl FnOnce(W) -> Option<T>,
    ) -> Result<T, Self> {
        W::try_from(raw)
            .ok()
            .and_then(parse)
            .ok_or(Self::UnknownEnum { field, value: raw })
    }

    /// A permission, kept raw when this build doesn't know it.
    #[must_use]
    pub fn permission(raw: i32) -> Permission {
        match common::Permission::try_from(raw) {
            Ok(common::Permission::ScreenRecording) => Permission::ScreenRecording,
            Ok(common::Permission::Accessibility) => Permission::Accessibility,
            Ok(common::Permission::Unspecified) | Err(_) => Permission::Unrecognized(raw),
        }
    }

    /// Parses a daemon `Window` field.
    pub fn window(field: &'static str, raw: i32) -> Result<Window, Self> {
        Self::enumeration(field, raw, |window| match window {
            daemon::Window::Hidden => Some(Window::Hidden),
            daemon::Window::Shown => Some(Window::Shown),
            daemon::Window::Unspecified => None,
        })
    }

    /// Parses a VMM `StopMode` field.
    pub fn stop_mode(field: &'static str, raw: i32) -> Result<StopMode, Self> {
        Self::enumeration(field, raw, |mode| match mode {
            vmm::StopMode::Graceful => Some(StopMode::Graceful),
            vmm::StopMode::Force => Some(StopMode::Force),
            vmm::StopMode::Unspecified => None,
        })
    }

    /// Parses a daemon `StopOutcome` field.
    pub fn stop_outcome(field: &'static str, raw: i32) -> Result<StopOutcome, Self> {
        Self::enumeration(field, raw, |outcome| match outcome {
            daemon::StopOutcome::Stopped => Some(StopOutcome::Stopped),
            daemon::StopOutcome::ShutdownPending => Some(StopOutcome::ShutdownPending),
            daemon::StopOutcome::Unspecified => None,
        })
    }

    /// Parses a daemon `ResumeOutcome` field.
    pub fn resumed(field: &'static str, raw: i32) -> Result<Resumed, Self> {
        Self::enumeration(field, raw, |resumed| match resumed {
            daemon::ResumeOutcome::Resumed => Some(Resumed::Resumed),
            daemon::ResumeOutcome::AlreadyRunning => Some(Resumed::AlreadyRunning),
            daemon::ResumeOutcome::Unspecified => None,
        })
    }

    /// Parses a daemon `NextStart` field.
    pub fn next_start(field: &'static str, raw: i32) -> Result<NextStart, Self> {
        Self::enumeration(field, raw, |next| match next {
            daemon::NextStart::ColdBoot => Some(NextStart::ColdBoot),
            daemon::NextStart::ResumeSavedState => Some(NextStart::ResumeSavedState),
            daemon::NextStart::Unspecified => None,
        })
    }

    /// Parses a daemon `CheckpointStep` field.
    pub fn checkpoint_step(field: &'static str, raw: i32) -> Result<Step, Self> {
        Self::enumeration(field, raw, |step| match step {
            daemon::CheckpointStep::SyncingGuest => Some(Step::SyncingGuest),
            daemon::CheckpointStep::Pausing => Some(Step::Pausing),
            daemon::CheckpointStep::Flushing => Some(Step::Flushing),
            daemon::CheckpointStep::Cloning => Some(Step::Cloning),
            daemon::CheckpointStep::SavingMemory => Some(Step::SavingMemory),
            daemon::CheckpointStep::Committing => Some(Step::Committing),
            daemon::CheckpointStep::Finishing => Some(Step::Finishing),
            daemon::CheckpointStep::Unspecified => None,
        })
    }

    /// Parses a daemon `GuestAfterFailure` field.
    pub fn guest_after_failure(field: &'static str, raw: i32) -> Result<GuestAfterFailure, Self> {
        Self::enumeration(field, raw, |guest| match guest {
            daemon::GuestAfterFailure::Untouched => Some(GuestAfterFailure::Untouched),
            daemon::GuestAfterFailure::ResumeRequested => Some(GuestAfterFailure::ResumeRequested),
            daemon::GuestAfterFailure::Unspecified => None,
        })
    }

    /// Parses a daemon `AfterCheckpoint` field.
    pub fn after_checkpoint(field: &'static str, raw: i32) -> Result<AfterCheckpoint, Self> {
        Self::enumeration(field, raw, |then| match then {
            daemon::AfterCheckpoint::Resume => Some(AfterCheckpoint::Resume),
            daemon::AfterCheckpoint::Stop => Some(AfterCheckpoint::Stop),
            daemon::AfterCheckpoint::Unspecified => None,
        })
    }
}

impl From<Status> for daemon::MachineStatus {
    fn from(status: Status) -> Self {
        let kind = match status {
            Status::Stopped(next) => {
                daemon::machine_status::Kind::Stopped(daemon::NextStart::from(next) as i32)
            }
            Status::Live(state) => daemon::machine_status::Kind::Live(state.into()),
        };
        Self { kind: Some(kind) }
    }
}

impl TryFrom<daemon::MachineStatus> for Status {
    type Error = ParseError;

    fn try_from(status: daemon::MachineStatus) -> Result<Self, ParseError> {
        match status.kind {
            None => Err(ParseError::Missing("machine_status.kind")),
            // A daemon's status carries the run state only.
            Some(daemon::machine_status::Kind::Live(state)) => {
                Ok(Self::Live(run_state(state.run_state)))
            }
            Some(daemon::machine_status::Kind::Stopped(raw)) => Ok(Self::Stopped(
                ParseError::next_start("machine_status.stopped", raw)?,
            )),
        }
    }
}

impl From<Step> for daemon::CheckpointStep {
    fn from(step: Step) -> Self {
        match step {
            Step::SyncingGuest => Self::SyncingGuest,
            Step::Pausing => Self::Pausing,
            Step::Flushing => Self::Flushing,
            Step::Cloning => Self::Cloning,
            Step::SavingMemory => Self::SavingMemory,
            Step::Committing => Self::Committing,
            Step::Finishing => Self::Finishing,
        }
    }
}

impl From<GuestAfterFailure> for daemon::GuestAfterFailure {
    fn from(guest: GuestAfterFailure) -> Self {
        match guest {
            GuestAfterFailure::Untouched => Self::Untouched,
            GuestAfterFailure::ResumeRequested => Self::ResumeRequested,
        }
    }
}

impl From<AfterCheckpoint> for daemon::AfterCheckpoint {
    fn from(then: AfterCheckpoint) -> Self {
        match then {
            AfterCheckpoint::Resume => Self::Resume,
            AfterCheckpoint::Stop => Self::Stop,
        }
    }
}

impl From<Certainty> for daemon::Certainty {
    fn from(certainty: Certainty) -> Self {
        match certainty {
            Certainty::NotDone => Self::NotDone,
            Certainty::MaybeDone => Self::MaybeDone,
        }
    }
}

impl daemon::Certainty {
    /// Whether a failed call may have taken effect. Unset, or a value this
    /// build doesn't know, reads as [`Certainty::MaybeDone`]: an unknown
    /// outcome is never rounded up to "not done".
    #[must_use]
    pub const fn or_maybe_done(self) -> Certainty {
        match self {
            Self::NotDone => Certainty::NotDone,
            Self::MaybeDone | Self::Unspecified => Certainty::MaybeDone,
        }
    }
}

// --- Lineage, as the daemon tells it ----------------------------------

/// A template's agent as `GoldenRole.agent_build` carries it, in the bundle
/// and on the wire alike: empty when not recorded.
pub(crate) fn agent_build(agent: &TemplateAgent) -> String {
    match agent {
        TemplateAgent::Recorded(build) => build.as_str().to_owned(),
        TemplateAgent::Unrecorded => String::new(),
    }
}

/// The inverse of [`agent_build`]. A build id this build can't read is an
/// error, never read as unrecorded.
pub(crate) fn template_agent(raw: &str) -> Result<TemplateAgent, ParseError> {
    match raw {
        "" => Ok(TemplateAgent::Unrecorded),
        raw => raw.parse().map(TemplateAgent::Recorded).map_err(
            |error: doppel_desktop::agent::AgentBuildError| ParseError::Invalid {
                field: "lineage.golden.agent_build",
                reason: error.to_string(),
            },
        ),
    }
}

impl From<&Lineage> for daemon::Lineage {
    fn from(lineage: &Lineage) -> Self {
        use daemon::lineage::{Origin as WireOrigin, Role as WireRole};
        let role = match &lineage.role {
            Role::Working => WireRole::Working(daemon::Done {}),
            Role::Golden { template, agent } => WireRole::Golden(daemon::GoldenRole {
                checkpoint: template.to_string(),
                agent_build: agent_build(agent),
            }),
            Role::Temporary => WireRole::Temporary(daemon::Done {}),
        };
        let origin = match &lineage.origin {
            Origin::Unrecorded => WireOrigin::Unrecorded(daemon::Done {}),
            Origin::Installed => WireOrigin::Installed(daemon::Done {}),
            Origin::Imported => WireOrigin::Imported(daemon::Done {}),
            Origin::Forked { from, checkpoint } => WireOrigin::Forked(daemon::ForkedFrom {
                machine: from.to_string(),
                checkpoint: checkpoint.to_string(),
            }),
        };
        Self {
            role: Some(role),
            origin: Some(origin),
        }
    }
}

impl TryFrom<&daemon::Lineage> for Lineage {
    type Error = ParseError;

    /// A role or origin this build doesn't know (a newer writer's) is an
    /// error, never read as working or unrecorded.
    fn try_from(wire: &daemon::Lineage) -> Result<Self, ParseError> {
        use daemon::lineage::{Origin as WireOrigin, Role as WireRole};
        let label = |field: &'static str, raw: &str| {
            raw.parse::<CheckpointLabel>()
                .map_err(|error| ParseError::Invalid {
                    field,
                    reason: error.to_string(),
                })
        };
        let role = match &wire.role {
            Some(WireRole::Working(_)) => Role::Working,
            Some(WireRole::Golden(golden)) => Role::Golden {
                template: label("lineage.golden.checkpoint", &golden.checkpoint)?,
                agent: template_agent(&golden.agent_build)?,
            },
            Some(WireRole::Temporary(_)) => Role::Temporary,
            None => return Err(ParseError::Missing("lineage.role")),
        };
        let origin = match &wire.origin {
            Some(WireOrigin::Unrecorded(_)) => Origin::Unrecorded,
            Some(WireOrigin::Installed(_)) => Origin::Installed,
            Some(WireOrigin::Imported(_)) => Origin::Imported,
            Some(WireOrigin::Forked(forked)) => Origin::Forked {
                from: forked
                    .machine
                    .parse()
                    .map_err(|error: SpecError| ParseError::Invalid {
                        field: "lineage.forked.machine",
                        reason: error.to_string(),
                    })?,
                checkpoint: label("lineage.forked.checkpoint", &forked.checkpoint)?,
            },
            None => return Err(ParseError::Missing("lineage.origin")),
        };
        Ok(Self { role, origin })
    }
}

impl From<&RecordedLineage> for daemon::machine_entry::RecordedLineage {
    fn from(recorded: &RecordedLineage) -> Self {
        match recorded {
            RecordedLineage::Known(lineage) => Self::Lineage(lineage.into()),
            RecordedLineage::Unreadable(why) => Self::LineageUnreadable(why.clone()),
        }
    }
}

impl daemon::MachineEntry {
    /// The entry's lineage. Infallible: one that doesn't parse is
    /// unreadable, with why; none at all (an older daemon) is unrecorded.
    #[must_use]
    pub fn recorded_lineage(&self) -> RecordedLineage {
        use daemon::machine_entry::RecordedLineage as Wire;
        match &self.recorded_lineage {
            None => RecordedLineage::Known(Lineage::default()),
            Some(Wire::Lineage(lineage)) => match Lineage::try_from(lineage) {
                Ok(lineage) => RecordedLineage::Known(lineage),
                Err(error) => RecordedLineage::Unreadable(error.to_string()),
            },
            Some(Wire::LineageUnreadable(why)) => RecordedLineage::Unreadable(why.clone()),
        }
    }
}

/// A duration as the daemon's protocol carries it: whole microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Micros(u64);

impl Micros {
    /// Below a microsecond is dropped; past `u64::MAX` saturates.
    #[must_use]
    pub fn saturating(duration: std::time::Duration) -> Self {
        Self(u64::try_from(duration.as_micros()).unwrap_or(u64::MAX))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A duration as the wire's `*_ms` fields carry it: whole milliseconds in
/// 32 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Millis(u32);

impl Millis {
    /// Below a millisecond is dropped; past `u32::MAX` saturates.
    #[must_use]
    pub fn saturating(duration: std::time::Duration) -> Self {
        Self(u32::try_from(duration.as_millis()).unwrap_or(u32::MAX))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl daemon::LoginResult {
    /// The desktop, its durations in whole microseconds: below one is
    /// dropped, and past `u64::MAX` saturates.
    #[must_use]
    pub fn in_micros(desktop: &Desktop) -> Self {
        use daemon::login_result::Outcome;
        let outcome = match &desktop.login {
            LoginOutcome::AlreadyActive { after } => {
                Outcome::AlreadyActiveAfterMicros(Micros::saturating(*after).get())
            }
            LoginOutcome::Unlocked { after } => {
                Outcome::UnlockedAfterMicros(Micros::saturating(*after).get())
            }
            LoginOutcome::NeedsPassword => Outcome::NeedsPassword(daemon::Done {}),
            LoginOutcome::PasswordNotAccepted => Outcome::PasswordNotAccepted(daemon::Done {}),
            LoginOutcome::NoAgent { waited, last } => Outcome::NoAgent(daemon::NoAgent {
                waited_micros: Micros::saturating(*waited).get(),
                last: last.clone(),
            }),
            LoginOutcome::Unknown { last } => Outcome::Unknown(last.clone()),
        };
        Self {
            outcome: Some(outcome),
            screen: desktop.screen.as_ref().map(daemon::HostScreen::in_micros),
        }
    }
}

impl TryFrom<daemon::LoginResult> for Desktop {
    type Error = ParseError;

    fn try_from(result: daemon::LoginResult) -> Result<Self, ParseError> {
        use daemon::login_result::Outcome;
        use std::time::Duration;
        let screen = result.screen.map(HostScreen::try_from).transpose()?;
        let login = match result.outcome {
            None => return Err(ParseError::Missing("login_result.outcome")),
            Some(Outcome::AlreadyActiveAfterMicros(after)) => LoginOutcome::AlreadyActive {
                after: Duration::from_micros(after),
            },
            Some(Outcome::UnlockedAfterMicros(after)) => LoginOutcome::Unlocked {
                after: Duration::from_micros(after),
            },
            Some(Outcome::NeedsPassword(_)) => LoginOutcome::NeedsPassword,
            Some(Outcome::PasswordNotAccepted(_)) => LoginOutcome::PasswordNotAccepted,
            Some(Outcome::NoAgent(no_agent)) => LoginOutcome::NoAgent {
                waited: Duration::from_micros(no_agent.waited_micros),
                last: no_agent.last,
            },
            Some(Outcome::Unknown(last)) => LoginOutcome::Unknown { last },
        };
        Ok(Self { login, screen })
    }
}

// --- Screen probe ---------------------------------------------------------------

impl From<Rgb> for common::Color {
    fn from(color: Rgb) -> Self {
        Self {
            red: color.red().into(),
            green: color.green().into(),
            blue: color.blue().into(),
        }
    }
}

impl TryFrom<common::Color> for Rgb {
    type Error = ParseError;

    fn try_from(color: common::Color) -> Result<Self, ParseError> {
        let channel = |value: u32, field| {
            u8::try_from(value).map_err(|_| ParseError::Invalid {
                field,
                reason: format!("{value} is past 255"),
            })
        };
        Ok(Self::new(
            channel(color.red, "color.red")?,
            channel(color.green, "color.green")?,
            channel(color.blue, "color.blue")?,
        ))
    }
}

impl From<Probe> for agent::ShowProbe {
    fn from(probe: Probe) -> Self {
        Self {
            frame: Some(Probe::frame().into()),
            color: Some(probe.color().into()),
        }
    }
}

impl daemon::HostScreen {
    /// The screen's state, its durations in whole microseconds: below one
    /// is dropped, and past `u64::MAX` saturates.
    #[must_use]
    pub fn in_micros(screen: &HostScreen) -> Self {
        use daemon::host_screen::State;
        let micros = |duration| Micros::saturating(duration).get();
        let state = match screen {
            HostScreen::Current { after } => State::CurrentAfterMicros(micros(*after)),
            HostScreen::CurrentWithProbe { after, why } => {
                State::CurrentWithProbe(daemon::CurrentWithProbe {
                    after_micros: micros(*after),
                    why: why.clone(),
                })
            }
            HostScreen::Unconfirmed(WhyUnconfirmed::ProbeUnsupported) => {
                State::ProbeUnsupported(daemon::Done {})
            }
            HostScreen::Unconfirmed(WhyUnconfirmed::NotShown { waited }) => {
                State::NotShownAfterMicros(micros(*waited))
            }
            HostScreen::Unconfirmed(WhyUnconfirmed::Failed(why)) => State::Failed(why.clone()),
        };
        Self { state: Some(state) }
    }
}

impl TryFrom<daemon::HostScreen> for HostScreen {
    type Error = ParseError;

    fn try_from(screen: daemon::HostScreen) -> Result<Self, ParseError> {
        use daemon::host_screen::State;
        use std::time::Duration;
        Ok(match screen.state {
            None => return Err(ParseError::Missing("host_screen.state")),
            Some(State::CurrentAfterMicros(after)) => Self::Current {
                after: Duration::from_micros(after),
            },
            Some(State::CurrentWithProbe(current)) => Self::CurrentWithProbe {
                after: Duration::from_micros(current.after_micros),
                why: current.why,
            },
            Some(State::ProbeUnsupported(_)) => Self::Unconfirmed(WhyUnconfirmed::ProbeUnsupported),
            Some(State::NotShownAfterMicros(waited)) => {
                Self::Unconfirmed(WhyUnconfirmed::NotShown {
                    waited: Duration::from_micros(waited),
                })
            }
            Some(State::Failed(why)) => Self::Unconfirmed(WhyUnconfirmed::Failed(why)),
        })
    }
}

// --- Remote errors ----------------------------------------------------------------

/// An error reported by a peer, parsed from `common.v1.Error`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RemoteError {
    #[error("missing permission in the guest: {0}")]
    PermissionMissing(Permission),
    #[error("invalid argument `{field}`: {reason}")]
    InvalidArgument { field: String, reason: String },
    #[error(
        "element reference from snapshot {snapshot_id} (node {node_index}) is stale; take a new snapshot"
    )]
    StaleReference { snapshot_id: u64, node_index: u32 },
    #[error("timed out after {after_ms} ms")]
    Timeout { after_ms: u32 },
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("unrecognized error from a newer peer")]
    Unrecognized,
}

/// A refusal changed nothing. A peer that gave up part way (its own timeout,
/// an internal error) or one this build can't read may have acted.
impl Failure for RemoteError {
    fn certainty(&self) -> Certainty {
        match self {
            Self::PermissionMissing(_)
            | Self::InvalidArgument { .. }
            | Self::StaleReference { .. }
            | Self::Unsupported(_)
            | Self::InvalidState(_) => Certainty::NotDone,
            Self::Timeout { .. } | Self::Internal(_) | Self::Unrecognized => Certainty::MaybeDone,
        }
    }
}

impl From<common::Error> for RemoteError {
    fn from(error: common::Error) -> Self {
        use common::error::Kind;
        match error.kind {
            Some(Kind::PermissionMissing(p)) => {
                Self::PermissionMissing(ParseError::permission(p.permission))
            }
            Some(Kind::InvalidArgument(e)) => Self::InvalidArgument {
                field: e.field,
                reason: e.reason,
            },
            Some(Kind::StaleReference(e)) => Self::StaleReference {
                snapshot_id: e.snapshot_id,
                node_index: e.node_index,
            },
            Some(Kind::Timeout(e)) => Self::Timeout {
                after_ms: e.after_ms,
            },
            Some(Kind::Unsupported(e)) => Self::Unsupported(e.what),
            Some(Kind::InvalidState(e)) => Self::InvalidState(e.reason),
            Some(Kind::Internal(e)) => Self::Internal(e.message),
            None => Self::Unrecognized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_permission_this_build_doesn_t_know_is_kept() {
        let missing = |permission| common::Error {
            kind: Some(common::error::Kind::PermissionMissing(
                common::PermissionMissing { permission },
            )),
        };
        assert_eq!(
            RemoteError::from(missing(common::Permission::Accessibility as i32)),
            RemoteError::PermissionMissing(Permission::Accessibility)
        );
        assert_eq!(
            RemoteError::from(missing(99)),
            RemoteError::PermissionMissing(Permission::Unrecognized(99))
        );
    }

    #[test]
    fn lineage_round_trips() {
        let golden = Role::Golden {
            template: "ready".parse().unwrap(),
            agent: doppel_machine::lineage::TemplateAgent::Unrecorded,
        };
        let forked = Origin::Forked {
            from: "golden-27".parse().unwrap(),
            checkpoint: "ready".parse().unwrap(),
        };
        for role in [Role::Working, golden, Role::Temporary] {
            for origin in [
                Origin::Unrecorded,
                Origin::Installed,
                Origin::Imported,
                forked.clone(),
            ] {
                let lineage = Lineage {
                    role: role.clone(),
                    origin,
                };
                let wire = daemon::Lineage::from(&lineage);
                assert_eq!(Lineage::try_from(&wire), Ok(lineage));
            }
        }
    }

    #[test]
    fn a_lineage_this_build_cannot_read_is_never_a_default() {
        use daemon::lineage::{Origin as WireOrigin, Role as WireRole};
        let good = daemon::Lineage::from(&Lineage::default());
        let refused = |change: fn(&mut daemon::Lineage)| {
            let mut wire = good.clone();
            change(&mut wire);
            Lineage::try_from(&wire).is_err()
        };
        // A newer writer's role or origin arrives unset.
        assert!(refused(|wire| wire.role = None));
        assert!(refused(|wire| wire.origin = None));
        assert!(refused(|wire| {
            wire.role = Some(WireRole::Golden(daemon::GoldenRole {
                checkpoint: "Not A Label".into(),
                agent_build: String::new(),
            }));
        }));
        assert!(refused(|wire| {
            wire.role = Some(WireRole::Golden(daemon::GoldenRole {
                checkpoint: "ready".into(),
                agent_build: "not a build".into(),
            }));
        }));
        assert!(refused(|wire| {
            wire.origin = Some(WireOrigin::Forked(daemon::ForkedFrom {
                machine: String::new(),
                checkpoint: "ready".into(),
            }));
        }));

        use daemon::machine_entry::RecordedLineage as Wire;
        let entry = |recorded| daemon::MachineEntry {
            name: "dev".into(),
            status: None,
            recorded_lineage: recorded,
        };
        assert!(matches!(
            entry(Some(Wire::Lineage(daemon::Lineage::default()))).recorded_lineage(),
            RecordedLineage::Unreadable(_)
        ));
        assert_eq!(
            entry(Some(Wire::LineageUnreadable("truncated".into()))).recorded_lineage(),
            RecordedLineage::Unreadable("truncated".into())
        );
        // An older daemon sends none: nothing was recorded.
        assert_eq!(
            entry(None).recorded_lineage(),
            RecordedLineage::Known(Lineage::default())
        );
    }

    #[test]
    fn unknown_run_state_is_kept() {
        assert_eq!(run_state(77), RunState::Unrecognized(77));
        assert_eq!(run_state(vmm::VmRunState::Paused as i32), RunState::Paused);
    }

    #[test]
    fn a_vmm_state_says_its_window_when_the_vmm_does() {
        let state = |window: i32| -> (RunState, Option<Window>) {
            let state = vmm::VmState {
                run_state: vmm::VmRunState::Running as i32,
                window,
            };
            (state.state(), state.window_shown())
        };
        assert_eq!(
            state(vmm::WindowMode::Shown as i32),
            (RunState::Running, Some(Window::Shown))
        );
        assert_eq!(
            state(vmm::WindowMode::Hidden as i32),
            (RunState::Running, Some(Window::Hidden))
        );
        // An older VMM doesn't say; a newer one may say what isn't known here.
        assert_eq!(
            state(vmm::WindowMode::Unspecified as i32),
            (RunState::Running, None)
        );
        assert_eq!(state(9), (RunState::Running, None));
    }

    #[test]
    fn machine_statuses_round_trip_and_keep_unknown_run_states() {
        for status in [
            Status::Stopped(NextStart::ColdBoot),
            Status::Stopped(NextStart::ResumeSavedState),
            Status::Live(RunState::Running),
            Status::Live(RunState::Unrecognized(77)),
        ] {
            assert_eq!(
                Status::try_from(daemon::MachineStatus::from(status)),
                Ok(status)
            );
        }
    }

    #[test]
    fn login_outcomes_round_trip() {
        use std::time::Duration;
        for outcome in [
            LoginOutcome::AlreadyActive {
                after: Duration::from_millis(1_500),
            },
            LoginOutcome::Unlocked {
                after: Duration::from_micros(1_119_042),
            },
            LoginOutcome::NeedsPassword,
            LoginOutcome::PasswordNotAccepted,
            LoginOutcome::NoAgent {
                waited: Duration::from_secs(90),
                last: "no session state".into(),
            },
            LoginOutcome::Unknown {
                last: "connection reset".into(),
            },
        ] {
            let desktop = Desktop {
                login: outcome,
                screen: None,
            };
            assert_eq!(
                Desktop::try_from(daemon::LoginResult::in_micros(&desktop)),
                Ok(desktop)
            );
        }
    }

    #[test]
    fn host_screens_round_trip() {
        use doppel_machine::screen_probe::{HostScreen, WhyUnconfirmed};
        use std::time::Duration;
        for screen in [
            HostScreen::Current {
                after: Duration::from_millis(3500),
            },
            HostScreen::CurrentWithProbe {
                after: Duration::from_millis(40),
                why: "hiding it failed: timeout".into(),
            },
            HostScreen::Unconfirmed(WhyUnconfirmed::ProbeUnsupported),
            HostScreen::Unconfirmed(WhyUnconfirmed::NotShown {
                waited: Duration::from_secs(20),
            }),
            HostScreen::Unconfirmed(WhyUnconfirmed::Failed("no VMM".into())),
        ] {
            let desktop = Desktop {
                login: LoginOutcome::AlreadyActive {
                    after: Duration::from_millis(12),
                },
                screen: Some(screen),
            };
            assert_eq!(
                Desktop::try_from(daemon::LoginResult::in_micros(&desktop)),
                Ok(desktop)
            );
        }
    }

    #[test]
    fn colours_past_eight_bits_are_refused() {
        use doppel_machine::screen_probe::Rgb;
        let color = common::Color::from(Rgb::new(255, 0, 9));
        assert_eq!(Rgb::try_from(color), Ok(Rgb::new(255, 0, 9)));
        let too_red = common::Color {
            red: 256,
            green: 0,
            blue: 0,
        };
        assert!(Rgb::try_from(too_red).is_err());
    }

    #[test]
    fn a_certainty_this_build_doesn_t_know_may_have_taken_effect() {
        assert_eq!(
            daemon::Certainty::Unspecified.or_maybe_done(),
            Certainty::MaybeDone
        );
        for certainty in [Certainty::NotDone, Certainty::MaybeDone] {
            assert_eq!(
                daemon::Certainty::from(certainty).or_maybe_done(),
                certainty
            );
        }
    }

    #[test]
    fn lifecycle_enums_round_trip_and_unspecified_is_rejected() {
        for window in [Window::Hidden, Window::Shown] {
            let raw = daemon::Window::from(window) as i32;
            assert_eq!(ParseError::window("w", raw), Ok(window));
        }
        for mode in [StopMode::Graceful, StopMode::Force] {
            let raw = vmm::StopMode::from(mode) as i32;
            assert_eq!(ParseError::stop_mode("m", raw), Ok(mode));
        }
        for outcome in [StopOutcome::Stopped, StopOutcome::ShutdownPending] {
            let raw = daemon::StopOutcome::from(outcome) as i32;
            assert_eq!(ParseError::stop_outcome("o", raw), Ok(outcome));
        }
        for resumed in [Resumed::Resumed, Resumed::AlreadyRunning] {
            let raw = daemon::ResumeOutcome::from(resumed) as i32;
            assert_eq!(ParseError::resumed("r", raw), Ok(resumed));
        }
        let unspecified = ParseError::UnknownEnum {
            field: "w",
            value: 0,
        };
        assert_eq!(ParseError::window("w", 0), Err(unspecified));
        assert!(ParseError::stop_mode("m", 0).is_err());
        assert!(ParseError::stop_outcome("o", 0).is_err());
        assert!(ParseError::resumed("r", 0).is_err());
    }

    #[test]
    fn checkpoint_steps_round_trip_and_unspecified_is_rejected() {
        for step in [
            Step::SyncingGuest,
            Step::Pausing,
            Step::Flushing,
            Step::Cloning,
            Step::SavingMemory,
            Step::Committing,
            Step::Finishing,
        ] {
            assert_eq!(
                ParseError::checkpoint_step("at", daemon::CheckpointStep::from(step) as i32),
                Ok(step)
            );
        }
        assert!(ParseError::checkpoint_step("at", 0).is_err());
        assert!(ParseError::checkpoint_step("at", 99).is_err());
    }

    #[test]
    fn a_machine_status_without_a_kind_or_with_an_unknown_start_is_rejected() {
        assert_eq!(
            Status::try_from(daemon::MachineStatus { kind: None }),
            Err(ParseError::Missing("machine_status.kind"))
        );
        assert_eq!(
            Status::try_from(daemon::MachineStatus {
                kind: Some(daemon::machine_status::Kind::Stopped(9)),
            }),
            Err(ParseError::UnknownEnum {
                field: "machine_status.stopped",
                value: 9
            })
        );
    }
}

// --- Input ------------------------------------------------------------------

mod input_convert;

// --- Agent status and images -----------------------------------------------

mod agent_convert;

// --- Recognized text -------------------------------------------------------------

mod screen_text_convert;

// --- Accessibility --------------------------------------------------------------

mod refs_convert;

mod ax_convert;

// --- Exec -------------------------------------------------------------------------

mod exec_convert;

// --- Files ------------------------------------------------------------------------

mod files_convert;

// --- Waits (daemon) -------------------------------------------------------------

mod wait_convert;

mod mark_convert;

// --- Activity and recordings (daemon) ---------------------------------------------

mod activity_convert;

// --- Films (doppel-scene) -----------------------------------------------------------

mod film_convert;

mod encoded_convert;

// --- Daemon reports -------------------------------------------------------------

mod report_convert;
