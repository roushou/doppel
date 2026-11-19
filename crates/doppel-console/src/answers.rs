//! The daemon's answers, as its clients parse them (`doppel_protocol`'s
//! reports and conversions), in the window's words. The desktop's port and
//! the browser's both map through here, so the two windows read the daemon
//! the same way.

use doppel_desktop::outcome::Failure;
use doppel_machine::lineage::RecordedLineage;
use doppel_machine::machine::MachineName;
use doppel_machine::vm::{RunState, Status};
use doppel_protocol::report::{
    CheckpointResult, MachineFailure, SessionInfo, SessionStatus, StopOutcome,
};

use crate::{
    LocalTime, Machine, MachineState, RecordingId, RecordingSummary, Run, Session, SessionActivity,
    SessionId, Verdict,
};

/// A machine the daemon lists, with its status or why it can't be read.
#[must_use]
pub fn machine(
    name: MachineName,
    status: Result<Status, MachineFailure>,
    lineage: RecordedLineage,
) -> Machine {
    Machine {
        name,
        state: match status {
            Ok(Status::Stopped(_)) => MachineState::Stopped,
            Ok(Status::Live(state)) => MachineState::Live(run(state)),
            Err(failure) => MachineState::Unreadable(failure.message().into()),
        },
        lineage,
    }
}

const fn run(state: RunState) -> Run {
    match state {
        RunState::Starting => Run::Starting,
        RunState::Running => Run::Running,
        RunState::Paused => Run::Paused,
        RunState::Pausing | RunState::Resuming | RunState::Saving | RunState::Restoring => {
            Run::Busy
        }
        RunState::Stopping | RunState::Stopped => Run::Stopping,
        RunState::Error => Run::Error,
        RunState::Unrecognized(_) => Run::Unknown,
    }
}

/// An agent's session.
#[must_use]
pub fn session(info: SessionInfo) -> Session {
    Session {
        id: SessionId(info.id),
        client: info.client,
        machines: info.machines,
        running: info.running,
        answered: info.answered,
        activity: match info.state {
            SessionStatus::Active => SessionActivity::Active,
            SessionStatus::Paused => SessionActivity::Paused,
            SessionStatus::Unknown(raw) => SessionActivity::Unknown(raw),
        },
    }
}

/// A recorded session, started at `started` on the person's clock.
#[must_use]
pub fn summary(
    summary: doppel_machine::recording::RecordingSummary,
    started: LocalTime,
) -> RecordingSummary {
    RecordingSummary {
        id: RecordingId::new(summary.id.to_string()),
        started,
        session: summary.session,
        machines: summary.machines,
        videos: summary.videos,
        calls: summary.calls,
        traced_ms: summary.traced_ms,
    }
}

/// An order that went as asked.
#[must_use]
pub const fn done() -> Verdict {
    Verdict::Done { note: None }
}

/// How a stop went.
#[must_use]
pub fn stopped(outcome: StopOutcome) -> Verdict {
    match outcome {
        StopOutcome::Stopped => done(),
        StopOutcome::ShutdownPending => Verdict::Done {
            note: Some("asked to shut down; it hasn't yet".into()),
        },
    }
}

/// How a checkpoint went.
#[must_use]
pub fn checkpoint(result: CheckpointResult) -> Verdict {
    match result {
        CheckpointResult::Completed {
            left_paused: Some(failure),
            ..
        } => Verdict::Done {
            note: Some(format!("saved, but it may be left paused: {failure}")),
        },
        CheckpointResult::Completed {
            left_paused: None, ..
        } => done(),
        CheckpointResult::Failed { error, .. } => Verdict::Refused(error.to_string()),
        CheckpointResult::Unknown { error, .. } => Verdict::Unknown(error.to_string()),
        CheckpointResult::InProgress { .. } => {
            Verdict::Unknown("the checkpoint ended before it finished".into())
        }
    }
}

/// A machine failure: refused when nothing was done, unknown when it
/// may have been.
#[must_use]
pub fn failed(failure: &MachineFailure) -> Verdict {
    let words = failure.to_string();
    match failure.certainty() {
        doppel_desktop::outcome::Certainty::NotDone => Verdict::Refused(words),
        doppel_desktop::outcome::Certainty::MaybeDone => Verdict::Unknown(words),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doppel_desktop::outcome::Certainty;
    use doppel_machine::lineage::Lineage;

    #[test]
    fn listings_become_machines() {
        let dev: MachineName = "dev".parse().unwrap();
        assert_eq!(
            machine(
                dev.clone(),
                Ok(Status::Live(RunState::Saving)),
                RecordedLineage::Unreadable("bad".into()),
            ),
            Machine {
                name: dev.clone(),
                state: MachineState::Live(Run::Busy),
                lineage: RecordedLineage::Unreadable("bad".into()),
            }
        );
        assert_eq!(
            machine(
                dev,
                Err(MachineFailure::new("no spec".into(), Certainty::NotDone)),
                RecordedLineage::Known(Lineage::default()),
            )
            .state,
            MachineState::Unreadable("no spec".into())
        );
    }

    #[test]
    fn a_failure_reads_as_refused_only_when_nothing_was_done() {
        let refused = MachineFailure::new("busy".into(), Certainty::NotDone);
        assert_eq!(failed(&refused), Verdict::Refused("busy".into()));
        let maybe = MachineFailure::new("lost".into(), Certainty::MaybeDone);
        assert_eq!(failed(&maybe), Verdict::Unknown("lost".into()));
    }

    #[test]
    fn a_shutdown_still_pending_is_done_with_a_note() {
        assert_eq!(
            stopped(StopOutcome::ShutdownPending),
            Verdict::Done {
                note: Some("asked to shut down; it hasn't yet".into()),
            }
        );
        assert_eq!(
            checkpoint(CheckpointResult::InProgress {
                awaiting: doppel_machine::checkpoint::Step::SyncingGuest,
            }),
            Verdict::Unknown("the checkpoint ended before it finished".into())
        );
    }
}
