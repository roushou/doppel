//! Starting a machine: the VMM process is launched, then
//!
//! ```text
//! connect (again, while its socket isn't listening yet)
//!   → read the VM's state → wait for state changes → running
//! ```
//!
//! A pure state machine, `(Startup, StartupEvent) -> (Startup, Actions)`:
//! the shell performs each [`StartupAction`] and feeds back the event it
//! produced.
//!
//! It ends in one of three states. [`Startup::Running`] is success.
//! [`Startup::Failed`] is a failure with the VMM known to be gone, so
//! nothing is using the machine's files: a VMM that exited, or one that was
//! stopped after it reported failure. [`Startup::Unknown`] is everything
//! else: a timeout or a broken exchange, after which the VMM may still be
//! bringing the guest up. An unknown outcome is never rounded up to success.
//!
//! `E` is the shell's error for a broken exchange. The machine never
//! inspects it, only carries it to the end.

use crate::vm::RunState;

/// Where a startup has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Startup<E> {
    /// The VMM was launched; connecting to its socket.
    Connecting,
    /// Connected; reading the VM's state.
    Connected,
    /// The VM is in this state, not yet running.
    Waiting(RunState),
    /// The VM failed; stopping the VMM so nothing uses the machine's files.
    StoppingVmm(StartupFailure<E>),
    /// The VM runs, in this state.
    Running(RunState),
    /// It failed, and the VMM is gone.
    Failed(StartupFailure<E>),
    /// It may still be starting: the VMM may be running.
    Unknown(StartupFailure<E>),
}

/// Why a startup didn't end running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupFailure<E> {
    /// The VMM process exited, with this status.
    VmmExited { status: String },
    /// The VM reported this state (stopped or failed) instead of running.
    VmEnded(RunState),
    /// The VMM closed its connection before the VM ran.
    ConnectionClosed,
    /// The startup's deadline passed.
    TimedOut,
    /// An exchange with the VMM, or a look at its process, broke.
    Broken(E),
}

/// What the shell does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupAction {
    /// Checks the VMM process is still there, then connects to its socket.
    Connect,
    /// As [`Self::Connect`], after a short pause: the socket wasn't
    /// listening yet, and nothing announces when it is.
    ConnectAgain,
    /// Reads the VM's state.
    ReadState,
    /// Waits for the VM's next state change.
    AwaitStateChange,
    /// Stops the VMM process and waits for it to exit.
    StopVmm,
}

/// What came of a [`StartupAction`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupEvent<E> {
    /// The VMM process exited, with this status.
    VmmExited {
        status: String,
    },
    /// Its socket isn't listening yet.
    NotListening,
    Connected,
    /// The VM is in this state: read, or changed to.
    State(RunState),
    /// The VMM closed the connection.
    Closed,
    /// The startup's deadline passed.
    TimedOut,
    /// An exchange or a look at the process broke: its effect is unknown.
    Broken(E),
    /// The VMM process was stopped and has exited.
    VmmStopped,
    /// Stopping the VMM process failed: it may still run.
    VmmNotStopped,
}

/// An event the startup's state can't take: a bug in the shell.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("startup event {event} is not valid in state {state}")]
pub struct IllegalStartupEvent {
    pub state: &'static str,
    pub event: &'static str,
}

impl<E> Startup<E> {
    /// The VMM was just launched.
    #[must_use]
    pub fn begin() -> (Self, Vec<StartupAction>) {
        (Self::Connecting, vec![StartupAction::Connect])
    }

    /// Whether it has ended: running, failed or unknown.
    #[must_use]
    pub const fn is_over(&self) -> bool {
        match self {
            Self::Running(_) | Self::Failed(_) | Self::Unknown(_) => true,
            Self::Connecting | Self::Connected | Self::Waiting(_) | Self::StoppingVmm(_) => false,
        }
    }

    /// The next state, and what to do, after `event`.
    ///
    /// # Errors
    /// The event can't happen in this state, or the startup is over.
    pub fn on(
        self,
        event: StartupEvent<E>,
    ) -> Result<(Self, Vec<StartupAction>), IllegalStartupEvent> {
        use StartupAction as Do;
        use StartupEvent as On;
        match (self, event) {
            // A deadline or a broken exchange leaves a running VMM possible.
            (Self::Connecting | Self::Connected | Self::Waiting(_), On::TimedOut) => {
                Ok((Self::Unknown(StartupFailure::TimedOut), Vec::new()))
            }
            (Self::Connecting | Self::Connected | Self::Waiting(_), On::Broken(error)) => {
                Ok((Self::Unknown(StartupFailure::Broken(error)), Vec::new()))
            }
            (Self::Connecting, On::VmmExited { status }) => Ok((
                Self::Failed(StartupFailure::VmmExited { status }),
                Vec::new(),
            )),
            (Self::Connecting, On::NotListening) => Ok((Self::Connecting, vec![Do::ConnectAgain])),
            (Self::Connecting, On::Connected) => Ok((Self::Connected, vec![Do::ReadState])),
            (Self::Connected | Self::Waiting(_), On::State(state)) => Ok(Self::reached(state)),
            (Self::Waiting(_), On::Closed) => Ok((
                Self::StoppingVmm(StartupFailure::ConnectionClosed),
                vec![Do::StopVmm],
            )),
            (Self::StoppingVmm(failure), On::VmmStopped) => Ok((Self::Failed(failure), Vec::new())),
            (Self::StoppingVmm(failure), On::VmmNotStopped) => {
                Ok((Self::Unknown(failure), Vec::new()))
            }
            (state, event) => Err(IllegalStartupEvent {
                state: state.name(),
                event: event.name(),
            }),
        }
    }

