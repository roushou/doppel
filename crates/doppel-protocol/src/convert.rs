//! The boundary: protobuf DTOs ⇄ core domain types.
//!
//! Inbound conversions are `TryFrom` (parse, don't validate). Outbound ones
//! are `From` because a valid domain value always has a wire form.

use doppel_machine::agent::AgentReport;
use doppel_machine::agent::Permission;
use doppel_machine::ax::tree::{SnapshotScope, SnapshotSpec};
use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::checkpoint::{AfterCheckpoint, GuestAfterFailure, Step};
use doppel_machine::input::Via;
use doppel_machine::lineage::{Lineage, Origin, RecordedLineage, Role};
use doppel_machine::login::LoginOutcome;
use doppel_machine::machine::SpecError;
use doppel_machine::outcome::{Certainty, Failure};
use doppel_machine::vm::{MachineReport, NextStart, RunState, Status, Window};

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
    ScreenText(#[from] doppel_machine::screen_text::ScreenTextError),
    #[error(transparent)]
    Mark(#[from] doppel_machine::mark::MarkError),
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

/// What a VMM says about its VM: how it runs, and how it's shown when it
/// says (`None` from a VMM that doesn't, or with a value unknown here).
impl From<vmm::VmState> for (RunState, Option<Window>) {
    fn from(state: vmm::VmState) -> Self {
        let window = match vmm::WindowMode::try_from(state.window) {
            Ok(vmm::WindowMode::Hidden) => Some(Window::Hidden),
            Ok(vmm::WindowMode::Shown) => Some(Window::Shown),
            Ok(vmm::WindowMode::Unspecified) | Err(_) => None,
        };
        (run_state(state.run_state), window)
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

impl From<daemon::Certainty> for Certainty {
    /// Infallible: a value this build doesn't know may have taken effect,
    /// never the reverse.
    fn from(certainty: daemon::Certainty) -> Self {
        match certainty {
            daemon::Certainty::NotDone => Self::NotDone,
            daemon::Certainty::MaybeDone | daemon::Certainty::Unspecified => Self::MaybeDone,
        }
    }
}

// --- Lineage, as the daemon tells it ----------------------------------

impl From<&Lineage> for daemon::Lineage {
    fn from(lineage: &Lineage) -> Self {
        use daemon::lineage::{Origin as WireOrigin, Role as WireRole};
        let role = match &lineage.role {
            Role::Working => WireRole::Working(daemon::Done {}),
            Role::Golden { template } => WireRole::Golden(daemon::GoldenRole {
                checkpoint: template.to_string(),
            }),
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
            },
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

impl From<&LoginOutcome> for daemon::LoginResult {
    fn from(outcome: &LoginOutcome) -> Self {
        use daemon::login_result::Outcome;
        let outcome = match outcome {
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
        }
    }
}

impl TryFrom<daemon::LoginResult> for LoginOutcome {
    type Error = ParseError;

    fn try_from(result: daemon::LoginResult) -> Result<Self, ParseError> {
        use daemon::login_result::Outcome;
        use std::time::Duration;
        Ok(match result.outcome {
            None => return Err(ParseError::Missing("login_result.outcome")),
            Some(Outcome::AlreadyActiveAfterMicros(after)) => Self::AlreadyActive {
                after: Duration::from_micros(after),
            },
            Some(Outcome::UnlockedAfterMicros(after)) => Self::Unlocked {
                after: Duration::from_micros(after),
            },
            Some(Outcome::NeedsPassword(_)) => Self::NeedsPassword,
            Some(Outcome::PasswordNotAccepted(_)) => Self::PasswordNotAccepted,
            Some(Outcome::NoAgent(no_agent)) => Self::NoAgent {
                waited: Duration::from_micros(no_agent.waited_micros),
                last: no_agent.last,
            },
            Some(Outcome::Unknown(last)) => Self::Unknown { last },
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
        };
        let forked = Origin::Forked {
            from: "golden-27".parse().unwrap(),
            checkpoint: "ready".parse().unwrap(),
        };
        for role in [Role::Working, golden] {
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
            vmm::VmState {
                run_state: vmm::VmRunState::Running as i32,
                window,
            }
            .into()
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
            assert_eq!(
                LoginOutcome::try_from(daemon::LoginResult::from(&outcome)),
                Ok(outcome)
            );
        }
    }

    #[test]
    fn a_certainty_this_build_doesn_t_know_may_have_taken_effect() {
        assert_eq!(
            Certainty::from(daemon::Certainty::Unspecified),
            Certainty::MaybeDone
        );
        assert_eq!(
            Certainty::from(daemon::Certainty::from(Certainty::NotDone)),
            Certainty::NotDone
        );
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

mod input_convert {
    use doppel_machine::geometry::Point;
    use doppel_machine::input::{
        Button, ClickCount, Input, Key, KeyChord, Modifier, NamedKey, Pointer, PointerPhase,
    };

    use crate::pb::common::v1 as common;

    impl From<Point> for common::Point {
        fn from(p: Point) -> Self {
            Self { x: p.x(), y: p.y() }
        }
    }

    impl From<NamedKey> for common::NamedKey {
        fn from(key: NamedKey) -> Self {
            match key {
                NamedKey::Return => common::NamedKey::Return,
                NamedKey::Tab => common::NamedKey::Tab,
                NamedKey::Space => common::NamedKey::Space,
                NamedKey::Delete => common::NamedKey::Delete,
                NamedKey::ForwardDelete => common::NamedKey::ForwardDelete,
                NamedKey::Escape => common::NamedKey::Escape,
                NamedKey::Left => common::NamedKey::Left,
                NamedKey::Right => common::NamedKey::Right,
                NamedKey::Up => common::NamedKey::Up,
                NamedKey::Down => common::NamedKey::Down,
                NamedKey::Home => common::NamedKey::Home,
                NamedKey::End => common::NamedKey::End,
                NamedKey::PageUp => common::NamedKey::PageUp,
                NamedKey::PageDown => common::NamedKey::PageDown,
                // F1..F12 are contiguous on the wire (21..=32); the core guarantees 1..=12.
                NamedKey::F(n) => common::NamedKey::try_from(20 + i32::from(n))
                    .unwrap_or(common::NamedKey::Unspecified),
            }
        }
    }

    impl From<Modifier> for common::Modifier {
        fn from(modifier: Modifier) -> Self {
            match modifier {
                Modifier::Command => Self::Command,
                Modifier::Shift => Self::Shift,
                Modifier::Option => Self::Option,
                Modifier::Control => Self::Control,
            }
        }
    }

    impl From<Button> for common::MouseButton {
        fn from(button: Button) -> Self {
            match button {
                Button::Left => Self::Left,
                Button::Right => Self::Right,
            }
        }
    }

    impl From<&Pointer> for common::Pointer {
        fn from(pointer: &Pointer) -> Self {
            let (phase, button, click_count) = match pointer.phase {
                PointerPhase::Press(button, count) => {
                    (common::PointerPhase::Press, Some(button), count.get())
                }
                PointerPhase::Release(button, count) => {
                    (common::PointerPhase::Release, Some(button), count.get())
                }
                PointerPhase::Move(held) => (common::PointerPhase::Move, held, 0),
            };
            Self {
                phase: phase as i32,
                at: Some(pointer.at.into()),
                button: button.map_or(common::MouseButton::Unspecified, common::MouseButton::from)
                    as i32,
                click_count,
                modifiers: pointer
                    .modifiers
                    .iter()
                    .map(|m| common::Modifier::from(*m) as i32)
                    .collect(),
            }
        }
    }

    impl From<&KeyChord> for common::KeyChord {
        fn from(chord: &KeyChord) -> Self {
            let modifiers = chord
                .modifiers
                .iter()
                .map(|m| common::Modifier::from(*m) as i32)
                .collect();
            let key = match chord.key {
                Key::Named(key) => common::key::Key::Named(common::NamedKey::from(key) as i32),
                Key::Character(c) => common::key::Key::Character(c.to_string()),
            };
            Self {
                modifiers,
                key: Some(common::Key { key: Some(key) }),
            }
        }
    }

    impl From<&Input> for common::Input {
        fn from(input: &Input) -> Self {
            use common::input::Action;
            let action = match input {
                Input::Click { at, button, count } => Action::Click(common::Click {
                    at: Some((*at).into()),
                    button: common::MouseButton::from(*button) as i32,
                    count: count.get(),
                }),
                Input::MoveTo(at) => Action::MoveTo((*at).into()),
                Input::Drag { from, to } => Action::Drag(common::Drag {
                    from: Some((*from).into()),
                    to: Some((*to).into()),
                    button: common::MouseButton::Left as i32,
                }),
                Input::Scroll { at, dx, dy } => Action::Scroll(common::Scroll {
                    at: Some((*at).into()),
                    dx: *dx,
                    dy: *dy,
                }),
                Input::Key(chord) => Action::Key(chord.into()),
                Input::Type(text) => Action::TypeText(common::TypeText { text: text.clone() }),
                Input::Pointer(pointer) => Action::Pointer(pointer.into()),
            };
            Self {
                action: Some(action),
            }
        }
    }

    use super::ParseError;

    impl TryFrom<common::Point> for Point {
        type Error = ParseError;
        fn try_from(p: common::Point) -> Result<Self, ParseError> {
            Self::new(p.x, p.y).map_err(|error| ParseError::input("point", &error))
        }
    }

    impl ParseError {
        fn input(field: &'static str, error: &dyn std::fmt::Display) -> Self {
            Self::Invalid {
                field,
                reason: error.to_string(),
            }
        }

        fn count(field: &'static str, raw: u32) -> Result<ClickCount, Self> {
            raw.try_into().map_err(|error| Self::input(field, &error))
        }

        fn point(field: &'static str, point: Option<common::Point>) -> Result<Point, Self> {
            point.ok_or(Self::Missing(field))?.try_into()
        }

        fn named_key(raw: i32) -> Result<NamedKey, Self> {
            let unknown = Self::UnknownEnum {
                field: "key.named",
                value: raw,
            };
            Ok(
                match common::NamedKey::try_from(raw).map_err(|_| unknown.clone())? {
                    common::NamedKey::Return => NamedKey::Return,
                    common::NamedKey::Tab => NamedKey::Tab,
                    common::NamedKey::Space => NamedKey::Space,
                    common::NamedKey::Delete => NamedKey::Delete,
                    common::NamedKey::ForwardDelete => NamedKey::ForwardDelete,
                    common::NamedKey::Escape => NamedKey::Escape,
                    common::NamedKey::Left => NamedKey::Left,
                    common::NamedKey::Right => NamedKey::Right,
                    common::NamedKey::Up => NamedKey::Up,
                    common::NamedKey::Down => NamedKey::Down,
                    common::NamedKey::Home => NamedKey::Home,
                    common::NamedKey::End => NamedKey::End,
                    common::NamedKey::PageUp => NamedKey::PageUp,
                    common::NamedKey::PageDown => NamedKey::PageDown,
                    f @ (common::NamedKey::F1
                    | common::NamedKey::F2
                    | common::NamedKey::F3
                    | common::NamedKey::F4
                    | common::NamedKey::F5
                    | common::NamedKey::F6
                    | common::NamedKey::F7
                    | common::NamedKey::F8
                    | common::NamedKey::F9
                    | common::NamedKey::F10
                    | common::NamedKey::F11
                    | common::NamedKey::F12) => {
                        // Contiguous on the wire (21..=32).
                        NamedKey::F(u8::try_from(f as i32 - 20).map_err(|_| unknown)?)
                    }
                    common::NamedKey::Unspecified => return Err(unknown),
                },
            )
        }

        fn modifiers(raw: &[i32]) -> Result<Vec<Modifier>, Self> {
            raw.iter()
                .map(|&raw| match common::Modifier::try_from(raw) {
                    Ok(common::Modifier::Command) => Ok(Modifier::Command),
                    Ok(common::Modifier::Shift) => Ok(Modifier::Shift),
                    Ok(common::Modifier::Option) => Ok(Modifier::Option),
                    Ok(common::Modifier::Control) => Ok(Modifier::Control),
                    Ok(common::Modifier::Function | common::Modifier::Unspecified) | Err(_) => {
                        Err(Self::UnknownEnum {
                            field: "modifiers",
                            value: raw,
                        })
                    }
                })
                .collect()
        }

        fn button(field: &'static str, raw: i32) -> Result<Option<Button>, Self> {
            match common::MouseButton::try_from(raw) {
                Ok(common::MouseButton::Left) => Ok(Some(Button::Left)),
                Ok(common::MouseButton::Right) => Ok(Some(Button::Right)),
                Ok(common::MouseButton::Unspecified) => Ok(None),
                // On the wire; no button this build sends.
                Ok(common::MouseButton::Middle) | Err(_) => {
                    Err(Self::UnknownEnum { field, value: raw })
                }
            }
        }

        fn pressed(field: &'static str, raw: i32) -> Result<Button, Self> {
            Self::button(field, raw)?.ok_or(Self::Missing(field))
        }
    }

    impl TryFrom<common::KeyChord> for KeyChord {
        type Error = ParseError;
        fn try_from(chord: common::KeyChord) -> Result<Self, ParseError> {
            let key = match chord.key.and_then(|key| key.key) {
                None => return Err(ParseError::Missing("key")),
                Some(common::key::Key::Named(raw)) => Key::Named(ParseError::named_key(raw)?),
                Some(common::key::Key::VirtualKeyCode(code)) => {
                    return Err(ParseError::Invalid {
                        field: "key.virtual_key_code",
                        reason: format!("{code}: this build only sends named keys and characters"),
                    });
                }
                Some(common::key::Key::Character(text)) => {
                    let mut chars = text.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => Key::Character(c),
                        (None | Some(_), _) => {
                            return Err(ParseError::Invalid {
                                field: "key.character",
                                reason: format!("{text:?} is not one character"),
                            });
                        }
                    }
                }
            };
            Ok(Self {
                modifiers: ParseError::modifiers(&chord.modifiers)?,
                key,
            })
        }
    }

    impl TryFrom<common::Pointer> for Pointer {
        type Error = ParseError;
        fn try_from(pointer: common::Pointer) -> Result<Self, ParseError> {
            let phase = match common::PointerPhase::try_from(pointer.phase) {
                Ok(common::PointerPhase::Press) => PointerPhase::Press(
                    ParseError::pressed("pointer.button", pointer.button)?,
                    ParseError::count("pointer.click_count", pointer.click_count)?,
                ),
                Ok(common::PointerPhase::Release) => PointerPhase::Release(
                    ParseError::pressed("pointer.button", pointer.button)?,
                    ParseError::count("pointer.click_count", pointer.click_count)?,
                ),
                Ok(common::PointerPhase::Move) => {
                    PointerPhase::Move(ParseError::button("pointer.button", pointer.button)?)
                }
                Ok(common::PointerPhase::Unspecified) | Err(_) => {
                    return Err(ParseError::UnknownEnum {
                        field: "pointer.phase",
                        value: pointer.phase,
                    });
                }
            };
            Ok(Self {
                phase,
                at: ParseError::point("pointer.at", pointer.at)?,
                modifiers: ParseError::modifiers(&pointer.modifiers)?,
            })
        }
    }

    impl TryFrom<common::Input> for Input {
        type Error = ParseError;
        fn try_from(input: common::Input) -> Result<Self, ParseError> {
            use common::input::Action;
            Ok(
                match input.action.ok_or(ParseError::Missing("input.action"))? {
                    Action::Click(click) => Self::Click {
                        at: ParseError::point("click.at", click.at)?,
                        button: ParseError::pressed("click.button", click.button)?,
                        count: ParseError::count("click.count", click.count)?,
                    },
                    Action::MoveTo(at) => Self::MoveTo(at.try_into()?),
                    // Drags are with the left button.
                    Action::Drag(drag) => Self::Drag {
                        from: ParseError::point("drag.from", drag.from)?,
                        to: ParseError::point("drag.to", drag.to)?,
                    },
                    Action::Scroll(scroll) => Self::Scroll {
                        at: ParseError::point("scroll.at", scroll.at)?,
                        dx: scroll.dx,
                        dy: scroll.dy,
                    },
                    Action::Key(chord) => Self::Key(chord.try_into()?),
                    Action::TypeText(text) => Self::Type(text.text),
                    Action::Pointer(pointer) => Self::Pointer(pointer.try_into()?),
                },
            )
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn every_input_round_trips() {
            let at = Point::new(10.5, 20.0).unwrap();
            let inputs = [
                Input::Click {
                    at,
                    button: Button::Right,
                    count: ClickCount::Double,
                },
                Input::MoveTo(at),
                Input::Drag {
                    from: at,
                    to: Point::new(1.0, 2.0).unwrap(),
                },
                Input::Scroll {
                    at,
                    dx: -3.0,
                    dy: 4.5,
                },
                Input::Key("cmd+shift+f12".parse().unwrap()),
                Input::Key("ctrl+é".parse().unwrap()),
                Input::Type("Grüße ✓ 🦊".into()),
                Input::Pointer(Pointer {
                    phase: PointerPhase::Press(Button::Left, ClickCount::Triple),
                    at,
                    modifiers: vec![Modifier::Option],
                }),
                Input::Pointer(Pointer {
                    phase: PointerPhase::Move(None),
                    at,
                    modifiers: Vec::new(),
                }),
                Input::Pointer(Pointer {
                    phase: PointerPhase::Release(Button::Right, ClickCount::Single),
                    at,
                    modifiers: Vec::new(),
                }),
            ];
            for input in inputs {
                let wire = common::Input::from(&input);
                assert_eq!(Input::try_from(wire), Ok(input));
            }
        }

        #[test]
        fn malformed_input_is_refused() {
            let click = |button| common::Input {
                action: Some(common::input::Action::Click(common::Click {
                    at: Some(common::Point { x: 1.0, y: 1.0 }),
                    button,
                    count: 1,
                })),
            };
            assert_eq!(
                Input::try_from(click(common::MouseButton::Unspecified as i32)),
                Err(ParseError::Missing("click.button"))
            );
            assert!(Input::try_from(click(99)).is_err());
            assert_eq!(
                Input::try_from(common::Input::default()),
                Err(ParseError::Missing("input.action"))
            );
        }

        #[test]
        fn a_pointer_carries_a_button_only_where_it_has_one() {
            use doppel_machine::input::ClickCount;
            let at = Point::new(3.0, 4.0).unwrap();
            let press = common::Pointer::from(&Pointer {
                phase: PointerPhase::Press(Button::Right, ClickCount::Double),
                at,
                modifiers: vec![Modifier::Command],
            });
            assert_eq!(press.phase, common::PointerPhase::Press as i32);
            assert_eq!(press.button, common::MouseButton::Right as i32);
            assert_eq!(press.click_count, 2);
            assert_eq!(press.modifiers, vec![common::Modifier::Command as i32]);
            let hover = common::Pointer::from(&Pointer {
                phase: PointerPhase::Move(None),
                at,
                modifiers: Vec::new(),
            });
            assert_eq!(hover.phase, common::PointerPhase::Move as i32);
            assert_eq!(hover.button, common::MouseButton::Unspecified as i32);
            assert_eq!(hover.click_count, 0);
        }

        #[test]
        fn every_named_key_survives_the_wire() {
            let keys = [
                NamedKey::Return,
                NamedKey::Tab,
                NamedKey::Space,
                NamedKey::Delete,
                NamedKey::ForwardDelete,
                NamedKey::Escape,
                NamedKey::Left,
                NamedKey::Right,
                NamedKey::Up,
                NamedKey::Down,
                NamedKey::Home,
                NamedKey::End,
                NamedKey::PageUp,
                NamedKey::PageDown,
                NamedKey::F(1),
                NamedKey::F(12),
            ];
            for key in keys {
                let wire = common::NamedKey::from(key) as i32;
                assert_eq!(ParseError::named_key(wire), Ok(key), "{key:?}");
            }
        }

        #[test]
        fn function_keys_map_onto_the_wire_range() {
            for n in 1..=12u8 {
                assert_ne!(
                    common::NamedKey::from(NamedKey::F(n)),
                    common::NamedKey::Unspecified,
                    "F{n}"
                );
            }
            assert_eq!(common::NamedKey::from(NamedKey::F(1)), common::NamedKey::F1);
            assert_eq!(
                common::NamedKey::from(NamedKey::F(12)),
                common::NamedKey::F12
            );
        }
    }
}

// --- Agent status and images -----------------------------------------------

mod agent_convert {
    use doppel_machine::agent::{
        AgentStatus, BundleId, GuestEvent, Permission, ProcessId, RunningApp, Session,
    };
    use doppel_machine::geometry::Size;
    use doppel_machine::screen::{ImageFormat, Screenshot};

    use super::ParseError;
    use crate::pb::agent::v1 as agent;
    use crate::pb::common::v1 as common;

    /// A display's size in points: both sides longer than zero.
    impl TryFrom<common::Size> for Size {
        type Error = ParseError;
        fn try_from(s: common::Size) -> Result<Self, ParseError> {
            Self::new(s.width, s.height)
                .ok()
                .filter(|size| size.width() > 0.0 && size.height() > 0.0)
                .ok_or_else(|| ParseError::Invalid {
                    field: "display_points",
                    reason: "not a positive size".into(),
                })
        }
    }

    impl TryFrom<agent::AgentStatus> for AgentStatus {
        type Error = ParseError;
        fn try_from(wire: agent::AgentStatus) -> Result<Self, ParseError> {
            let session = match agent::SessionState::try_from(wire.session) {
                Ok(agent::SessionState::Active) => Session::Active,
                Ok(agent::SessionState::Locked) => Session::Locked,
                Ok(agent::SessionState::Unspecified) => Session::Unreported,
                Err(_) => Session::Unrecognized(wire.session),
            };
            Ok(Self {
                granted: Granted(wire.granted).into(),
                session,
                display: wire
                    .display_points
                    .ok_or(ParseError::Missing("display_points"))?
                    .try_into()?,
                frontmost: wire.frontmost_app.map(RunningApp::from),
            })
        }
    }

    /// Permissions as the wire lists them; unknown values are kept.
    struct Granted(Vec<i32>);

    impl From<Granted> for Vec<Permission> {
        fn from(Granted(raw): Granted) -> Self {
            raw.into_iter().map(ParseError::permission).collect()
        }
    }

    impl From<agent::AgentEvent> for GuestEvent {
        /// Infallible: an event this build can't read is kept as unrecognized.
        fn from(event: agent::AgentEvent) -> Self {
            use agent::agent_event::Kind;
            match event.kind {
                Some(Kind::FrontmostAppChanged(changed)) => {
                    Self::FrontmostAppChanged(changed.app.map(RunningApp::from))
                }
                Some(Kind::UiChanged(changed)) => Self::UiChanged(ProcessId::new(changed.pid)),
                Some(Kind::PermissionsChanged(changed)) => {
                    Self::PermissionsChanged(Granted(changed.granted).into())
                }
                None => Self::Unrecognized,
            }
        }
    }

    impl From<agent::RunningApp> for RunningApp {
        /// Infallible: an empty bundle id means the process has none.
        fn from(app: agent::RunningApp) -> Self {
            Self {
                pid: ProcessId::new(app.pid),
                bundle_id: BundleId::new(app.bundle_id),
                name: app.name,
            }
        }
    }

    impl TryFrom<common::Image> for Screenshot {
        type Error = ParseError;
        fn try_from(wire: common::Image) -> Result<Self, ParseError> {
            let format = match common::ImageEncoding::try_from(wire.encoding) {
                Ok(common::ImageEncoding::Png) => ImageFormat::Png,
                Ok(common::ImageEncoding::Jpeg) => ImageFormat::Jpeg,
                Ok(common::ImageEncoding::Unspecified) | Err(_) => {
                    return Err(ParseError::Invalid {
                        field: "encoding",
                        reason: format!("unknown encoding {}", wire.encoding),
                    });
                }
            };
            if wire.data.is_empty() || wire.width_pixels == 0 || wire.height_pixels == 0 {
                return Err(ParseError::Invalid {
                    field: "data",
                    reason: "empty image".into(),
                });
            }
            Ok(Self {
                format,
                width_pixels: wire.width_pixels,
                height_pixels: wire.height_pixels,
                display: wire
                    .display_points
                    .ok_or(ParseError::Missing("display_points"))?
                    .try_into()?,
                data: wire.data.to_vec(),
            })
        }
    }

    impl From<Size> for common::Size {
        fn from(display: Size) -> Self {
            Self {
                width: display.width(),
                height: display.height(),
            }
        }
    }

    impl From<&RunningApp> for agent::RunningApp {
        fn from(app: &RunningApp) -> Self {
            Self {
                pid: app.pid.get(),
                bundle_id: app
                    .bundle_id
                    .as_ref()
                    .map_or_else(String::new, |id| id.as_str().to_owned()),
                name: app.name.clone(),
            }
        }
    }

    impl From<&AgentStatus> for agent::AgentStatus {
        fn from(status: &AgentStatus) -> Self {
            Self {
                granted: status
                    .granted
                    .iter()
                    .map(|permission| match permission {
                        Permission::ScreenRecording => common::Permission::ScreenRecording as i32,
                        Permission::Accessibility => common::Permission::Accessibility as i32,
                        Permission::Unrecognized(raw) => *raw,
                    })
                    .collect(),
                session: match status.session {
                    Session::Active => agent::SessionState::Active as i32,
                    Session::Locked => agent::SessionState::Locked as i32,
                    Session::Unreported => agent::SessionState::Unspecified as i32,
                    Session::Unrecognized(raw) => raw,
                },
                display_points: Some(status.display.into()),
                frontmost_app: status.frontmost.as_ref().map(agent::RunningApp::from),
            }
        }
    }

    impl From<&Screenshot> for common::Image {
        fn from(shot: &Screenshot) -> Self {
            Self {
                encoding: match shot.format {
                    ImageFormat::Png => common::ImageEncoding::Png,
                    ImageFormat::Jpeg => common::ImageEncoding::Jpeg,
                } as i32,
                width_pixels: shot.width_pixels,
                height_pixels: shot.height_pixels,
                display_points: Some(shot.display.into()),
                data: shot.data.clone().into(),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn events_keep_what_this_build_cant_read() {
            use agent::agent_event::Kind;
            let event = |kind| GuestEvent::from(agent::AgentEvent { kind });
            assert_eq!(
                event(Some(Kind::UiChanged(agent::UiChanged { pid: 612 }))),
                GuestEvent::UiChanged(ProcessId::new(612))
            );
            assert_eq!(
                event(Some(Kind::FrontmostAppChanged(
                    agent::FrontmostAppChanged { app: None }
                ))),
                GuestEvent::FrontmostAppChanged(None)
            );
            assert_eq!(
                event(Some(Kind::PermissionsChanged(agent::PermissionsChanged {
                    granted: vec![42],
                }))),
                GuestEvent::PermissionsChanged(vec![Permission::Unrecognized(42)])
            );
            // A kind from a newer agent decodes to no kind at all.
            assert_eq!(event(None), GuestEvent::Unrecognized);
        }

        #[test]
        fn status_keeps_unknown_values() {
            let status = AgentStatus::try_from(agent::AgentStatus {
                granted: vec![common::Permission::Accessibility as i32, 42],
                session: 9,
                display_points: Some(common::Size {
                    width: 1280.0,
                    height: 800.0,
                }),
                frontmost_app: Some(agent::RunningApp {
                    pid: 7,
                    bundle_id: String::new(),
                    name: "x".into(),
                }),
            })
            .unwrap();
            assert_eq!(
                status.granted,
                vec![Permission::Accessibility, Permission::Unrecognized(42)]
            );
            assert_eq!(status.session, Session::Unrecognized(9));
            assert_eq!(
                AgentStatus::try_from(agent::AgentStatus::from(&status)),
                Ok(status.clone()),
                "unknown values survive a round trip"
            );
            assert_eq!(status.frontmost.unwrap().bundle_id, None);
        }

        #[test]
        fn screenshots_round_trip() {
            let shot = Screenshot {
                format: ImageFormat::Jpeg,
                width_pixels: 2,
                height_pixels: 1,
                display: Size::new(1280.0, 800.0).unwrap(),
                data: vec![1, 2, 3],
            };
            assert_eq!(Screenshot::try_from(common::Image::from(&shot)), Ok(shot));
        }

        #[test]
        fn images_must_be_complete() {
            let good = common::Image {
                encoding: common::ImageEncoding::Png as i32,
                width_pixels: 2,
                height_pixels: 1,
                display_points: Some(common::Size {
                    width: 1.0,
                    height: 0.5,
                }),
                data: vec![1].into(),
            };
            assert!(Screenshot::try_from(good.clone()).is_ok());
            assert!(
                Screenshot::try_from(common::Image {
                    data: Default::default(),
                    ..good.clone()
                })
                .is_err()
            );
            assert!(
                Screenshot::try_from(common::Image {
                    encoding: 0,
                    ..good.clone()
                })
                .is_err()
            );
            assert!(
                Screenshot::try_from(common::Image {
                    display_points: None,
                    ..good
                })
                .is_err()
            );
        }
    }
}

// --- Recognized text -------------------------------------------------------------

mod screen_text_convert {
    use doppel_machine::geometry::Rect;
    use doppel_machine::geometry::Size;
    use doppel_machine::screen_text::{Confidence, ScreenText, TextLine};

    use super::ParseError;
    use crate::pb::common::v1 as common;
    use crate::pb::vmm::v1 as vmm;

    impl TryFrom<common::Rect> for Rect {
        type Error = ParseError;
        fn try_from(rect: common::Rect) -> Result<Self, ParseError> {
            let origin = rect.origin.ok_or(ParseError::Missing("frame.origin"))?;
            let size = rect.size.ok_or(ParseError::Missing("frame.size"))?;
            Self::new(origin.x, origin.y, size.width, size.height).map_err(|error| {
                ParseError::Invalid {
                    field: "frame",
                    reason: error.to_string(),
                }
            })
        }
    }

    impl TryFrom<vmm::TextLine> for TextLine {
        type Error = ParseError;
        fn try_from(line: vmm::TextLine) -> Result<Self, ParseError> {
            let words = line
                .words
                .into_iter()
                .map(|word| {
                    let frame = word.frame.ok_or(ParseError::Missing("words.frame"))?;
                    Ok((word.text, frame.try_into()?))
                })
                .collect::<Result<_, ParseError>>()?;
            Ok(Self::new(
                line.text,
                line.frame.ok_or(ParseError::Missing("frame"))?.try_into()?,
                Confidence::new(line.confidence)?,
                words,
            )?)
        }
    }

    impl TryFrom<vmm::RecognizedText> for ScreenText {
        type Error = ParseError;
        fn try_from(wire: vmm::RecognizedText) -> Result<Self, ParseError> {
            let display: Size = wire
                .display_points
                .ok_or(ParseError::Missing("display_points"))?
                .try_into()?;
            let lines = wire
                .lines
                .into_iter()
                .map(TextLine::try_from)
                .collect::<Result<_, _>>()?;
            Ok(Self::new(display, lines))
        }
    }

    impl From<Rect> for common::Rect {
        fn from(rect: Rect) -> Self {
            Self {
                origin: Some(common::Point {
                    x: rect.left(),
                    y: rect.top(),
                }),
                size: Some(common::Size {
                    width: rect.width(),
                    height: rect.height(),
                }),
            }
        }
    }

    impl From<&ScreenText> for vmm::RecognizedText {
        fn from(text: &ScreenText) -> Self {
            Self {
                display_points: Some(text.display().into()),
                lines: text
                    .lines()
                    .iter()
                    .map(|line| vmm::TextLine {
                        text: line.text().to_owned(),
                        frame: Some(line.region().into()),
                        confidence: line.confidence().get(),
                        words: line
                            .words()
                            .map(|(word, region)| vmm::TextWord {
                                text: word.to_owned(),
                                frame: Some(region.into()),
                            })
                            .collect(),
                    })
                    .collect(),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn rect(x: f64, y: f64, width: f64, height: f64) -> Option<common::Rect> {
            Some(common::Rect {
                origin: Some(common::Point { x, y }),
                size: Some(common::Size { width, height }),
            })
        }

        fn wire(words: Vec<vmm::TextWord>, confidence: f32) -> vmm::RecognizedText {
            vmm::RecognizedText {
                display_points: Some(common::Size {
                    width: 1280.0,
                    height: 800.0,
                }),
                lines: vec![vmm::TextLine {
                    text: "Cancel Save".into(),
                    frame: rect(10.0, 20.0, 110.0, 20.0),
                    confidence,
                    words,
                }],
            }
        }

        #[test]
        fn recognized_text_round_trips() {
            let text = ScreenText::try_from(wire(
                vec![
                    vmm::TextWord {
                        text: "Cancel".into(),
                        frame: rect(10.0, 20.0, 50.0, 12.0),
                    },
                    vmm::TextWord {
                        text: "Save".into(),
                        frame: rect(64.0, 20.0, 50.0, 12.0),
                    },
                ],
                0.75,
            ))
            .unwrap();
            assert_eq!(
                ScreenText::try_from(vmm::RecognizedText::from(&text)),
                Ok(text)
            );
        }

        #[test]
        fn word_frames_carry_through_to_matches() {
            let text = ScreenText::try_from(wire(
                vec![
                    vmm::TextWord {
                        text: "Cancel".into(),
                        frame: rect(10.0, 20.0, 60.0, 20.0),
                    },
                    vmm::TextWord {
                        text: "Save".into(),
                        frame: rect(80.0, 20.0, 40.0, 20.0),
                    },
                ],
                0.9,
            ))
            .unwrap();
            let found = text.find(&"save".parse().unwrap());
            assert_eq!(
                (found[0].region.center().x(), found[0].region.center().y()),
                (100.0, 30.0)
            );
        }

        #[test]
        fn malformed_recognition_is_refused() {
            assert!(ScreenText::try_from(wire(vec![], 1.5)).is_err());
            let stray = vmm::TextWord {
                text: "Open".into(),
                frame: rect(0.0, 0.0, 1.0, 1.0),
            };
            assert!(ScreenText::try_from(wire(vec![stray], 0.9)).is_err());
            let mut no_frame = wire(vec![], 0.9);
            no_frame.lines[0].frame = None;
            assert!(ScreenText::try_from(no_frame).is_err());
        }
    }
}

// --- Accessibility --------------------------------------------------------------

mod refs_convert {
    use doppel_machine::ax::refs::{Generation, SnapshotRefs};
    use doppel_machine::ax::tree::{NodeIndex, SnapshotId};

    use super::ParseError;
    use crate::pb::daemon::v1 as daemon;

    impl TryFrom<daemon::ElementRefs> for SnapshotRefs {
        type Error = ParseError;
        fn try_from(wire: daemon::ElementRefs) -> Result<Self, ParseError> {
            let generation = Generation::new(wire.generation).ok_or(ParseError::Invalid {
                field: "generation",
                reason: "generations count from 1".into(),
            })?;
            Ok(Self::new(
                SnapshotId::new(wire.snapshot),
                generation,
                wire.nodes.into_iter().map(NodeIndex::new).collect(),
            ))
        }
    }

    impl From<&SnapshotRefs> for daemon::ElementRefs {
        fn from(refs: &SnapshotRefs) -> Self {
            Self {
                snapshot: refs.snapshot().get(),
                generation: refs.generation().get(),
                nodes: refs.nodes().iter().map(|node| node.get()).collect(),
            }
        }
    }
}

mod ax_convert {
    use doppel_machine::agent::RunningApp;
    use doppel_machine::ax::tree::{
        ActOn, ActionName, AxNode, AxSnapshot, AxText, AxTree, AxValue, ElementAction,
        ElementTarget, Menus, NodeIndex, Retention, Role, Scope, SnapshotId, SnapshotRequest,
        States, Subrole, Truncation,
    };
    use doppel_machine::geometry::Rect;

    use super::ParseError;
    use crate::pb::agent::v1 as agent;

    impl TryFrom<agent::AxSnapshot> for AxSnapshot {
        type Error = ParseError;
        fn try_from(wire: agent::AxSnapshot) -> Result<Self, ParseError> {
            let nodes = wire
                .nodes
                .into_iter()
                .map(|n| (n.parent.map(NodeIndex::new), WireNode(n).parse()))
                .collect();
            let tree = AxTree::new(nodes).map_err(|e| ParseError::Invalid {
                field: "nodes",
                reason: e.to_string(),
            })?;
            let truncation = match agent::Truncation::try_from(wire.truncation) {
                Ok(agent::Truncation::Complete) => Truncation::Complete,
                Ok(agent::Truncation::NodeBudget) => Truncation::NodeBudget,
                Ok(agent::Truncation::DepthBudget) => Truncation::DepthBudget,
                Ok(agent::Truncation::Deadline) => Truncation::Deadline,
                Ok(agent::Truncation::Unspecified) | Err(_) => {
                    Truncation::Unreported(wire.truncation)
                }
            };
            Ok(Self {
                id: SnapshotId::new(wire.snapshot_id),
                app: RunningApp::from(wire.app.ok_or(ParseError::Missing("app"))?),
                tree,
                truncation,
            })
        }
    }

    /// One node as the agent sent it.
    struct WireNode(agent::AxNode);

    impl WireNode {
        /// Lenient per node: an odd frame or value is dropped, not fatal, since
        /// apps report all sorts. The tree's shape is what must be right.
        fn parse(self) -> AxNode {
            let wire = self.0;
            let value = wire.value.and_then(|v| v.value).and_then(|v| match v {
                common_value::Value::Text(t) => Some(AxValue::Text(t)),
                common_value::Value::Number(n) if n.is_finite() => Some(AxValue::Number(n)),
                common_value::Value::Number(_) => None,
                common_value::Value::Boolean(b) => Some(AxValue::Boolean(b)),
            });
            // A frame that isn't a rectangle (not finite, or a negative
            // size) is dropped like any other odd one.
            let frame = wire.frame.and_then(|r| Rect::try_from(r).ok());
            let flags = wire.flags.unwrap_or_default();
            AxNode {
                role: Role::parse(&wire.role),
                subrole: Subrole::parse(&wire.subrole),
                title: AxText::new(wire.title),
                description: AxText::new(wire.description),
                identifier: AxText::new(wire.identifier),
                value,
                frame,
                states: States {
                    enabled: flags.enabled,
                    focused: flags.focused,
                    selected: flags.selected,
                    expanded: flags.expanded,
                },
                actions: wire.actions.iter().map(|a| ActionName::parse(a)).collect(),
            }
        }
    }

    use agent::ax_value as common_value;

    impl From<&AxSnapshot> for agent::AxSnapshot {
        fn from(snapshot: &AxSnapshot) -> Self {
            let tree = &snapshot.tree;
            Self {
                snapshot_id: snapshot.id.get(),
                app: Some((&snapshot.app).into()),
                nodes: tree
                    .iter()
                    .map(|(index, node)| WireNode::encode(tree.parent(index), node))
                    .collect(),
                truncation: match snapshot.truncation {
                    Truncation::Complete => agent::Truncation::Complete as i32,
                    Truncation::NodeBudget => agent::Truncation::NodeBudget as i32,
                    Truncation::DepthBudget => agent::Truncation::DepthBudget as i32,
                    Truncation::Deadline => agent::Truncation::Deadline as i32,
                    Truncation::Unreported(raw) => raw,
                },
            }
        }
    }

    impl WireNode {
        /// An element's text as the wire carries it: none is empty.
        fn text(text: Option<&AxText>) -> String {
            text.map_or_else(String::new, |text| text.as_str().to_owned())
        }

        fn encode(parent: Option<NodeIndex>, node: &AxNode) -> agent::AxNode {
            agent::AxNode {
                parent: parent.map(NodeIndex::get),
                role: node.role.as_ax().to_owned(),
                subrole: node
                    .subrole
                    .as_ref()
                    .map(|subrole| subrole.as_ax().to_owned())
                    .unwrap_or_default(),
                title: Self::text(node.title.as_ref()),
                description: Self::text(node.description.as_ref()),
                identifier: Self::text(node.identifier.as_ref()),
                value: node.value.as_ref().map(|value| agent::AxValue {
                    value: Some(match value {
                        AxValue::Text(text) => common_value::Value::Text(text.clone()),
                        AxValue::Number(n) => common_value::Value::Number(*n),
                        AxValue::Boolean(b) => common_value::Value::Boolean(*b),
                    }),
                }),
                frame: node.frame.map(Into::into),
                flags: Some(agent::AxFlags {
                    enabled: node.states.enabled,
                    focused: node.states.focused,
                    selected: node.states.selected,
                    expanded: node.states.expanded,
                }),
                actions: node
                    .actions
                    .iter()
                    .map(|action| action.as_ax().to_owned())
                    .collect(),
            }
        }
    }

    impl TryFrom<agent::PerformAxAction> for ActOn {
        type Error = ParseError;
        fn try_from(wire: agent::PerformAxAction) -> Result<Self, ParseError> {
            use agent::perform_ax_action::Action;
            let action = match wire.action.ok_or(ParseError::Missing("action"))? {
                Action::Press(_) => ElementAction::Perform(ActionName::Press),
                Action::ShowMenu(_) => ElementAction::Perform(ActionName::ShowMenu),
                Action::Raise(_) => ElementAction::Perform(ActionName::Raise),
                Action::Named(name) => ElementAction::Perform(ActionName::parse(&name)),
                Action::Focus(_) => ElementAction::Focus,
                Action::Select(_) => ElementAction::Select,
                Action::SetValue(text) => ElementAction::SetValue(text),
            };
            Ok(Self {
                target: ElementTarget {
                    snapshot: SnapshotId::new(wire.snapshot_id),
                    node: NodeIndex::new(wire.node_index),
                },
                action,
            })
        }
    }

    impl ParseError {
        /// A menu policy; unspecified reads as leaving menus out.
        pub fn menus(field: &'static str, raw: i32) -> Result<Menus, Self> {
            match agent::MenuPolicy::try_from(raw) {
                Ok(agent::MenuPolicy::Exclude | agent::MenuPolicy::Unspecified) => {
                    Ok(Menus::Exclude)
                }
                Ok(agent::MenuPolicy::Include) => Ok(Menus::Include),
                Err(_) => Err(Self::UnknownEnum { field, value: raw }),
            }
        }
    }

    impl From<Menus> for agent::MenuPolicy {
        fn from(menus: Menus) -> Self {
            match menus {
                Menus::Exclude => Self::Exclude,
                Menus::Include => Self::Include,
            }
        }
    }

    impl From<&SnapshotRequest> for agent::TakeAxSnapshot {
        fn from(request: &SnapshotRequest) -> Self {
            Self {
                scope: Some(match request.scope {
                    Scope::FocusedWindow => {
                        agent::take_ax_snapshot::Scope::FocusedWindow(agent::Empty {})
                    }
                    Scope::App(pid) => agent::take_ax_snapshot::Scope::ApplicationPid(pid.get()),
                }),
                max_nodes: request.max_nodes,
                max_depth: request.max_depth,
                deadline_ms: u32::try_from(request.deadline.as_millis()).unwrap_or(u32::MAX),
                menus: match request.menus {
                    Menus::Exclude => agent::MenuPolicy::Exclude,
                    Menus::Include => agent::MenuPolicy::Include,
                } as i32,
                retention: match request.retention {
                    Retention::KeptForActions => agent::Retention::KeptForActions,
                    Retention::Observation => agent::Retention::Observation,
                } as i32,
            }
        }
    }

    impl From<&ActOn> for agent::PerformAxAction {
        fn from(act: &ActOn) -> Self {
            use agent::perform_ax_action::Action;
            let action = match &act.action {
                ElementAction::Perform(ActionName::Press) => Action::Press(agent::Empty {}),
                ElementAction::Perform(ActionName::ShowMenu) => Action::ShowMenu(agent::Empty {}),
                ElementAction::Perform(ActionName::Raise) => Action::Raise(agent::Empty {}),
                ElementAction::Perform(
                    other @ (ActionName::Increment
                    | ActionName::Decrement
                    | ActionName::Confirm
                    | ActionName::Cancel
                    | ActionName::Pick
                    | ActionName::ScrollToVisible
                    | ActionName::ShowAlternateUi
                    | ActionName::ShowDefaultUi
                    | ActionName::Other(_)),
                ) => Action::Named(other.as_ax().to_owned()),
                ElementAction::Focus => Action::Focus(agent::Empty {}),
                ElementAction::Select => Action::Select(agent::Empty {}),
                ElementAction::SetValue(text) => Action::SetValue(text.clone()),
            };
            Self {
                snapshot_id: act.target.snapshot.get(),
                node_index: act.target.node.get(),
                action: Some(action),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::pb::common::v1 as common;

        fn wire_node(parent: Option<u32>, role: &str) -> agent::AxNode {
            agent::AxNode {
                parent,
                role: role.into(),
                ..Default::default()
            }
        }

        fn wire(nodes: Vec<agent::AxNode>) -> agent::AxSnapshot {
            agent::AxSnapshot {
                snapshot_id: 9,
                app: Some(agent::RunningApp {
                    pid: 3,
                    bundle_id: String::new(),
                    name: "App".into(),
                }),
                nodes,
                truncation: agent::Truncation::Complete as i32,
            }
        }

        #[test]
        fn snapshots_parse_into_typed_trees() {
            let mut button = wire_node(Some(0), "AXButton");
            button.title = "OK".into();
            button.description = String::new();
            button.actions = vec!["AXPress".into(), "AXCustom".into()];
            button.value = Some(agent::AxValue {
                value: Some(common_value::Value::Number(f64::NAN)),
            });
            button.frame = Some(common::Rect {
                origin: Some(common::Point { x: 1.0, y: 2.0 }),
                size: Some(common::Size {
                    width: 3.0,
                    height: f64::INFINITY,
                }),
            });
            button.flags = Some(agent::AxFlags {
                enabled: Some(false),
                ..Default::default()
            });
            let snap =
                AxSnapshot::try_from(wire(vec![wire_node(None, "AXWindow"), button])).unwrap();
            assert_eq!(snap.id, SnapshotId::new(9));
            assert_eq!(snap.app.bundle_id, None);
            let ok = snap.tree.get(NodeIndex::new(1)).unwrap();
            assert_eq!(ok.role, Role::Button);
            assert_eq!(ok.title.as_deref(), Some("OK"));
            assert_eq!(ok.description, None, "empty strings are absent");
            assert_eq!(ok.value, None, "NaN is dropped");
            assert_eq!(ok.frame, None, "an infinite size is dropped");
            assert_eq!(ok.states.enabled, Some(false));
            assert_eq!(ok.states.focused, None);
            assert_eq!(
                ok.actions,
                [ActionName::Press, ActionName::parse("AXCustom")]
            );
        }

        #[test]
        fn snapshots_round_trip() {
            let mut button = wire_node(Some(0), "AXButton");
            button.subrole = "AXCloseButton".into();
            button.title = "Save".into();
            button.actions = vec!["AXPress".into(), "AXCustom".into()];
            button.flags = Some(agent::AxFlags {
                enabled: Some(true),
                focused: None,
                selected: Some(false),
                expanded: None,
            });
            button.frame = Some(common::Rect {
                origin: Some(common::Point { x: 1.0, y: 2.0 }),
                size: Some(common::Size {
                    width: 3.0,
                    height: 4.0,
                }),
            });
            let mut text = wire_node(Some(0), "AXSomethingNew");
            text.value = Some(agent::AxValue {
                value: Some(common_value::Value::Text("hi".into())),
            });
            let snapshot =
                AxSnapshot::try_from(wire(vec![wire_node(None, "AXWindow"), button, text]))
                    .unwrap();
            let again = AxSnapshot::try_from(agent::AxSnapshot::from(&snapshot)).unwrap();
            assert_eq!(again, snapshot);
        }

        #[test]
        fn actions_parse_back() {
            let act = |action| ActOn {
                target: ElementTarget {
                    snapshot: SnapshotId::new(4),
                    node: NodeIndex::new(2),
                },
                action,
            };
            for action in [
                ElementAction::Perform(ActionName::Press),
                ElementAction::Perform(ActionName::ShowMenu),
                ElementAction::Perform(ActionName::Raise),
                ElementAction::Perform(ActionName::Increment),
                ElementAction::Perform(ActionName::parse("AXCustom")),
                ElementAction::Focus,
                ElementAction::Select,
                ElementAction::SetValue("v".into()),
            ] {
                let act = act(action);
                assert_eq!(ActOn::try_from(agent::PerformAxAction::from(&act)), Ok(act));
            }
        }

        #[test]
        fn bad_shapes_and_missing_apps_are_rejected() {
            let orphan = wire(vec![
                wire_node(None, "AXWindow"),
                wire_node(Some(5), "AXButton"),
            ]);
            assert!(matches!(
                AxSnapshot::try_from(orphan),
                Err(ParseError::Invalid { field: "nodes", .. })
            ));
            assert!(AxSnapshot::try_from(wire(Vec::new())).is_err());
            let mut no_app = wire(vec![wire_node(None, "AXWindow")]);
            no_app.app = None;
            assert_eq!(
                AxSnapshot::try_from(no_app),
                Err(ParseError::Missing("app"))
            );
            let mut odd = wire(vec![wire_node(None, "AXWindow")]);
            odd.truncation = 42;
            assert_eq!(
                AxSnapshot::try_from(odd).unwrap().truncation,
                Truncation::Unreported(42)
            );
        }

        #[test]
        fn actions_map_onto_the_wire() {
            use agent::perform_ax_action::Action;
            let target = doppel_machine::ax::tree::ElementTarget {
                snapshot: SnapshotId::new(5),
                node: NodeIndex::new(2),
            };
            let wire = |action| agent::PerformAxAction::from(&ActOn { target, action }).action;
            assert_eq!(
                wire(ElementAction::Perform(ActionName::Press)),
                Some(Action::Press(agent::Empty {}))
            );
            assert_eq!(
                wire(ElementAction::Perform(ActionName::Increment)),
                Some(Action::Named("AXIncrement".into()))
            );
            assert_eq!(
                wire(ElementAction::SetValue("é".into())),
                Some(Action::SetValue("é".into()))
            );
        }
    }
}

// --- Exec -------------------------------------------------------------------------

mod exec_convert {
    use std::time::Duration;

    use doppel_machine::exec::{Completeness, ExecOutput, ExecRequest, Termination};

    use super::ParseError;
    use crate::pb::agent::v1 as agent;

    impl From<&ExecRequest> for agent::Exec {
        fn from(request: &ExecRequest) -> Self {
            Self {
                program: request.program().as_str().to_owned(),
                arguments: request.arguments().to_vec(),
                stdin: request.stdin().to_vec().into(),
                timeout_ms: u32::try_from(request.timeout().as_millis()).unwrap_or(u32::MAX),
                max_output_bytes: request.max_output(),
                working_directory: request
                    .working_directory()
                    .map(|d| d.as_str().to_owned())
                    .unwrap_or_default(),
            }
        }
    }

    impl TryFrom<agent::ExecResult> for ExecOutput {
        type Error = ParseError;
        fn try_from(wire: agent::ExecResult) -> Result<Self, ParseError> {
            use agent::exec_result::Termination as Wire;
            let termination = match wire.termination.ok_or(ParseError::Missing("termination"))? {
                Wire::ExitCode(code) => Termination::Exited(code),
                Wire::Signal(signal) => Termination::Signaled(signal),
                Wire::TimedOut(_) => Termination::TimedOut,
            };
            let completeness = match agent::OutputCompleteness::try_from(wire.completeness) {
                Ok(agent::OutputCompleteness::Full) => Completeness::Full,
                Ok(agent::OutputCompleteness::Truncated) => Completeness::Truncated,
                Ok(agent::OutputCompleteness::Unspecified) | Err(_) => {
                    Completeness::Unreported(wire.completeness)
                }
            };
            Ok(Self {
                termination,
                stdout: wire.stdout.to_vec(),
                stderr: wire.stderr.to_vec(),
                completeness,
            })
        }
    }

    impl From<&ExecOutput> for agent::ExecResult {
        fn from(output: &ExecOutput) -> Self {
            use agent::exec_result::Termination as Wire;
            Self {
                termination: Some(match output.termination {
                    Termination::Exited(code) => Wire::ExitCode(code),
                    Termination::Signaled(signal) => Wire::Signal(signal),
                    Termination::TimedOut => Wire::TimedOut(agent::Empty {}),
                }),
                stdout: output.stdout.clone().into(),
                stderr: output.stderr.clone().into(),
                completeness: match output.completeness {
                    Completeness::Full => agent::OutputCompleteness::Full as i32,
                    Completeness::Truncated => agent::OutputCompleteness::Truncated as i32,
                    Completeness::Unreported(raw) => raw,
                },
            }
        }
    }

    impl TryFrom<agent::Exec> for ExecRequest {
        type Error = ParseError;
        fn try_from(wire: agent::Exec) -> Result<Self, ParseError> {
            let invalid = |field, error: &dyn std::fmt::Display| ParseError::Invalid {
                field,
                reason: error.to_string(),
            };
            let request = Self::new(
                wire.program
                    .parse()
                    .map_err(|error| invalid("program", &error))?,
                wire.arguments,
                Duration::from_millis(u64::from(wire.timeout_ms)),
                wire.max_output_bytes,
            )
            .map_err(|error| invalid("exec", &error))?
            .with_stdin(wire.stdin.to_vec());
            Ok(if wire.working_directory.is_empty() {
                request
            } else {
                request.in_directory(
                    wire.working_directory
                        .parse()
                        .map_err(|error| invalid("working_directory", &error))?,
                )
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn requests_and_results_round_trip() {
            let request = ExecRequest::new(
                "/bin/echo".parse().unwrap(),
                vec!["hi".into(), "there".into()],
                Duration::from_millis(1500),
                100,
            )
            .unwrap()
            .with_stdin(b"in".to_vec());
            assert_eq!(
                ExecRequest::try_from(agent::Exec::from(&request)),
                Ok(request.clone())
            );
            let elsewhere = request.in_directory("/tmp".parse().unwrap());
            assert_eq!(
                ExecRequest::try_from(agent::Exec::from(&elsewhere)),
                Ok(elsewhere)
            );
            for output in [
                ExecOutput {
                    termination: Termination::Exited(3),
                    stdout: b"out".to_vec(),
                    stderr: b"err".to_vec(),
                    completeness: Completeness::Full,
                },
                ExecOutput {
                    termination: Termination::Signaled(9),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    completeness: Completeness::Unreported(7),
                },
            ] {
                assert_eq!(
                    ExecOutput::try_from(agent::ExecResult::from(&output)),
                    Ok(output)
                );
            }
        }

        #[test]
        fn requests_and_results_cross_the_wire() {
            let request = ExecRequest::new(
                "/bin/echo".parse().unwrap(),
                vec!["hi".into()],
                Duration::from_millis(1500),
                100,
            )
            .unwrap()
            .with_stdin(b"in".to_vec())
            .in_directory("/tmp".parse().unwrap());
            let wire = agent::Exec::from(&request);
            assert_eq!(
                (
                    wire.program.as_str(),
                    wire.timeout_ms,
                    wire.working_directory.as_str()
                ),
                ("/bin/echo", 1500, "/tmp")
            );
            assert_eq!(&wire.stdin[..], b"in");

            let out = ExecOutput::try_from(agent::ExecResult {
                termination: Some(agent::exec_result::Termination::TimedOut(agent::Empty {})),
                stdout: b"partial".to_vec().into(),
                stderr: Default::default(),
                completeness: agent::OutputCompleteness::Truncated as i32,
            })
            .unwrap();
            assert_eq!(out.termination, Termination::TimedOut);
            assert_eq!(out.completeness, Completeness::Truncated);
            assert_eq!(
                ExecOutput::try_from(agent::ExecResult::default()),
                Err(ParseError::Missing("termination"))
            );
        }
    }
}

// --- Files ------------------------------------------------------------------------

mod files_convert {
    use doppel_machine::files::{FileMode, ReadRequest, WritePolicy, WriteRequest};

    use super::ParseError;
    use crate::pb::agent::v1 as agent;

    impl ParseError {
        fn file(field: &'static str, error: &dyn std::fmt::Display) -> Self {
            Self::Invalid {
                field,
                reason: error.to_string(),
            }
        }
    }

    impl TryFrom<agent::ReadFile> for ReadRequest {
        type Error = ParseError;
        fn try_from(wire: agent::ReadFile) -> Result<Self, ParseError> {
            let path = wire
                .path
                .parse()
                .map_err(|error| ParseError::file("path", &error))?;
            Self::new(path, wire.max_bytes).map_err(|error| ParseError::file("max_bytes", &error))
        }
    }

    impl TryFrom<agent::WriteFile> for WriteRequest {
        type Error = ParseError;
        fn try_from(wire: agent::WriteFile) -> Result<Self, ParseError> {
            let path = wire
                .path
                .parse()
                .map_err(|error| ParseError::file("path", &error))?;
            // 0 means the default, as the agent reads it.
            let mode = FileMode::new(if wire.mode == 0 { 0o644 } else { wire.mode })
                .map_err(|error| ParseError::file("mode", &error))?;
            let policy = match agent::WritePolicy::try_from(wire.policy) {
                Ok(agent::WritePolicy::CreateNew) => WritePolicy::CreateNew,
                Ok(agent::WritePolicy::Replace) => WritePolicy::Replace,
                Ok(agent::WritePolicy::Unspecified) | Err(_) => {
                    return Err(ParseError::UnknownEnum {
                        field: "policy",
                        value: wire.policy,
                    });
                }
            };
            Self::new(path, wire.data.to_vec(), mode, policy)
                .map_err(|error| ParseError::file("data", &error))
        }
    }

    impl From<&ReadRequest> for agent::ReadFile {
        fn from(request: &ReadRequest) -> Self {
            Self {
                path: request.path.as_str().to_owned(),
                max_bytes: request.max_bytes(),
            }
        }
    }

    impl From<&WriteRequest> for agent::WriteFile {
        fn from(request: &WriteRequest) -> Self {
            Self {
                path: request.path.as_str().to_owned(),
                data: request.data().to_vec().into(),
                mode: request.mode.bits(),
                policy: match request.policy {
                    WritePolicy::CreateNew => agent::WritePolicy::CreateNew,
                    WritePolicy::Replace => agent::WritePolicy::Replace,
                } as i32,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn requests_round_trip_and_bad_ones_are_refused() {
            let read = ReadRequest::new("/tmp/a".parse().unwrap(), 10).unwrap();
            assert_eq!(
                ReadRequest::try_from(agent::ReadFile::from(&read)),
                Ok(read)
            );
            let write = WriteRequest::new(
                "/tmp/b".parse().unwrap(),
                b"data".to_vec(),
                FileMode::new(0o755).unwrap(),
                WritePolicy::Replace,
            )
            .unwrap();
            assert_eq!(
                WriteRequest::try_from(agent::WriteFile::from(&write)),
                Ok(write)
            );
            assert!(
                ReadRequest::try_from(agent::ReadFile {
                    path: "relative".into(),
                    max_bytes: 1,
                })
                .is_err()
            );
            assert!(matches!(
                WriteRequest::try_from(agent::WriteFile {
                    path: "/tmp/c".into(),
                    ..Default::default()
                }),
                Err(ParseError::UnknownEnum {
                    field: "policy",
                    ..
                })
            ));
        }
    }
}

// --- Waits (daemon) -------------------------------------------------------------

mod wait_convert {
    use std::time::Duration;

    use doppel_machine::ax::tree::AxSnapshot;
    use doppel_machine::screen_text::ScreenText;
    use doppel_machine::wait::{Condition, TextCondition, WaitOutcome, WaitSpec};

    use super::{Micros, ParseError};
    use crate::pb::daemon::v1 as daemon;

    impl From<&Condition> for daemon::wait_for_element::Condition {
        fn from(condition: &Condition) -> Self {
            match condition {
                Condition::Appears(text) => Self::Appears(text.clone()),
                Condition::Disappears(text) => Self::Disappears(text.clone()),
                Condition::Frontmost(app) => Self::Frontmost(app.clone()),
            }
        }
    }

    impl From<&TextCondition> for daemon::wait_for_text::Condition {
        fn from(condition: &TextCondition) -> Self {
            match condition {
                TextCondition::Appears(query) => Self::Appears(query.as_str().to_owned()),
                TextCondition::Disappears(query) => Self::Disappears(query.as_str().to_owned()),
            }
        }
    }

    /// How a wait ended, apart from what it saw.
    enum End {
        Satisfied(daemon::WaitSatisfied),
        TimedOut(daemon::WaitTimedOut),
        Failed(daemon::WaitFailed),
    }

    impl End {
        fn of<S>(outcome: &WaitOutcome<S>) -> (Self, Option<&S>) {
            match outcome {
                WaitOutcome::Satisfied {
                    seen,
                    elapsed,
                    polls,
                } => (
                    Self::Satisfied(daemon::WaitSatisfied {
                        elapsed_micros: Micros::saturating(*elapsed).get(),
                        polls: *polls,
                    }),
                    Some(seen),
                ),
                WaitOutcome::TimedOut {
                    last,
                    last_error,
                    elapsed,
                    polls,
                } => (
                    Self::TimedOut(daemon::WaitTimedOut {
                        last_error: last_error.clone(),
                        elapsed_micros: Micros::saturating(*elapsed).get(),
                        polls: *polls,
                    }),
                    last.as_ref(),
                ),
                WaitOutcome::Failed { reason, polls } => (
                    Self::Failed(daemon::WaitFailed {
                        reason: reason.clone(),
                        polls: *polls,
                    }),
                    None,
                ),
            }
        }

        /// Pairs the end with what was seen: required when satisfied,
        /// absent when failed.
        fn outcome<S>(self, seen: Option<S>) -> Result<WaitOutcome<S>, ParseError> {
            Ok(match self {
                Self::Satisfied(satisfied) => WaitOutcome::Satisfied {
                    seen: seen.ok_or(ParseError::Missing("seen"))?,
                    elapsed: Duration::from_micros(satisfied.elapsed_micros),
                    polls: satisfied.polls,
                },
                Self::TimedOut(timed_out) => WaitOutcome::TimedOut {
                    last: seen,
                    last_error: timed_out.last_error,
                    elapsed: Duration::from_micros(timed_out.elapsed_micros),
                    polls: timed_out.polls,
                },
                Self::Failed(failed) => match seen {
                    None => WaitOutcome::Failed {
                        reason: failed.reason,
                        polls: failed.polls,
                    },
                    Some(_) => {
                        return Err(ParseError::Invalid {
                            field: "seen",
                            reason: "a failed wait saw nothing".into(),
                        });
                    }
                },
            })
        }
    }

    impl From<&WaitOutcome<AxSnapshot>> for daemon::ElementWaitResult {
        fn from(outcome: &WaitOutcome<AxSnapshot>) -> Self {
            use daemon::element_wait_result::End as Wire;
            let (end, seen) = End::of(outcome);
            Self {
                end: Some(match end {
                    End::Satisfied(end) => Wire::Satisfied(end),
                    End::TimedOut(end) => Wire::TimedOut(end),
                    End::Failed(end) => Wire::Failed(end),
                }),
                seen: seen.map(Into::into),
            }
        }
    }

    impl TryFrom<daemon::ElementWaitResult> for WaitOutcome<AxSnapshot> {
        type Error = ParseError;
        fn try_from(wire: daemon::ElementWaitResult) -> Result<Self, ParseError> {
            use daemon::element_wait_result::End as Wire;
            let end = match wire.end.ok_or(ParseError::Missing("end"))? {
                Wire::Satisfied(end) => End::Satisfied(end),
                Wire::TimedOut(end) => End::TimedOut(end),
                Wire::Failed(end) => End::Failed(end),
            };
            end.outcome(wire.seen.map(AxSnapshot::try_from).transpose()?)
        }
    }

    impl From<&WaitOutcome<ScreenText>> for daemon::TextWaitResult {
        fn from(outcome: &WaitOutcome<ScreenText>) -> Self {
            use daemon::text_wait_result::End as Wire;
            let (end, seen) = End::of(outcome);
            Self {
                end: Some(match end {
                    End::Satisfied(end) => Wire::Satisfied(end),
                    End::TimedOut(end) => Wire::TimedOut(end),
                    End::Failed(end) => Wire::Failed(end),
                }),
                seen: seen.map(Into::into),
            }
        }
    }

    impl TryFrom<daemon::TextWaitResult> for WaitOutcome<ScreenText> {
        type Error = ParseError;
        fn try_from(wire: daemon::TextWaitResult) -> Result<Self, ParseError> {
            use daemon::text_wait_result::End as Wire;
            let end = match wire.end.ok_or(ParseError::Missing("end"))? {
                Wire::Satisfied(end) => End::Satisfied(end),
                Wire::TimedOut(end) => End::TimedOut(end),
                Wire::Failed(end) => End::Failed(end),
            };
            end.outcome(wire.seen.map(ScreenText::try_from).transpose()?)
        }
    }

    impl ParseError {
        fn timeout(timeout_ms: u32) -> Duration {
            Duration::from_millis(u64::from(timeout_ms))
        }

        fn wait<C>(
            spec: Result<WaitSpec<C>, doppel_machine::wait::WaitError>,
        ) -> Result<WaitSpec<C>, Self> {
            spec.map_err(|error| Self::Invalid {
                field: "wait",
                reason: error.to_string(),
            })
        }

        /// # Errors
        /// No condition, or one the core refuses (empty text, a timeout out
        /// of range).
        pub fn element_wait(
            condition: Option<daemon::wait_for_element::Condition>,
            timeout_ms: u32,
        ) -> Result<WaitSpec<Condition>, Self> {
            use daemon::wait_for_element::Condition as Wire;
            let condition = match condition.ok_or(Self::Missing("condition"))? {
                Wire::Appears(text) => Condition::Appears(text),
                Wire::Disappears(text) => Condition::Disappears(text),
                Wire::Frontmost(app) => Condition::Frontmost(app),
            };
            Self::wait(WaitSpec::new(condition, Self::timeout(timeout_ms)))
        }

        /// # Errors
        /// No condition, empty text, or a timeout out of range.
        pub fn text_wait(
            condition: Option<daemon::wait_for_text::Condition>,
            timeout_ms: u32,
        ) -> Result<WaitSpec<TextCondition>, Self> {
            use daemon::wait_for_text::Condition as Wire;
            let condition = match condition.ok_or(Self::Missing("condition"))? {
                Wire::Appears(text) => TextCondition::Appears(text.parse()?),
                Wire::Disappears(text) => TextCondition::Disappears(text.parse()?),
            };
            Self::wait(WaitSpec::new(condition, Self::timeout(timeout_ms)))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::pb::agent::v1 as agent;

        fn snapshot() -> AxSnapshot {
            AxSnapshot::try_from(agent::AxSnapshot {
                snapshot_id: 3,
                app: Some(agent::RunningApp {
                    pid: 1,
                    bundle_id: "com.apple.finder".into(),
                    name: "Finder".into(),
                }),
                nodes: vec![agent::AxNode {
                    role: "AXWindow".into(),
                    ..Default::default()
                }],
                truncation: agent::Truncation::Complete as i32,
            })
            .unwrap()
        }

        #[test]
        fn wait_outcomes_round_trip() {
            let outcomes = [
                WaitOutcome::Satisfied {
                    seen: snapshot(),
                    elapsed: Duration::from_millis(250),
                    polls: 2,
                },
                WaitOutcome::TimedOut {
                    last: Some(snapshot()),
                    last_error: None,
                    elapsed: Duration::from_secs(5),
                    polls: 9,
                },
                WaitOutcome::TimedOut {
                    last: None,
                    last_error: Some("no window yet".into()),
                    elapsed: Duration::from_secs(5),
                    polls: 9,
                },
                WaitOutcome::Failed {
                    reason: "agent unreachable".into(),
                    polls: 1,
                },
            ];
            for outcome in outcomes {
                let wire = daemon::ElementWaitResult::from(&outcome);
                assert_eq!(WaitOutcome::try_from(wire), Ok(outcome));
            }
            let unseen = daemon::ElementWaitResult {
                end: Some(daemon::element_wait_result::End::Satisfied(
                    daemon::WaitSatisfied::default(),
                )),
                seen: None,
            };
            assert_eq!(
                WaitOutcome::<AxSnapshot>::try_from(unseen),
                Err(ParseError::Missing("seen"))
            );
        }

        #[test]
        fn wait_specs_round_trip_through_their_requests() {
            let spec = WaitSpec::new(
                Condition::Frontmost("Finder".into()),
                Duration::from_millis(1500),
            )
            .unwrap();
            let wire = daemon::wait_for_element::Condition::from(spec.condition());
            assert_eq!(ParseError::element_wait(Some(wire), 1500), Ok(spec));
            let text = WaitSpec::new(
                TextCondition::Disappears("Saving…".parse().unwrap()),
                Duration::from_secs(2),
            )
            .unwrap();
            let wire = daemon::wait_for_text::Condition::from(text.condition());
            assert_eq!(ParseError::text_wait(Some(wire), 2000), Ok(text));
            assert!(ParseError::element_wait(None, 1000).is_err());
            assert!(
                ParseError::element_wait(
                    Some(daemon::wait_for_element::Condition::Appears("x".into())),
                    0
                )
                .is_err(),
                "a zero timeout is refused"
            );
        }
    }
}

mod mark_convert {
    use std::time::Duration;

    use doppel_machine::ax::tree::{ElementTarget, NodeIndex, SnapshotId};
    use doppel_machine::geometry::Point;
    use doppel_machine::mark::{Area, Lasts, Mark, MarkAt, Note, Shape};

    use super::ParseError;
    use crate::pb::common::v1 as common;
    use crate::pb::daemon::v1 as daemon;

    impl daemon::MarkScreen {
        /// Asks for `mark` on machine `name`.
        #[must_use]
        pub fn new(name: String, mark: &Mark) -> Self {
            let (shape, from) = match mark.shape() {
                Shape::Box => (daemon::MarkShape::Box, None),
                Shape::Dot => (daemon::MarkShape::Dot, None),
                Shape::Arrow { from } => (daemon::MarkShape::Arrow, from.map(Into::into)),
            };
            Self {
                name,
                shape: shape.into(),
                at: Some(match mark.at() {
                    MarkAt::Point(at) => daemon::mark_screen::At::Point(at.into()),
                    MarkAt::Area(area) => daemon::mark_screen::At::Area(rect(area)),
                    MarkAt::Element(element) => {
                        daemon::mark_screen::At::Element(daemon::ElementOf {
                            snapshot_id: element.snapshot.get(),
                            node_index: element.node.get(),
                        })
                    }
                }),
                from,
                note: mark
                    .note()
                    .map(|note| note.as_str().to_owned())
                    .unwrap_or_default(),
                lasts: Some(match mark.lasts() {
                    Lasts::For(time) => daemon::mark_screen::Lasts::ForMs(millis(time)),
                    Lasts::UntilActed(time) => {
                        daemon::mark_screen::Lasts::UntilActedMs(millis(time))
                    }
                }),
            }
        }
    }

    fn millis(time: Duration) -> u32 {
        u32::try_from(time.as_millis()).unwrap_or(u32::MAX)
    }

    fn rect(area: Area) -> common::Rect {
        area.rect().into()
    }

    impl TryFrom<&daemon::MarkScreen> for Mark {
        type Error = ParseError;
        fn try_from(wire: &daemon::MarkScreen) -> Result<Self, ParseError> {
            let at = match wire.at.ok_or(ParseError::Missing("at"))? {
                daemon::mark_screen::At::Point(at) => MarkAt::Point(at.try_into()?),
                daemon::mark_screen::At::Area(area) => {
                    let (origin, size) = (
                        area.origin.ok_or(ParseError::Missing("area.origin"))?,
                        area.size.ok_or(ParseError::Missing("area.size"))?,
                    );
                    MarkAt::Area(Area::new(origin.x, origin.y, size.width, size.height)?)
                }
                daemon::mark_screen::At::Element(element) => MarkAt::Element(ElementTarget {
                    snapshot: SnapshotId::new(element.snapshot_id),
                    node: NodeIndex::new(element.node_index),
                }),
            };
            let from = wire.from.map(Point::try_from).transpose()?;
            let unknown = ParseError::UnknownEnum {
                field: "shape",
                value: wire.shape,
            };
            let shape =
                match daemon::MarkShape::try_from(wire.shape).map_err(|_| unknown.clone())? {
                    daemon::MarkShape::Box => Shape::Box,
                    daemon::MarkShape::Dot => Shape::Dot,
                    daemon::MarkShape::Arrow => Shape::Arrow { from },
                    daemon::MarkShape::Unspecified => return Err(unknown),
                };
            if from.is_some() && !matches!(shape, Shape::Arrow { .. }) {
                return Err(ParseError::Invalid {
                    field: "from",
                    reason: "only an arrow points from somewhere".into(),
                });
            }
            let lasts = match wire.lasts.ok_or(ParseError::Missing("lasts"))? {
                daemon::mark_screen::Lasts::ForMs(ms) => {
                    Lasts::For(Duration::from_millis(ms.into()))
                }
                daemon::mark_screen::Lasts::UntilActedMs(ms) => {
                    Lasts::UntilActed(Duration::from_millis(ms.into()))
                }
            };
            Ok(Self::new(at, shape, Note::new(&wire.note)?, lasts)?)
        }
    }

    #[cfg(test)]
    mod tests {
        use doppel_machine::mark::DEFAULT_LASTS;

        use super::*;

        #[test]
        fn marks_round_trip() {
            let marks = [
                Mark::new(
                    MarkAt::Element(ElementTarget {
                        snapshot: SnapshotId::new(7),
                        node: NodeIndex::new(3),
                    }),
                    Shape::Box,
                    Note::new("this one").unwrap(),
                    Lasts::UntilActed(DEFAULT_LASTS),
                )
                .unwrap(),
                Mark::new(
                    MarkAt::Point(Point::new(4.0, 5.0).unwrap()),
                    Shape::Arrow {
                        from: Some(Point::new(1.0, 2.0).unwrap()),
                    },
                    None,
                    Lasts::For(Duration::from_secs(10)),
                )
                .unwrap(),
                Mark::new(
                    MarkAt::Area(Area::new(1.0, 2.0, 3.0, 4.0).unwrap()),
                    Shape::Dot,
                    None,
                    Lasts::For(Duration::from_secs(1)),
                )
                .unwrap(),
            ];
            for mark in marks {
                let wire = daemon::MarkScreen::new("live".into(), &mark);
                assert_eq!(wire.name, "live");
                assert_eq!(Mark::try_from(&wire).unwrap(), mark);
            }
        }

        #[test]
        fn bad_marks_are_refused() {
            let good = daemon::MarkScreen::new(
                "live".into(),
                &Mark::new(
                    MarkAt::Point(Point::new(4.0, 5.0).unwrap()),
                    Shape::Box,
                    None,
                    Lasts::For(DEFAULT_LASTS),
                )
                .unwrap(),
            );
            let refused = |change: fn(&mut daemon::MarkScreen)| {
                let mut wire = good.clone();
                change(&mut wire);
                Mark::try_from(&wire).is_err()
            };
            assert!(refused(|wire| wire.at = None));
            assert!(refused(|wire| wire.lasts = None));
            assert!(refused(|wire| wire.shape = 0));
            assert!(refused(
                |wire| wire.from = Some(common::Point { x: 1.0, y: 1.0 })
            ));
            assert!(refused(|wire| {
                wire.lasts = Some(daemon::mark_screen::Lasts::ForMs(61_000));
            }));
            assert!(refused(|wire| wire.note = "x".repeat(201)));
        }
    }
}

// --- Activity and recordings (daemon) ---------------------------------------------

mod activity_convert {
    use std::time::Duration;

    use doppel_machine::activity::{
        Activity, CALLOUT_LASTS, Callout, CalloutLasts, CalloutShape, Ended, Ending, Gesture,
        OnScreen, Phase, Target, TextRead,
    };
    use doppel_machine::ax::tree::AxSnapshot;
    use doppel_machine::geometry::{Point, Rect, Size};
    use doppel_machine::recording::{
        Recording, RecordingSummary, RecordingVideo, TraceActor, TraceCall, TraceLine, TraceOutcome,
    };

    use super::ParseError;
    use crate::pb::common::v1 as common;
    use crate::pb::daemon::v1 as daemon;

    fn invalid(field: &'static str, reason: impl ToString) -> ParseError {
        ParseError::Invalid {
            field,
            reason: reason.to_string(),
        }
    }

    /// An event as the daemon sends it. Refused without a phase, with a
    /// machine name that isn't one, or with names a viewer can't read.
    impl TryFrom<daemon::Activity> for Activity {
        type Error = ParseError;
        fn try_from(activity: daemon::Activity) -> Result<Self, ParseError> {
            let phase = match activity.phase.ok_or(ParseError::Missing("phase"))? {
                daemon::activity::Phase::Started(started) => Phase::Started(on_screen(started)),
                daemon::activity::Phase::Ended(ended) => Phase::Ended(ending(ended)),
                daemon::activity::Phase::Recent(recent) => Phase::Recent {
                    acted: on_screen(recent.started.ok_or(ParseError::Missing("started"))?),
                    started_ago: Duration::from_millis(recent.started_ms_ago),
                    ended: recent
                        .ended
                        .zip(recent.ended_ms_ago)
                        .map(|(ended, ago)| (ending(ended), Duration::from_millis(ago))),
                },
                daemon::activity::Phase::Named(refs) => Phase::Named(refs.try_into()?),
            };
            Ok(Self {
                session: activity.session,
                request: activity.request,
                machine: activity
                    .machine
                    .parse()
                    .map_err(|error| invalid("machine", error))?,
                client: activity.client,
                caption: activity.caption,
                phase,
            })
        }
    }

    /// What a call read. Refused without its text, or with text, a query
    /// or a region this build can't read.
    impl TryFrom<daemon::TextRead> for TextRead {
        type Error = ParseError;
        fn try_from(read: daemon::TextRead) -> Result<Self, ParseError> {
            Ok(Self {
                text: read.text.ok_or(ParseError::Missing("text"))?.try_into()?,
                query: match read.query {
                    Some(query) => Some(query.parse().map_err(|error| invalid("query", error))?),
                    None => None,
                },
                found: read
                    .found
                    .into_iter()
                    .map(Rect::try_from)
                    .collect::<Result<_, _>>()?,
            })
        }
    }

    /// How a call ended, and what it saw, as the daemon says it: what can't
    /// be read is left out.
    fn ending(ended: daemon::CallEnded) -> Ended {
        Ended {
            ending: match ended.outcome {
                Some(daemon::call_ended::Outcome::Done(_)) => Ending::Done,
                Some(daemon::call_ended::Outcome::Failed(why)) => Ending::Failed(why),
                Some(daemon::call_ended::Outcome::TimedOut(why)) => Ending::TimedOut(why),
                Some(daemon::call_ended::Outcome::Refused(why)) => Ending::Refused(why),
                None => Ending::Unknown,
            },
            seen: ended
                .seen
                .and_then(|seen| AxSnapshot::try_from(seen).ok())
                .map(Box::new),
            refs: ended.refs.and_then(|refs| refs.try_into().ok()),
            read: ended
                .read
                .and_then(|read| TextRead::try_from(read).ok())
                .map(Box::new),
        }
    }

    /// What a call did on the screen, and where, as the daemon says it. A
    /// mark is on its own place, not the call's target; a place that can't
    /// be read is left out.
    fn on_screen(started: daemon::CallStarted) -> OnScreen {
        let (target, callout) = match started.mark {
            Some(mark) => {
                let target = mark.at.clone().and_then(|at| match at {
                    daemon::screen_mark::At::Point(at) => {
                        target(daemon::call_started::Target::Point(at))
                    }
                    daemon::screen_mark::At::Area(area) => {
                        target(daemon::call_started::Target::Element(area))
                    }
                });
                (target, Some(Box::new(callout(&mark))))
            }
            None => (started.target.and_then(target), None),
        };
        let why = Some(started.why.trim())
            .filter(|why| !why.is_empty())
            .map(ToOwned::to_owned);
        OnScreen {
            gesture: gesture(started.gesture),
            target,
            callout,
            why,
        }
    }

    fn callout(mark: &daemon::ScreenMark) -> Callout {
        use daemon::MarkShape as Wire;
        let from = point(mark.from);
        Callout {
            shape: match Wire::try_from(mark.shape) {
                Ok(Wire::Box) => CalloutShape::Box,
                Ok(Wire::Dot) => CalloutShape::Dot,
                Ok(Wire::Arrow) => CalloutShape::Arrow { from },
                Ok(Wire::Unspecified) | Err(_) => CalloutShape::Unknown(mark.shape),
            },
            note: Some(mark.note.trim())
                .filter(|note| !note.is_empty())
                .map(ToOwned::to_owned),
            lasts: match mark.lasts {
                Some(daemon::screen_mark::Lasts::ForMs(ms)) => {
                    CalloutLasts::For(Duration::from_millis(ms.into()))
                }
                Some(daemon::screen_mark::Lasts::UntilActedMs(ms)) => {
                    CalloutLasts::UntilActed(Duration::from_millis(ms.into()))
                }
                None => CalloutLasts::For(CALLOUT_LASTS),
            },
        }
    }

    fn gesture(raw: i32) -> Gesture {
        use daemon::Gesture as Wire;
        match Wire::try_from(raw) {
            Ok(Wire::Click) => Gesture::Click,
            Ok(Wire::DoubleClick) => Gesture::DoubleClick,
            Ok(Wire::SecondaryClick) => Gesture::SecondaryClick,
            Ok(Wire::Move) => Gesture::Move,
            Ok(Wire::Drag) => Gesture::Drag,
            Ok(Wire::Scroll) => Gesture::Scroll,
            Ok(Wire::Key) => Gesture::Key,
            Ok(Wire::Type) => Gesture::Type,
            Ok(Wire::Act) => Gesture::Act,
            Ok(Wire::Look) => Gesture::Look,
            Ok(Wire::Wait) => Gesture::Wait,
            Ok(Wire::Run) => Gesture::Run,
            Ok(Wire::Lifecycle) => Gesture::Lifecycle,
            Ok(Wire::Checkpoint) => Gesture::Checkpoint,
            Ok(Wire::Rollback) => Gesture::Rollback,
            Ok(Wire::Mark) => Gesture::Mark,
            Ok(Wire::Other | Wire::Unspecified) => Gesture::Other,
            Err(_) => Gesture::Unknown(raw),
        }
    }

    /// `None` for a target without the points it needs, a point off the
    /// display, or an element's frame without area.
    fn target(target: daemon::call_started::Target) -> Option<Target> {
        use daemon::call_started::Target as Wire;
        match target {
            Wire::Point(at) => Some(Target::Point(point(Some(at))?)),
            Wire::Element(element) => {
                let frame = Rect::try_from(element.frame?).ok()?;
                frame.has_area().then_some(Target::Element {
                    frame,
                    label: element.label,
                })
            }
            Wire::Drag(drag) => Some(Target::Drag {
                from: point(drag.from)?,
                to: point(drag.to)?,
            }),
        }
    }

    fn point(at: Option<common::Point>) -> Option<Point> {
        let at = at?;
        Point::new(at.x, at.y).ok()
    }

    /// A recording as listed. Refused when its id or a machine isn't a name.
    impl TryFrom<daemon::RecordingSummary> for RecordingSummary {
        type Error = ParseError;
        fn try_from(summary: daemon::RecordingSummary) -> Result<Self, ParseError> {
            Ok(Self {
                id: summary.id.parse().map_err(|error| invalid("id", error))?,
                started_unix_seconds: summary.started_unix_seconds,
                session: summary.session,
                machines: summary
                    .machines
                    .iter()
                    .map(|name| name.parse().map_err(|error| invalid("machines", error)))
                    .collect::<Result<_, _>>()?,
                videos: summary.videos,
                calls: summary.calls,
                traced_ms: summary.traced_ms,
            })
        }
    }

    /// A recording, opened. Refused when its id isn't one; videos whose
    /// names aren't are left out, and trace lines that can't be read are
    /// kept as they are.
    impl TryFrom<daemon::Recording> for Recording {
        type Error = ParseError;
        fn try_from(recording: daemon::Recording) -> Result<Self, ParseError> {
            Ok(Self {
                id: recording.id.parse().map_err(|error| invalid("id", error))?,
                videos: recording
                    .videos
                    .into_iter()
                    .filter_map(|video| {
                        Some(RecordingVideo {
                            file: video.file.parse().ok()?,
                            machine: video.machine.parse().ok()?,
                            start_ms: video.start_ms,
                            points: video
                                .display_points
                                .and_then(|size| Size::try_from(size).ok()),
                        })
                    })
                    .collect(),
                trace: recording
                    .trace
                    .into_iter()
                    .enumerate()
                    .map(|(line, traced)| trace_line(traced, u32::try_from(line).ok()))
                    .collect(),
            })
        }
    }

    /// Line `index` of a trace.
    fn trace_line(line: daemon::TraceLine, index: Option<u32>) -> TraceLine {
        use daemon::trace_line::Line;
        match line.line {
            Some(Line::Call(call)) => {
                let machine = match call.machine {
                    Some(raw) => match raw.parse() {
                        Ok(name) => Some(name),
                        Err(_) => return TraceLine::Unreadable(format!("a call on {raw:?}")),
                    },
                    None => None,
                };
                TraceLine::Call(TraceCall {
                    start_ms: call.start_ms,
                    end_ms: call.end_ms,
                    machine,
                    call: call.call,
                    outcome: match daemon::TraceOutcome::try_from(call.outcome) {
                        Ok(daemon::TraceOutcome::Done) => TraceOutcome::Done,
                        Ok(daemon::TraceOutcome::Failed) => TraceOutcome::Failed,
                        Ok(daemon::TraceOutcome::TimedOut) => TraceOutcome::TimedOut,
                        Ok(daemon::TraceOutcome::Refused) => TraceOutcome::Refused,
                        Ok(daemon::TraceOutcome::Unspecified) | Err(_) => {
                            TraceOutcome::Unknown(call.outcome)
                        }
                    },
                    error: call.error,
                    waited_ms: call.waited_ms,
                    queued_ms: call.queued_ms,
                    by: match daemon::TraceActor::try_from(call.by) {
                        Ok(daemon::TraceActor::Session) => TraceActor::Session,
                        Ok(daemon::TraceActor::Person) => TraceActor::Person,
                        Ok(daemon::TraceActor::Unspecified) | Err(_) => {
                            TraceActor::Unknown(call.by)
                        }
                    },
                    acted: call.acted.map(|acted| Box::new(on_screen(acted))),
                    seen: index.filter(|_| call.seen),
                    refs: call
                        .refs
                        .and_then(|refs| refs.try_into().ok())
                        .map(Box::new),
                    read: call
                        .read
                        .and_then(|read| TextRead::try_from(read).ok())
                        .map(Box::new),
                })
            }
            Some(Line::Unreadable(raw)) => TraceLine::Unreadable(raw),
            None => TraceLine::Unreadable(String::new()),
        }
    }

    #[cfg(test)]
    mod tests {
        use doppel_machine::activity::*;
        use doppel_machine::geometry::{Point, Size};
        use doppel_machine::recording::*;
        use std::time::Duration;

        use crate::pb::common::v1 as common;
        use crate::pb::daemon::v1 as daemon;

        fn started(target: Option<daemon::call_started::Target>, gesture: i32) -> daemon::Activity {
            daemon::Activity {
                session: 3,
                request: 9,
                machine: "dev".into(),
                client: "claude".into(),
                caption: "click".into(),
                phase: Some(daemon::activity::Phase::Started(daemon::CallStarted {
                    why: String::new(),
                    gesture,
                    target,
                    mark: None,
                })),
            }
        }

        #[test]
        fn a_started_call_keeps_its_gesture_and_place() {
            let read = Activity::try_from(started(
                Some(daemon::call_started::Target::Point(common::Point {
                    x: 10.0,
                    y: 20.0,
                })),
                daemon::Gesture::Click as i32,
            ));
            assert_eq!(
                read.ok().map(|activity| activity.phase),
                Some(Phase::Started(OnScreen {
                    why: None,
                    gesture: Gesture::Click,
                    target: Some(Target::Point(Point::new(10.0, 20.0).unwrap())),
                    callout: None,
                }))
            );
        }

        #[test]
        fn unknown_gestures_are_kept_and_bad_places_dropped() {
            let read = Activity::try_from(started(
                Some(daemon::call_started::Target::Element(
                    daemon::ElementTarget {
                        frame: Some(common::Rect {
                            origin: Some(common::Point { x: 1.0, y: 1.0 }),
                            size: Some(common::Size {
                                width: 0.0,
                                height: 4.0,
                            }),
                        }),
                        label: "OK".into(),
                    },
                )),
                99,
            ));
            assert_eq!(
                read.ok().map(|activity| activity.phase),
                Some(Phase::Started(OnScreen {
                    why: None,
                    gesture: Gesture::Unknown(99),
                    target: None,
                    callout: None,
                }))
            );
        }

        #[test]
        fn a_mark_is_drawn_on_its_own_place() {
            let mut event = started(None, daemon::Gesture::Mark as i32);
            event.phase = Some(daemon::activity::Phase::Started(daemon::CallStarted {
                why: String::new(),
                gesture: daemon::Gesture::Mark as i32,
                target: None,
                mark: Some(daemon::ScreenMark {
                    shape: daemon::MarkShape::Arrow as i32,
                    at: Some(daemon::screen_mark::At::Point(common::Point {
                        x: 5.0,
                        y: 6.0,
                    })),
                    from: Some(common::Point { x: 1.0, y: 2.0 }),
                    note: " here ".into(),
                    lasts: Some(daemon::screen_mark::Lasts::UntilActedMs(3000)),
                }),
            }));
            assert_eq!(
                Activity::try_from(event)
                    .ok()
                    .map(|activity| activity.phase),
                Some(Phase::Started(OnScreen {
                    why: None,
                    gesture: Gesture::Mark,
                    target: Some(Target::Point(Point::new(5.0, 6.0).unwrap())),
                    callout: Some(Box::new(Callout {
                        shape: CalloutShape::Arrow {
                            from: Point::new(1.0, 2.0).ok()
                        },
                        note: Some("here".into()),
                        lasts: CalloutLasts::UntilActed(Duration::from_secs(3)),
                    })),
                }))
            );
        }

        #[test]
        fn events_without_a_phase_or_a_valid_machine_are_skipped() {
            let mut no_phase = started(None, 1);
            no_phase.phase = None;
            assert!(Activity::try_from(no_phase).is_err());
            let mut bad_name = started(None, 1);
            bad_name.machine = "Not A Name".into();
            assert!(Activity::try_from(bad_name).is_err());
        }

        #[test]
        fn endings_say_why() {
            let mut ended = started(None, 1);
            ended.phase = Some(daemon::activity::Phase::Ended(daemon::CallEnded {
                outcome: Some(daemon::call_ended::Outcome::TimedOut("no window".into())),
                seen: None,
                refs: None,
                read: None,
            }));
            assert_eq!(
                Activity::try_from(ended)
                    .ok()
                    .map(|activity| activity.phase),
                Some(Phase::Ended(Ended {
                    ending: Ending::TimedOut("no window".into()),
                    seen: None,
                    refs: None,
                    read: None,
                }))
            );
        }

        #[test]
        fn names_are_read_and_a_generation_of_zero_is_skipped() {
            let named = |generation| {
                let mut event = started(None, 1);
                event.phase = Some(daemon::activity::Phase::Named(daemon::ElementRefs {
                    snapshot: 7,
                    generation,
                    nodes: vec![3, 5],
                }));
                Activity::try_from(event)
                    .ok()
                    .map(|activity| activity.phase)
            };
            let Some(Phase::Named(refs)) = named(2) else {
                panic!("expected names");
            };
            let refs: Vec<_> = refs
                .refs()
                .map(|(node, r)| (node.get(), r.to_string()))
                .collect();
            assert_eq!(refs, [(3, "s2e1".to_owned()), (5, "s2e2".to_owned())]);
            assert_eq!(named(0), None);
        }

        #[test]
        fn a_recording_keeps_what_it_can_read() {
            let recording = Recording::try_from(daemon::Recording {
                id: "2026-11-14T06-15-42Z-s1".into(),
                videos: vec![
                    daemon::RecordingVideo {
                        file: "live-001.mp4".into(),
                        machine: "live".into(),
                        start_ms: Some(250),
                        display_points: Some(crate::pb::common::v1::Size {
                            width: 1280.0,
                            height: 800.0,
                        }),
                    },
                    daemon::RecordingVideo {
                        file: "../etc".into(),
                        machine: "live".into(),
                        start_ms: None,
                        display_points: None,
                    },
                ],
                trace: vec![
                    daemon::TraceLine {
                        line: Some(daemon::trace_line::Line::Call(daemon::TraceCall {
                            start_ms: 10,
                            end_ms: 20,
                            machine: Some("live".into()),
                            call: "click".into(),
                            outcome: 99,
                            by: daemon::TraceActor::Person as i32,
                            ..daemon::TraceCall::default()
                        })),
                    },
                    daemon::TraceLine { line: None },
                ],
            })
            .unwrap();
            assert_eq!(recording.videos.len(), 1, "a bad file name is left out");
            assert_eq!(recording.videos[0].points, Size::new(1280.0, 800.0).ok());
            let TraceLine::Call(call) = &recording.trace[0] else {
                panic!("a call");
            };
            assert_eq!(call.outcome, TraceOutcome::Unknown(99));
            assert_eq!(call.by, TraceActor::Person);
            assert_eq!(recording.trace[1], TraceLine::Unreadable(String::new()));
        }
    }
}
