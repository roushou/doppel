//! Stopping a running machine.
//!
//! Forced, it is powered off: nothing in the guest gets to run. Gracefully,
//! the logged-in session is asked to shut down (what the Apple menu's Shut
//! Down does, without its confirmation):
//!
//! ```text
//! ask the session ─ asked ─────────────────────────────→ wait for the stop
//!   ├ refused → unlock the screen → ask again ─ asked ──→ wait for the stop
//!   │             └ not locked, or not unlocked ─┐
//!   └ can't be asked ────────────────────────────┴→ power button → wait
//! ```
//!
//! A locked screen refuses the request, and the lock screen ignores the
//! power button too, so a refusal is followed by unlocking it with the
//! stored password. Without an agent to ask (the login window), the VM's
//! power button is pressed instead; on a logged-in guest macOS may answer
//! it with a confirmation dialog.
//!
//! A pure state machine, `(Stop, StopEvent) -> (Stop, Actions)`. `U` is the
//! shell's reason the session couldn't be asked, `E` its error for a broken
//! exchange; the machine only carries them.

use std::time::Duration;

/// How the machine is to be stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    Graceful,
    Force,
}

/// Where a stop left the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    Stopped,
    /// Asked to shut down, and still running.
    ShutdownPending,
}

/// How the guest was asked to stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopRequest<U> {
    /// Powered off: nothing in the guest got to run.
    Forced,
    /// The logged-in session was asked to shut down.
    Session,
    /// As `Session`, after unlocking the screen with the stored password.
    SessionAfterUnlock { unlocked_after: Duration },
    /// The VM's power button, because the session couldn't be asked.
    PowerButton { session: U },
}

/// Which time the session is being asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asking {
    First,
    AfterUnlock { unlocked_after: Duration },
}

/// Where a stop has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop<U, E> {
    PoweringOff,
    AskingSession(Asking),
    /// The session refused: unlocking the screen, then asking again.
    Unlocking {
        refused: U,
    },
    /// The session couldn't be asked, for this reason.
    PressingPowerButton {
        session: U,
    },
    /// Asked to stop, this way; waiting for the VM to stop.
    AwaitingStop(StopRequest<U>),
    /// The VM stopped.
    Stopped(StopRequest<U>),
    /// Asked to stop, but it hadn't by the deadline: a guest app may be
    /// holding the shutdown up, or a dialog awaits a click.
    ShutdownPending(StopRequest<U>),
    /// An exchange broke: whether the request went out says the error.
    Failed(E),
}

/// What the shell does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopAction {
    PowerOff,
    /// Asks the logged-in session to shut down, through the agent.
    AskSession,
    /// Unlocks the screen with the stored password.
    Unlock,
    PressPowerButton,
    /// Waits for the VM to stop, until the stop's deadline.
    AwaitStop,
}

/// What came of a [`StopAction`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopEvent<U, E> {
    PoweredOff,
    /// The session took the request.
    SessionAsked,
    /// The session was asked and refused: the screen may be locked.
    SessionRefused(U),
    /// The session couldn't be asked: no agent, say.
    SessionUnavailable(U),
    Unlocked {
        after: Duration,
    },
    /// The screen wasn't locked: the refusal stands.
    NotLocked,
    /// The screen stayed locked, for this reason.
    NotUnlocked(U),
    PowerButtonPressed,
    /// The VM stopped: its VMM closed the connection.
    VmStopped,
    /// The stop's deadline passed with the VM still running.
    DeadlinePassed,
    Broken(E),
}

/// An event the stop's state can't take: a bug in the shell.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("stop event {event} is not valid in state {state}")]
pub struct IllegalStopEvent {
    pub state: &'static str,
    pub event: &'static str,
}

impl<U, E> Stop<U, E> {
    #[must_use]
    pub fn begin(mode: StopMode) -> (Self, Vec<StopAction>) {
        match mode {
            StopMode::Force => (Self::PoweringOff, vec![StopAction::PowerOff]),
            StopMode::Graceful => (
                Self::AskingSession(Asking::First),
                vec![StopAction::AskSession],
            ),
        }
    }