    /// The VM reported `state`.
    fn reached(state: RunState) -> (Self, Vec<StartupAction>) {
        match state {
            RunState::Running => (Self::Running(state), Vec::new()),
            RunState::Error | RunState::Stopped => (
                Self::StoppingVmm(StartupFailure::VmEnded(state)),
                vec![StartupAction::StopVmm],
            ),
            RunState::Starting
            | RunState::Pausing
            | RunState::Paused
            | RunState::Resuming
            | RunState::Saving
            | RunState::Restoring
            | RunState::Stopping
            | RunState::Unrecognized(_) => {
                (Self::Waiting(state), vec![StartupAction::AwaitStateChange])
            }
        }
    }

    const fn name(&self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Waiting(_) => "waiting",
            Self::StoppingVmm(_) => "stopping the VMM",
            Self::Running(_) => "running",
            Self::Failed(_) => "failed",
            Self::Unknown(_) => "unknown",
        }
    }
}

impl<E> StartupEvent<E> {
    const fn name(&self) -> &'static str {
        match self {
            Self::VmmExited { .. } => "VMM exited",
            Self::NotListening => "not listening",
            Self::Connected => "connected",
            Self::State(_) => "state",
            Self::Closed => "closed",
            Self::TimedOut => "timed out",
            Self::Broken(_) => "broken",
            Self::VmmStopped => "VMM stopped",
            Self::VmmNotStopped => "VMM not stopped",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Run = Startup<&'static str>;

    /// Feeds `events` from the start; the states and actions on the way.
    fn run(events: Vec<StartupEvent<&'static str>>) -> (Run, Vec<StartupAction>) {
        let (mut state, mut done) = Run::begin();
        for event in events {
            let (next, actions) = state.on(event).unwrap();
            state = next;
            done.extend(actions);
        }
        (state, done)
    }

    #[test]
    fn a_vm_that_comes_up_ends_running() {
        use StartupAction as Do;
        use StartupEvent as On;
        let (state, actions) = run(vec![
            On::NotListening,
            On::NotListening,
            On::Connected,
            On::State(RunState::Starting),
            On::State(RunState::Running),
        ]);
        assert_eq!(state, Startup::Running(RunState::Running));
        assert!(state.is_over());
        assert_eq!(
            actions,
            [
                Do::Connect,
                Do::ConnectAgain,
                Do::ConnectAgain,
                Do::ReadState,
                Do::AwaitStateChange,
            ]
        );
    }

    #[test]
    fn a_vmm_that_exits_fails_with_its_status() {
        let (state, _) = run(vec![StartupEvent::VmmExited {
            status: "exit status: 1".into(),
        }]);
        assert_eq!(
            state,
            Startup::Failed(StartupFailure::VmmExited {
                status: "exit status: 1".into()
            })
        );
    }

    /// A VM that failed has its VMM stopped: only then has it failed.
    #[test]
    fn a_vm_that_fails_has_its_vmm_stopped_before_it_counts_as_failed() {
        use StartupEvent as On;
        for end in [RunState::Error, RunState::Stopped] {
            let (stopping, actions) = run(vec![On::Connected, On::State(end)]);
            assert_eq!(stopping, Startup::StoppingVmm(StartupFailure::VmEnded(end)));
            assert_eq!(actions.last(), Some(&StartupAction::StopVmm));
            assert!(!stopping.is_over());
            let (stopped, _) = stopping.clone().on(On::VmmStopped).unwrap();
            assert_eq!(stopped, Startup::Failed(StartupFailure::VmEnded(end)));
            let (not_stopped, _) = stopping.on(On::VmmNotStopped).unwrap();
            assert_eq!(not_stopped, Startup::Unknown(StartupFailure::VmEnded(end)));
        }
        let (closed, _) = run(vec![
            On::Connected,
            On::State(RunState::Starting),
            On::Closed,
            On::VmmStopped,
        ]);
        assert_eq!(closed, Startup::Failed(StartupFailure::ConnectionClosed));
    }

    /// A timeout or a broken exchange leaves it unknown, never failed or
    /// running, whatever the startup had got to.
    #[test]
    fn a_timeout_or_a_broken_exchange_leaves_the_outcome_unknown() {
        use StartupEvent as On;
        for before in [
            vec![],
            vec![On::Connected],
            vec![On::Connected, On::State(RunState::Starting)],
        ] {
            let (timed_out, _) = run([before.clone(), vec![On::TimedOut]].concat());
            assert_eq!(timed_out, Startup::Unknown(StartupFailure::TimedOut));
            let (broken, _) = run([before, vec![On::Broken("reset")]].concat());
            assert_eq!(broken, Startup::Unknown(StartupFailure::Broken("reset")));
        }
    }

    #[test]
    fn an_event_out_of_place_is_refused() {
        let (connecting, _) = Run::begin();
        assert_eq!(
            connecting.on(StartupEvent::State(RunState::Running)),
            Err(IllegalStartupEvent {
                state: "connecting",
                event: "state",
            })
        );
        let (running, _) = run(vec![
            StartupEvent::Connected,
            StartupEvent::State(RunState::Running),
        ]);
        assert!(running.on(StartupEvent::TimedOut).is_err(), "it is over");
    }
}
