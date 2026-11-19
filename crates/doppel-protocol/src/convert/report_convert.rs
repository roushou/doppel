use std::time::Duration;

use doppel_machine::screen_probe::HostScreen;

use super::ParseError;
use crate::pb::daemon::v1 as daemon;
use crate::report::{
    Acted, CheckpointReport, CheckpointResult, Cleanup, HandedBack, MachineFailure, SessionInfo,
    SessionStatus, StopReport, StopRequest,
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
            Some(Request::PowerButton(session)) => StopRequest::PowerButton { session },
        };
        let outcome = ParseError::stop_outcome("machine_stopped.outcome", stopped.outcome)?;
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

impl From<Acted> for daemon::ActOutcome {
    fn from(acted: Acted) -> Self {
        match acted {
            Acted::Done => Self::Done,
            Acted::StaleReference => Self::StaleReference,
        }
    }
}

impl ParseError {
    /// Parses a daemon `ActOutcome` field.
    pub fn acted(field: &'static str, raw: i32) -> Result<Acted, Self> {
        Self::enumeration(field, raw, |acted| match acted {
            daemon::ActOutcome::Done => Some(Acted::Done),
            daemon::ActOutcome::StaleReference => Some(Acted::StaleReference),
            daemon::ActOutcome::Unspecified => None,
        })
    }
}

impl From<&HandedBack> for daemon::HandedBack {
    fn from(note: &HandedBack) -> Self {
        Self {
            machine: note.machine.to_string(),
            held_millis: u64::try_from(note.held.as_millis()).unwrap_or(u64::MAX),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn act_outcomes_round_trip_and_unspecified_is_rejected() {
        for acted in [Acted::Done, Acted::StaleReference] {
            let raw = daemon::ActOutcome::from(acted) as i32;
            assert_eq!(ParseError::acted("a", raw), Ok(acted));
        }
        assert!(ParseError::acted("a", 0).is_err());
    }

    #[test]
    fn a_handed_back_note_round_trips() {
        let note = HandedBack {
            machine: "dev".parse().unwrap(),
            held: Duration::from_millis(1500),
        };
        assert_eq!(
            HandedBack::try_from(&daemon::HandedBack::from(&note)),
            Ok(note)
        );
    }
}