    /// Whether it has ended: stopped, pending or failed.
    #[must_use]
    pub const fn is_over(&self) -> bool {
        match self {
            Self::Stopped(_) | Self::ShutdownPending(_) | Self::Failed(_) => true,
            Self::PoweringOff
            | Self::AskingSession(_)
            | Self::Unlocking { .. }
            | Self::PressingPowerButton { .. }
            | Self::AwaitingStop(_) => false,
        }
    }

    /// The next state, and what to do, after `event`.
    ///
    /// # Errors
    /// The event can't happen in this state, or the stop is over.
    pub fn on(self, event: StopEvent<U, E>) -> Result<(Self, Vec<StopAction>), IllegalStopEvent> {
        use StopAction as Do;
        use StopEvent as On;
        let press = |session| {
            (
                Self::PressingPowerButton { session },
                vec![Do::PressPowerButton],
            )
        };
        let wait = |request| (Self::AwaitingStop(request), vec![Do::AwaitStop]);
        match (self, event) {
            (
                Self::PoweringOff
                | Self::AskingSession(_)
                | Self::Unlocking { .. }
                | Self::PressingPowerButton { .. }
                | Self::AwaitingStop(_),
                On::Broken(error),
            ) => Ok((Self::Failed(error), Vec::new())),
            // A forced stop is done once asked: the VMM may close before it
            // answers.
            (Self::PoweringOff, On::PoweredOff) => {
                Ok((Self::Stopped(StopRequest::Forced), Vec::new()))
            }
            (Self::AskingSession(Asking::First), On::SessionAsked) => {
                Ok(wait(StopRequest::Session))
            }
            (Self::AskingSession(Asking::AfterUnlock { unlocked_after }), On::SessionAsked) => {
                Ok(wait(StopRequest::SessionAfterUnlock { unlocked_after }))
            }
            (Self::AskingSession(Asking::First), On::SessionRefused(refused)) => {
                Ok((Self::Unlocking { refused }, vec![Do::Unlock]))
            }
            // Refused again, unlocked: the power button is all that's left.
            (Self::AskingSession(Asking::AfterUnlock { .. }), On::SessionRefused(session))
            | (Self::AskingSession(_), On::SessionUnavailable(session))
            | (Self::Unlocking { .. }, On::NotUnlocked(session))
            | (Self::Unlocking { refused: session }, On::NotLocked) => Ok(press(session)),
            (Self::Unlocking { .. }, On::Unlocked { after }) => Ok((
                Self::AskingSession(Asking::AfterUnlock {
                    unlocked_after: after,
                }),
                vec![Do::AskSession],
            )),
            (Self::PressingPowerButton { session }, On::PowerButtonPressed) => {
                Ok(wait(StopRequest::PowerButton { session }))
            }
            (Self::AwaitingStop(request), On::VmStopped) => {
                Ok((Self::Stopped(request), Vec::new()))
            }
            (Self::AwaitingStop(request), On::DeadlinePassed) => {
                Ok((Self::ShutdownPending(request), Vec::new()))
            }
            (state, event) => Err(IllegalStopEvent {
                state: state.name(),
                event: event.name(),
            }),
        }
    }

    const fn name(&self) -> &'static str {
        match self {
            Self::PoweringOff => "powering off",
            Self::AskingSession(_) => "asking the session",
            Self::Unlocking { .. } => "unlocking",
            Self::PressingPowerButton { .. } => "pressing the power button",
            Self::AwaitingStop(_) => "awaiting the stop",
            Self::Stopped(_) => "stopped",
            Self::ShutdownPending(_) => "shutdown pending",
            Self::Failed(_) => "failed",
        }
    }
}

