//! A session: one client driving machines (an agent's `doppel mcp`). It can
//! be paused, which holds its next calls, and it ends when its client goes.
//!
//! A pure state machine: `(state, event) -> (state, actions)`, with
//! illegal transitions as typed errors.

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    /// Its client's connection closed.
    Disconnected,
    /// The daemon stopped, and closed its connection.
    DaemonStopped,
}

impl std::fmt::Display for SessionEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Disconnected => "its connection closed",
            Self::DaemonStopped => "the daemon stopped",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Active,
    /// Its next calls wait; one already running finishes.
    Paused,
    Ended(SessionEnd),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    Pause,
    Resume,
    End(SessionEnd),
}

/// What the shell does after a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    /// Its calls wait from now on.
    Hold,
    /// Its waiting calls go on.
    Release,
    /// Its recordings are completed.
    Finish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("the session is already paused")]
    AlreadyPaused,
    #[error("the session isn't paused")]
    NotPaused,
    #[error("the session has ended")]
    Ended,
}

impl SessionState {
    /// # Errors
    /// Pausing a paused session, resuming an active one, or anything once
    /// it ended.
    pub fn on(self, event: SessionEvent) -> Result<(Self, Vec<SessionAction>), SessionError> {
        match (self, event) {
            (Self::Active, SessionEvent::Pause) => Ok((Self::Paused, vec![SessionAction::Hold])),
            (Self::Paused, SessionEvent::Pause) => Err(SessionError::AlreadyPaused),
            (Self::Paused, SessionEvent::Resume) => {
                Ok((Self::Active, vec![SessionAction::Release]))
            }
            (Self::Active, SessionEvent::Resume) => Err(SessionError::NotPaused),
            (Self::Active, SessionEvent::End(end)) => {
                Ok((Self::Ended(end), vec![SessionAction::Finish]))
            }
            // Its waiting calls are cancelled with its connection.
            (Self::Paused, SessionEvent::End(end)) => Ok((
                Self::Ended(end),
                vec![SessionAction::Release, SessionAction::Finish],
            )),
            (Self::Ended(_), SessionEvent::Pause | SessionEvent::Resume | SessionEvent::End(_)) => {
                Err(SessionError::Ended)
            }
        }
    }

    /// Whether its calls may run.
    #[must_use]
    pub const fn may_act(&self) -> bool {
        match self {
            Self::Active => true,
            Self::Paused | Self::Ended(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_pauses_resumes_and_ends() {
        let (paused, actions) = SessionState::Active.on(SessionEvent::Pause).unwrap();
        assert_eq!(
            (paused, actions),
            (SessionState::Paused, vec![SessionAction::Hold])
        );
        assert!(!paused.may_act());
        let (active, actions) = paused.on(SessionEvent::Resume).unwrap();
        assert_eq!(
            (active, actions),
            (SessionState::Active, vec![SessionAction::Release])
        );
        let end = SessionEvent::End(SessionEnd::Disconnected);
        assert_eq!(
            active.on(end),
            Ok((
                SessionState::Ended(SessionEnd::Disconnected),
                vec![SessionAction::Finish]
            ))
        );
        assert_eq!(
            paused.on(end),
            Ok((
                SessionState::Ended(SessionEnd::Disconnected),
                vec![SessionAction::Release, SessionAction::Finish]
            ))
        );
    }

    #[test]
    fn illegal_transitions_are_refused() {
        assert_eq!(
            SessionState::Paused.on(SessionEvent::Pause),
            Err(SessionError::AlreadyPaused)
        );
        assert_eq!(
            SessionState::Active.on(SessionEvent::Resume),
            Err(SessionError::NotPaused)
        );
        let ended = SessionState::Ended(SessionEnd::Disconnected);
        for event in [
            SessionEvent::Pause,
            SessionEvent::Resume,
            SessionEvent::End(SessionEnd::Disconnected),
            SessionEvent::End(SessionEnd::DaemonStopped),
        ] {
            assert_eq!(ended.on(event), Err(SessionError::Ended));
        }
        assert!(!ended.may_act());
    }

    #[test]
    fn a_session_ends_with_its_reason_whatever_its_state() {
        for state in [SessionState::Active, SessionState::Paused] {
            let (ended, actions) = state
                .on(SessionEvent::End(SessionEnd::DaemonStopped))
                .unwrap();
            assert_eq!(ended, SessionState::Ended(SessionEnd::DaemonStopped));
            assert_eq!(actions.last(), Some(&SessionAction::Finish));
        }
    }
}
