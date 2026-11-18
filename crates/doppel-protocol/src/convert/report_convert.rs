use std::time::Duration;

use doppel_machine::screen_probe::HostScreen;

use super::ParseError;
use crate::pb::daemon::v1 as daemon;
use crate::report::{
    CheckpointReport, CheckpointResult, Cleanup, HandedBack, MachineFailure, SessionInfo,
    SessionStatus, StopOutcome, StopReport, StopRequest,
};

impl TryFrom<daemon::MachineStopped> for StopReport {
    type Error = ParseError;

    fn try_from(stopped: daemon::MachineStopped) -> Result<Self, ParseError> {
        use daemon::machine_stopped::Request;
        let request = match stopped.request {
            None => return Err(ParseError::Missing("machine_stopped.request")),
            Some(Request::Forced(_)) => StopRequest::Forced,
            Some(Request::Session(_)) => StopRequest::Session,
            Some(Request::SessionAfterUnlockMicros(micros)) => StopRequest::SessionAfterUnlock {
                unlocked_after: Duration::from_micros(micros),
            },
            Some(Request::PowerButton(why)) => StopRequest::PowerButton { why },
        };
        let outcome = match daemon::StopOutcome::try_from(stopped.outcome) {
            Ok(daemon::StopOutcome::Stopped) => StopOutcome::Stopped,
            Ok(daemon::StopOutcome::ShutdownPending) => StopOutcome::ShutdownPending,
            Ok(daemon::StopOutcome::Unspecified) | Err(_) => {
                return Err(ParseError::UnknownEnum {
                    field: "machine_stopped.outcome",
                    value: stopped.outcome,
                });
            }
        };
        Ok(Self { request, outcome })
    }
}

impl TryFrom<daemon::CheckpointReport> for CheckpointReport {
    type Error = ParseError;

    fn try_from(report: daemon::CheckpointReport) -> Result<Self, ParseError> {
        use daemon::checkpoint_report::Result as Wire;
        let failure = |failure: Option<daemon::MachineFailure>, field| {
            failure
                .map(MachineFailure::from_wire)
                .ok_or(ParseError::Missing(field))
        };
        let result = match report.result {
            None => return Err(ParseError::Missing("checkpoint_report.result")),
            Some(Wire::Completed(completed)) => CheckpointResult::Completed {
                memory_bytes: completed.memory_bytes,
                sync_skipped: completed.sync_skipped,
                left_paused: completed.left_paused.map(MachineFailure::from_wire),
                screen: completed.screen.map(HostScreen::try_from).transpose()?,
            },
            Some(Wire::Failed(failed)) => CheckpointResult::Failed {
                at: ParseError::checkpoint_step("checkpoint_failed.at", failed.at)?,
                error: failure(failed.error, "checkpoint_failed.error")?,
                guest: ParseError::guest_after_failure("checkpoint_failed.guest", failed.guest)?,
            },
            Some(Wire::Unknown(unknown)) => CheckpointResult::Unknown {
                at: ParseError::checkpoint_step("checkpoint_unknown.at", unknown.at)?,
                error: failure(unknown.error, "checkpoint_unknown.error")?,
            },
            Some(Wire::InProgress(step)) => CheckpointResult::InProgress {
                awaiting: ParseError::checkpoint_step("checkpoint_report.in_progress", step)?,
            },
        };
        Ok(Self {
            result,
            paused_for: report.paused_for_micros.map(Duration::from_micros),
            cleanup: report
                .cleanup
                .into_iter()
                .map(|cleanup| Cleanup {
                    action: cleanup.action,
                    result: cleanup
                        .failure
                        .map_or(Ok(()), |failure| Err(MachineFailure::from_wire(failure))),
                })
                .collect(),
        })
    }
}

impl TryFrom<&daemon::HandedBack> for HandedBack {
    type Error = ParseError;

    fn try_from(note: &daemon::HandedBack) -> Result<Self, ParseError> {
        Ok(Self {
            machine: note
                .machine
                .parse()
                .map_err(|_| ParseError::Missing("handed_back.machine"))?,
            held: Duration::from_millis(note.held_millis),
        })
    }
}

impl TryFrom<daemon::SessionEntry> for SessionInfo {
    type Error = ParseError;

    fn try_from(entry: daemon::SessionEntry) -> Result<Self, ParseError> {
        Ok(Self {
            id: entry.id,
            machines: entry
                .machines
                .iter()
                .map(|name| name.parse())
                .collect::<Result<_, _>>()?,
            client: entry.client,
            answered: entry.answered,
            running: entry.running,
            state: match daemon::SessionState::try_from(entry.state) {
                Ok(daemon::SessionState::Active) => SessionStatus::Active,
                Ok(daemon::SessionState::Paused) => SessionStatus::Paused,
                Ok(daemon::SessionState::Unspecified) | Err(_) => {
                    SessionStatus::Unknown(entry.state)
                }
            },
        })
    }
}