impl<U, E> StopEvent<U, E> {
    const fn name(&self) -> &'static str {
        match self {
            Self::PoweredOff => "powered off",
            Self::SessionAsked => "session asked",
            Self::SessionRefused(_) => "session refused",
            Self::SessionUnavailable(_) => "session unavailable",
            Self::Unlocked { .. } => "unlocked",
            Self::NotLocked => "not locked",
            Self::NotUnlocked(_) => "not unlocked",
            Self::PowerButtonPressed => "power button pressed",
            Self::VmStopped => "VM stopped",
            Self::DeadlinePassed => "deadline passed",
            Self::Broken(_) => "broken",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Run = Stop<&'static str, &'static str>;
    type On = StopEvent<&'static str, &'static str>;

    fn run(mode: StopMode, events: Vec<On>) -> (Run, Vec<StopAction>) {
        let (mut state, mut done) = Run::begin(mode);
        for event in events {
            let (next, actions) = state.on(event).unwrap();
            state = next;
            done.extend(actions);
        }
        (state, done)
    }

    #[test]
    fn a_forced_stop_is_done_once_powered_off() {
        let (state, actions) = run(StopMode::Force, vec![On::PoweredOff]);
        assert_eq!(state, Stop::Stopped(StopRequest::Forced));
        assert_eq!(actions, [StopAction::PowerOff]);
    }

    #[test]
    fn a_session_that_takes_the_request_is_waited_for() {
        use StopAction as Do;
        let (stopped, actions) = run(StopMode::Graceful, vec![On::SessionAsked, On::VmStopped]);
        assert_eq!(stopped, Stop::Stopped(StopRequest::Session));
        assert_eq!(actions, [Do::AskSession, Do::AwaitStop]);
        let (pending, _) = run(
            StopMode::Graceful,
            vec![On::SessionAsked, On::DeadlinePassed],
        );
        assert_eq!(pending, Stop::ShutdownPending(StopRequest::Session));
        assert!(pending.is_over());
    }

    /// A locked screen refuses: it is unlocked and the session asked again.
    #[test]
    fn a_refusal_is_followed_by_unlocking_and_asking_again() {
        use StopAction as Do;
        let after = Duration::from_secs(3);
        let (state, actions) = run(
            StopMode::Graceful,
            vec![
                On::SessionRefused("locked"),
                On::Unlocked { after },
                On::SessionAsked,
                On::VmStopped,
            ],
        );
        assert_eq!(
            state,
            Stop::Stopped(StopRequest::SessionAfterUnlock {
                unlocked_after: after
            })
        );
        assert_eq!(
            actions,
            [Do::AskSession, Do::Unlock, Do::AskSession, Do::AwaitStop]
        );
    }

    /// Whatever keeps the session from being asked, the power button is
    /// pressed, and the stop says why.
    #[test]
    fn the_power_button_is_the_fallback_and_says_why() {
        let after = Duration::from_secs(1);
        for (events, why) in [
            (vec![On::SessionUnavailable("no agent")], "no agent"),
            (
                vec![On::SessionRefused("refused"), On::NotLocked],
                "refused",
            ),
            (
                vec![
                    On::SessionRefused("refused"),
                    On::NotUnlocked("wrong password"),
                ],
                "wrong password",
            ),
            (
                vec![
                    On::SessionRefused("refused"),
                    On::Unlocked { after },
                    On::SessionRefused("refused again"),
                ],
                "refused again",
            ),
        ] {
            let (state, actions) = run(
                StopMode::Graceful,
                [events, vec![On::PowerButtonPressed, On::VmStopped]].concat(),
            );
            assert_eq!(
                state,
                Stop::Stopped(StopRequest::PowerButton { session: why })
            );
            assert!(actions.contains(&StopAction::PressPowerButton));
        }
    }

    #[test]
    fn a_broken_exchange_fails_the_stop_and_an_event_out_of_place_is_refused() {
        let (failed, _) = run(
            StopMode::Graceful,
            vec![On::SessionAsked, On::Broken("reset")],
        );
        assert_eq!(failed, Stop::Failed("reset"));
        let (powering_off, _) = Run::begin(StopMode::Force);
        assert_eq!(
            powering_off.on(On::SessionAsked),
            Err(IllegalStopEvent {
                state: "powering off",
                event: "session asked",
            })
        );
    }
}
