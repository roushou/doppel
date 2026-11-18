//! The machine operations agents call: each waits its machine's turn, then
//! blocks until it's done. They speak in machines, not tools: refs,
//! rendering and the model's vocabulary belong to the MCP side.

use doppel_desktop::agent::AgentReport;
use doppel_desktop::ax::tree::{ActOn, AxSnapshot, Menus, Scope, SnapshotRequest};
use doppel_desktop::ax::tree::{SnapshotScope, SnapshotSpec};
use doppel_desktop::deadline::Deadline;
use doppel_desktop::exec::{ExecOutput, ExecRequest};
use doppel_desktop::files::{ReadRequest, WriteRequest};
use doppel_desktop::input::Input;
use doppel_desktop::input::Via;
use doppel_desktop::outcome::{Certainty, Failure, Outcome, Unconfirmed};
use doppel_desktop::screen::Screenshot;
use doppel_desktop::screen::ScreenshotSize;
use doppel_desktop::screen_text::ScreenText;
use doppel_desktop::wait::{
    Condition, Next, Poll, TextCondition, WaitOutcome, WaitSpec, Waiting, Watch,
};
use doppel_host::agent::{AgentClient, AgentError};
use doppel_host::capabilities::Clock;
use doppel_host::files::Filesystem;
use doppel_host::launcher::Window;
use doppel_host::login::AwaitDesktopError;
use doppel_host::machines::{
    AgentConnection, AgentConnectionError, CHECKPOINT_TIMEOUT, CLAIM_TIMEOUT, CheckpointReport,
    Claim, Listing, MachineError, Machines, Paused, Resumed, START_TIMEOUT, STOP_TIMEOUT,
    StopOutcome, StopReport,
};
use doppel_host::store::{Listed, StoreError};
use doppel_host::vmm::{StopMode, VmmError};
use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::checkpoint::AfterCheckpoint;
use doppel_machine::lineage::{RecordedLineage, Role};
use doppel_machine::login::{Desktop, LoginPolicy};
use doppel_machine::machine::MachineName;
use doppel_machine::vm::MachineReport;
use doppel_machine::vm::{NextStart, RunState, Status};
use doppel_protocol::convert::RemoteError;
use doppel_protocol::transport::Cancel;

use crate::Daemon;
use crate::call_events::CallEvent;
use crate::forks::{ForkPlan, MacError, PlanError};
use crate::scheduler::Cancelled;
use crate::scheduler::{Begun, Call, Turn};

/// How an action on an element ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acted {
    Done,
    /// The element is gone or changed since its snapshot; nothing was done.
    StaleReference,
}

/// Why an operation failed. Each says whether it may still have taken
/// effect.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error(transparent)]
    Machine(#[from] MachineError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    Desktop(#[from] AwaitDesktopError),
    #[error(
        "{machine} has no checkpoint {checkpoint}; nothing was changed (status lists its checkpoints)"
    )]
    NoSuchCheckpoint {
        machine: MachineName,
        checkpoint: CheckpointLabel,
    },
    #[error("{0} didn't power off; check status")]
    StillRunning(MachineName),
    /// A rollback changes the machine before it can fail: whatever failed,
    /// the guest was powered off or its files replaced.
    #[error("rolling {machine} back to {checkpoint} failed part way: {source}")]
    RollbackInterrupted {
        machine: MachineName,
        checkpoint: CheckpointLabel,
        #[source]
        source: Box<CallError>,
    },
    /// Cancelled before it changed anything: while it waited its turn, or
    /// during a wait.
    #[error("cancelled")]
    Cancelled,
}

impl Failure for CallError {
    fn certainty(&self) -> Certainty {
        match self {
            Self::Machine(error) => error.certainty(),
            Self::Agent(error) => error.certainty(),
            Self::Desktop(error) => error.certainty(),
            Self::NoSuchCheckpoint { .. } | Self::Cancelled => Certainty::NotDone,
            // The power-off was asked for; the guest may finish it later.
            Self::StillRunning(_) | Self::RollbackInterrupted { .. } => Certainty::MaybeDone,
        }
    }
}

impl From<AgentConnectionError> for CallError {
    fn from(error: AgentConnectionError) -> Self {
        match error {
            AgentConnectionError::Connect(error) => Self::Machine(error),
            AgentConnectionError::Request(error) => Self::Agent(error),
        }
    }
}

impl<C, F> Daemon<C, F>
where
    C: Clock + Clone,
    F: Filesystem,
{
    /// `machine`'s turn (see [`Call::turn`]). Viewers are told the call
    /// started at its first.
    fn turn<'s>(&'s self, machine: &MachineName, call: &'s Call) -> Result<Turn<'s, C>, CallError> {
        let (turn, begun) = call
            .turn(machine, &self.agent_connections, || {
                self.machines.clock().now()
            })
            .map_err(|Cancelled| CallError::Cancelled)?;
        match begun {
            Begun::First(at, Some(session_call)) => self.publish(&CallEvent::TurnBegan {
                call: &session_call,
                at,
            }),
            Begun::First(_, None) | Begun::Again => {}
        }
        Ok(turn)
    }

    /// When claiming a machine must have finished.
    fn claim_deadline(&self) -> Deadline {
        self.machines.within(CLAIM_TIMEOUT)
    }

    /// A turn for an operation that stops, pauses or restarts the guest:
    /// the agent connection is closed, and the next call opens a fresh one.
    fn lifecycle_turn<'s>(
        &'s self,
        machine: &MachineName,
        call: &'s Call,
    ) -> Result<Turn<'s, C>, CallError> {
        let mut turn = self.turn(machine, call)?;
        turn.agent().close();
        Ok(turn)
    }

    pub(crate) fn status(
        &self,
        machine: &MachineName,
        call: &Call,
    ) -> Result<MachineReport, CallError> {
        let mut turn = self.turn(machine, call)?;
        let status = self
            .machines
            .machine(machine)
            .status(self.machines.deadline())?;
        let agent = match status {
            Status::Live(state) if state.is_running() => {
                let by = self.machines.deadline();
                match turn
                    .agent()
                    .run(&self.machines, machine, by, |agent, by| agent.status(by))
                {
                    Ok(status) => AgentReport::Connected(status),
                    Err(error) => AgentReport::Unreachable(error.to_string()),
                }
            }
            Status::Live(_) | Status::Stopped(_) => AgentReport::NotRunning,
        };
        let checkpoints = self
            .machines
            .machine(machine)
            .checkpoints()?
            .into_iter()
            .filter_map(Result::ok)
            .collect();
        Ok(MachineReport {
            status,
            agent,
            checkpoints,
        })
    }

    pub(crate) fn screenshot(
        &self,
        machine: &MachineName,
        via: Via,
        size: ScreenshotSize,
        call: &Call,
    ) -> Result<Screenshot, CallError> {
        let mut turn = self.turn(machine, call)?;
        let by = self.machines.deadline();
        match via {
            Via::Agent => Ok(turn.agent().run(&self.machines, machine, by, |agent, by| {
                let max = match size {
                    ScreenshotSize::MaxPixels(n) => n,
                    ScreenshotSize::MatchPoints => {
                        let display = agent.status(by)?.display;
                        Self::longest_side(display.width(), display.height())
                    }
                };
                agent.capture(max, by)
            })?),
            Via::Host => {
                let max = match size {
                    ScreenshotSize::MaxPixels(n) => n,
                    ScreenshotSize::MatchPoints => {
                        let store = self.machines.store();
                        let display = store
                            .read_spec(&store.bundle(machine))
                            .map_err(MachineError::from)?
                            .display()
                            .points();
                        Self::longest_side(display.width(), display.height())
                    }
                };
                let by = self.machines.deadline();
                Ok(self
                    .machines
                    .machine(machine)
                    .running(by)?
                    .screenshot(max, by)?)
            }
        }
    }

    pub(crate) fn recognize_text(
        &self,
        machine: &MachineName,
        call: &Call,
    ) -> Result<ScreenText, CallError> {
        let _turn = self.turn(machine, call)?;
        let by = self.machines.deadline();
        Ok(self
            .machines
            .machine(machine)
            .running(by)?
            .recognize_text(by)?)
    }

    /// A person's own read of the screen's text: it runs on the host's
    /// frame, never the guest, so it takes no turn.
    pub(crate) fn observe_text(&self, machine: &MachineName) -> Result<ScreenText, CallError> {
        let by = self.machines.deadline();
        Ok(self
            .machines
            .machine(machine)
            .running(by)?
            .recognize_text(by)?)
    }

    pub(crate) fn input(
        &self,
        machine: &MachineName,
        via: Via,
        input: &Input,
        call: &Call,
    ) -> Result<(), CallError> {
        let mut turn = self.turn(machine, call)?;
        match via {
            Via::Agent => {
                let by = self.machines.deadline();
                turn.agent().run(&self.machines, machine, by, |agent, by| {
                    agent.input(input, by)
                })?;
            }
            Via::Host => {
                let by = self.machines.deadline();
                self.machines
                    .machine(machine)
                    .running(by)?
                    .host_input(input, by)?;
            }
        }
        Ok(())
    }

    /// Input from a person at a viewer, through the VMM's virtual devices.
    /// It doesn't wait for the machine's queue: a person's clicks never
    /// wait behind an agent's call.
    pub(crate) fn host_input(&self, machine: &MachineName, input: &Input) -> Result<(), CallError> {
        let by = self.machines.deadline();
        self.machines
            .machine(machine)
            .running(by)?
            .host_input(input, by)?;
        Ok(())
    }

    pub(crate) fn snapshot(
        &self,
        machine: &MachineName,
        spec: SnapshotSpec,
        call: &Call,
    ) -> Result<AxSnapshot, CallError> {
        let mut turn = self.turn(machine, call)?;
        let by = self.machines.deadline();
        Ok(turn.agent().run(&self.machines, machine, by, |agent, by| {
            Self::take(agent, spec, by)
        })?)
    }

    /// A person's look at the frontmost app's focused window. It takes no
    /// turn, so the agents' calls keep their order, and runs on a
    /// connection of its own: the guest answers it between their requests
    /// and keeps none of it, so their refs stay valid.
    pub(crate) fn observe(&self, machine: &MachineName) -> Result<AxSnapshot, CallError> {
        let by = self.machines.deadline();
        let mut running = self.machines.machine(machine).running(by)?;
        let mut agent = running.agent(by)?;
        let look = SnapshotRequest::new(Scope::FocusedWindow, Menus::Exclude).observation();
        Ok(agent.ax_snapshot(&look, by)?)
    }

    pub(crate) fn act(
        &self,
        machine: &MachineName,
        act: &ActOn,
        call: &Call,
    ) -> Result<Acted, CallError> {
        let mut turn = self.turn(machine, call)?;
        let by = self.machines.deadline();
        match turn
            .agent()
            .run(&self.machines, machine, by, |agent, by| agent.act(act, by))
        {
            Ok(()) => Ok(Acted::Done),
            Err(AgentConnectionError::Request(AgentError::Remote(
                RemoteError::StaleReference { .. },
            ))) => Ok(Acted::StaleReference),
            Err(error) => Err(error.into()),
        }
    }

    /// Waits on accessibility snapshots, taken on the machine's agent
    /// connection.
    pub(crate) fn wait_for(
        &self,
        machine: &MachineName,
        wait: WaitSpec<Condition>,
        spec: SnapshotSpec,
        call: &Call,
    ) -> Result<WaitOutcome<AxSnapshot>, CallError> {
        let mut turn = self.turn(machine, call)?;
        let machines = &self.machines;
        Self::poll_until(
            machines,
            turn.agent(),
            call.cancel(),
            wait,
            |connection, by| match connection.run(machines, machine, by, |agent, by| {
                Self::take(agent, spec, by)
            }) {
                Ok(snapshot) => Poll::Seen(snapshot),
                Err(AgentConnectionError::Connect(error)) => Self::machine_poll(&error),
                Err(AgentConnectionError::Request(error)) => Self::agent_poll(&error),
            },
        )
    }

    /// Polls text recognition on the host.
    pub(crate) fn wait_for_text(
        &self,
        machine: &MachineName,
        wait: WaitSpec<TextCondition>,
        call: &Call,
    ) -> Result<WaitOutcome<ScreenText>, CallError> {
        let mut turn = self.turn(machine, call)?;
        let machines = &self.machines;
        // Not needed to read the screen, but its events wake the wait.
        let _ = turn.agent().client(machines, machine, machines.deadline());
        Self::poll_until(
            machines,
            turn.agent(),
            call.cancel(),
            wait,
            |_, by| match machines
                .machine(machine)
                .running(by)
                .and_then(|mut running| running.recognize_text(by))
            {
                Ok(text) => Poll::Seen(text),
                Err(error) => Self::machine_poll(&error),
            },
        )
    }

    /// A channel of its own: a long program mustn't hold the connection, and
    /// cancelling it closes the channel.
    pub(crate) fn exec(
        &self,
        machine: &MachineName,
        request: &ExecRequest,
        call: &Call,
    ) -> Result<ExecOutput, CallError> {
        let _turn = self.turn(machine, call)?;
        let by = self.machines.deadline();
        Ok(self
            .machines
            .machine(machine)
            .running(by)?
            .agent(by)?
            .exec(request, call.cancel())?)
    }

    pub(crate) fn read_file(
        &self,
        machine: &MachineName,
        request: &ReadRequest,
        call: &Call,
    ) -> Result<Vec<u8>, CallError> {
        let mut turn = self.turn(machine, call)?;
        let by = self.machines.deadline();
        Ok(turn.agent().run(&self.machines, machine, by, |agent, by| {
            agent.read_file(request, by)
        })?)
    }

    pub(crate) fn write_file(
        &self,
        machine: &MachineName,
        request: &WriteRequest,
        call: &Call,
    ) -> Result<(), CallError> {
        let mut turn = self.turn(machine, call)?;
        let by = self.machines.deadline();
        Ok(turn.agent().run(&self.machines, machine, by, |agent, by| {
            agent.write_file(request, by)
        })?)
    }

    pub(crate) fn checkpoint(
        &self,
        machine: &MachineName,
        label: &CheckpointLabel,
        then: AfterCheckpoint,
        call: &Call,
    ) -> Result<CheckpointReport, CallError> {
        let _turn = self.lifecycle_turn(machine, call)?;
        let claim = self.machines.claim(machine, self.claim_deadline())?;
        Ok(claim.checkpoint(label, then, self.machines.within(CHECKPOINT_TIMEOUT))?)
    }

    /// `source`'s turn; the new machine's claim keeps anyone else off it.
    pub(crate) fn fork(
        &self,
        source: &MachineName,
        checkpoint: &CheckpointLabel,
        into: &MachineName,
        call: &Call,
    ) -> Result<(), CallError> {
        let _turn = self.turn(source, call)?;
        let from = self.machines.claim(source, self.claim_deadline())?;
        let target = self.machines.claim(into, self.claim_deadline())?;
        Ok(from.fork(checkpoint, &target)?)
    }

    /// Deletes a stopped machine for good, after the calls on it before.
    pub(crate) fn delete(&self, machine: &MachineName, call: &Call) -> Result<(), CallError> {
        let _turn = self.lifecycle_turn(machine, call)?;
        let claim = self.machines.claim(machine, self.claim_deadline())?;
        Ok(claim.delete(self.machines.deadline())?)
    }

    /// Deletes one checkpoint for good, after the calls on its machine
    /// before.
    pub(crate) fn delete_checkpoint(
        &self,
        machine: &MachineName,
        checkpoint: &CheckpointLabel,
        call: &Call,
    ) -> Result<(), CallError> {
        let _turn = self.lifecycle_turn(machine, call)?;
        let claim = self.machines.claim(machine, self.claim_deadline())?;
        Ok(claim.delete_checkpoint(checkpoint)?)
    }

    pub(crate) fn start(
        &self,
        machine: &MachineName,
        window: Window,
        call: &Call,
    ) -> Outcome<(RunState, NextStart), CallError> {
        let _turn = self.lifecycle_turn(machine, call)?;
        let claim = self
            .machines
            .claim(machine, self.claim_deadline())
            .map_err(CallError::from)?;
        claim
            .start(window, self.machines.within(START_TIMEOUT))
            .map_err(|unconfirmed| unconfirmed.map(CallError::from))
    }

    /// Waits for the desktop, logging in with the stored password if
    /// asked, then, once it's ready, until screenshots show it as it is
    /// (after a resume from saved memory they lag). Takes no turn: it only
    /// watches, and a start's caller waits on it.
    pub(crate) fn await_desktop(
        &self,
        machine: &MachineName,
        call: &Call,
    ) -> Result<Desktop, CallError> {
        let machine = self.machines.machine(machine);
        let login = machine.await_desktop(LoginPolicy::DEFAULT, call.cancel())?;
        let screen = if login.is_ready() {
            Some(machine.catch_up_screen(call.cancel())?)
        } else {
            None
        };
        Ok(Desktop { login, screen })
    }

    pub(crate) fn stop(
        &self,
        machine: &MachineName,
        mode: StopMode,
        call: &Call,
    ) -> Result<StopReport, CallError> {
        let _turn = self.lifecycle_turn(machine, call)?;
        let claim = self.machines.claim(machine, self.claim_deadline())?;
        Ok(claim.stop(mode, self.machines.within(STOP_TIMEOUT))?)
    }

    pub(crate) fn pause(&self, machine: &MachineName, call: &Call) -> Result<(), CallError> {
        let _turn = self.lifecycle_turn(machine, call)?;
        let claim = self.machines.claim(machine, self.claim_deadline())?;
        Ok(claim.pause(self.machines.deadline())?)
    }

    pub(crate) fn resume(&self, machine: &MachineName, call: &Call) -> Result<Resumed, CallError> {
        let _turn = self.lifecycle_turn(machine, call)?;
        let claim = self.machines.claim(machine, self.claim_deadline())?;
        Ok(claim.resume(self.machines.deadline())?)
    }

    /// Replaces the machine's state with the checkpoint's. It must be stopped.
    pub(crate) fn restore(
        &self,
        machine: &MachineName,
        checkpoint: &CheckpointLabel,
        call: &Call,
    ) -> Outcome<(), CallError> {
        let _turn = self.lifecycle_turn(machine, call)?;
        let claim = self
            .machines
            .claim(machine, self.claim_deadline())
            .map_err(CallError::from)?;
        claim
            .restore(checkpoint, self.machines.deadline())
            .map_err(|unconfirmed| unconfirmed.map(CallError::from))
    }

    /// Checks the checkpoint exists before discarding anything, then powers
    /// off, restores and starts, all under one claim, and waits for the
    /// desktop. Once it has begun, a failure can't claim nothing changed.
    /// The machine comes back shown if its VMM said it was; otherwise
    /// (stopped, or a VMM that doesn't say) hidden.
    pub(crate) fn rollback(
        &self,
        machine: &MachineName,
        checkpoint: &CheckpointLabel,
        call: &Call,
    ) -> Outcome<Desktop, CallError> {
        let _turn = self.lifecycle_turn(machine, call)?;
        let known = self
            .machines
            .machine(machine)
            .checkpoints()
            .map_err(CallError::from)?;
        if !known.iter().any(|entry| entry.as_ref() == Ok(checkpoint)) {
            return Err(CallError::NoSuchCheckpoint {
                machine: machine.clone(),
                checkpoint: checkpoint.clone(),
            }
            .into());
        }
        let claim = self
            .machines
            .claim(machine, self.claim_deadline())
            .map_err(CallError::from)?;
        let begun = |error: CallError| {
            Unconfirmed::Unknown(CallError::RollbackInterrupted {
                machine: machine.clone(),
                checkpoint: checkpoint.clone(),
                source: Box::new(error),
            })
        };
        let window = match self
            .machines
            .machine(machine)
            .status(self.machines.deadline())
            .map_err(CallError::from)?
        {
            Status::Live(_) => {
                let window = self
                    .machines
                    .machine(machine)
                    .window(self.machines.deadline())
                    .map_err(CallError::from)?
                    .unwrap_or(Window::Hidden);
                self.force_stop(&claim).map_err(begun)?;
                window
            }
            Status::Stopped(_) => Window::Hidden,
        };
        claim
            .restore(checkpoint, self.machines.deadline())
            .map_err(|unconfirmed| begun(unconfirmed.into_error().into()))?;
        claim
            .start(window, self.machines.within(START_TIMEOUT))
            .map_err(|unconfirmed| begun(unconfirmed.into_error().into()))?;
        self.await_desktop(machine, call).map_err(begun)
    }

    /// A forced stop that must end with the VM off.
    fn force_stop(&self, claim: &Claim<'_, C, F>) -> Result<(), CallError> {
        match claim
            .stop(StopMode::Force, self.machines.within(STOP_TIMEOUT))?
            .outcome
        {
            StopOutcome::Stopped => Ok(()),
            StopOutcome::ShutdownPending => Err(CallError::StillRunning(claim.name().clone())),
        }
    }

    fn take<K: Clock>(
        agent: &mut AgentClient<K>,
        spec: SnapshotSpec,
        by: Deadline,
    ) -> Result<AxSnapshot, AgentError> {
        let scope = match spec.scope {
            SnapshotScope::FocusedWindow => Scope::FocusedWindow,
            SnapshotScope::FrontmostApp => match agent.status(by)?.frontmost {
                Some(app) => Scope::App(app.pid),
                None => {
                    return Err(AgentError::Remote(RemoteError::InvalidState(
                        "the guest has no frontmost app".into(),
                    )));
                }
            },
        };
        agent.ax_snapshot(&SnapshotRequest::new(scope, spec.menus), by)
    }

    /// Runs the core's wait machine: polls when it says and pauses in
    /// between. A change the guest reports ends a pause early (see
    /// [`Waiting::on_change`]); a cancel ends it, and the wait. Each poll
    /// gets the usual call budget, cut short by the wait's own end.
    fn poll_until<W: Watch>(
        machines: &Machines<C, F>,
        agent: &mut AgentConnection<C>,
        cancel: &Cancel,
        wait: WaitSpec<W>,
        mut poll: impl FnMut(&mut AgentConnection<C>, Deadline) -> Poll<W::Seen>,
    ) -> Result<WaitOutcome<W::Seen>, CallError> {
        let clock = machines.clock();
        let mut waiting = Waiting::start(wait, clock.now());
        loop {
            let by = machines.deadline().earlier(waiting.poll_deadline());
            let seen = poll(agent, by);
            // Whatever changed before this poll's reply, it has seen.
            agent.seen();
            let (mut next, mut at) = match waiting.observe(seen, clock.now()) {
                Next::Done(outcome) => return Ok(outcome),
                Next::PollAt(next, at) => (next, at),
            };
            loop {
                match agent.pause(clock, at, cancel) {
                    Paused::Due => break,
                    Paused::Changed => (next, at) = next.on_change(clock.now()),
                    Paused::Cancelled => return Err(CallError::Cancelled),
                }
            }
            waiting = next;
        }
    }

    /// Which agent failures are worth polling through.
    fn agent_poll<S>(error: &AgentError) -> Poll<S> {
        match error {
            // No window yet, an app that didn't answer in time, a tree that moved.
            AgentError::Remote(
                RemoteError::InvalidState(_)
                | RemoteError::Internal(_)
                | RemoteError::Timeout { .. },
            )
            // A broken connection: the next poll connects afresh.
            | AgentError::Timeout
            | AgentError::Io(_)
            | AgentError::Closed => Poll::Retryable(error.to_string()),
            AgentError::Remote(
                RemoteError::PermissionMissing(_)
                | RemoteError::InvalidArgument { .. }
                | RemoteError::StaleReference { .. }
                | RemoteError::Unsupported(_)
                | RemoteError::Unrecognized,
            )
            | AgentError::CannotObserve
            | AgentError::VersionMismatch { .. }
            | AgentError::Stream(_)
            | AgentError::Encode(_)
            | AgentError::Protocol(_)
            | AgentError::Parse(_)
            | AgentError::Cancelled => Poll::Fatal(error.to_string()),
        }
    }

    /// Which machine failures are worth polling through: a VMM slow to
    /// answer one poll (its screen busy, or the wait's own deadline reached
    /// mid-poll) may answer the next, and a wait that runs out on it has
    /// timed out, not failed.
    fn machine_poll<S>(error: &MachineError) -> Poll<S> {
        if let MachineError::Vmm(VmmError::Timeout) = error {
            Poll::Retryable(error.to_string())
        } else {
            Poll::Fatal(error.to_string())
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "display sizes in points are small positive numbers"
    )]
    fn longest_side(width: f64, height: f64) -> u32 {
        width.max(height).round() as u32
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use doppel_host::capabilities::SystemClock;
    use doppel_host::files::LocalFilesystem;
    use doppel_host::launcher::VmmBinary;
    use doppel_host::store::Store;
    use doppel_machine::bundle::{DataRoot, RuntimeDir};

    use super::*;

    #[test]
    fn a_cancelled_wait_ends_at_once_instead_of_at_its_timeout() {
        let dir = std::env::temp_dir().join(format!("dm-cancel-{}", std::process::id()));
        let machines = Machines::new(
            Store::new(
                DataRoot::new(dir.join("data")).unwrap(),
                RuntimeDir::new(dir.join("run")).unwrap(),
            ),
            VmmBinary::new(dir.join("no-vmm")),
            SystemClock,
            LocalFilesystem,
        );
        let cancel = Cancel::new();
        let mut agent = AgentConnection::default();
        let wait =
            WaitSpec::new(Condition::Appears("Save".into()), Duration::from_secs(60)).unwrap();
        let canceller = cancel.clone();
        let later = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            canceller.cancel();
        });
        let started = SystemClock.now();
        // Never satisfied: only the cancel can end it early.
        let result = Daemon::poll_until(&machines, &mut agent, &cancel, wait, |_, _| {
            Poll::<AxSnapshot>::Retryable("no window yet".into())
        });
        later.join().unwrap();
        assert!(matches!(result, Err(CallError::Cancelled)));
        assert!(SystemClock.now().duration_since(started) < Duration::from_secs(5));
    }

    #[test]
    fn a_vmm_slow_to_answer_a_poll_is_polled_through_but_one_gone_is_not() {
        type Ops = Daemon<SystemClock, LocalFilesystem>;
        assert!(matches!(
            Ops::machine_poll::<ScreenText>(&MachineError::Vmm(VmmError::Timeout)),
            Poll::Retryable(_)
        ));
        assert!(matches!(
            Ops::machine_poll::<ScreenText>(&MachineError::Vmm(VmmError::Closed)),
            Poll::Fatal(_)
        ));
    }
}

/// Why fresh forks weren't run. Every fork made before a failure is kept.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ForksError {
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Mac(#[from] MacError),
    #[error(transparent)]
    Turn(CallError),
    #[error(transparent)]
    Listing(MachineError),
    #[error("making {failed} failed{}", Self::kept(.made))]
    Making {
        made: Vec<MachineName>,
        failed: MachineName,
        #[source]
        source: Box<CallError>,
    },
}

impl ForksError {
    fn kept(made: &[MachineName]) -> String {
        if made.is_empty() {
            String::new()
        } else {
            let names: Vec<String> = made.iter().map(ToString::to_string).collect();
            format!(" ({} made and kept; none started)", names.join(", "))
        }
    }
}

impl Failure for ForksError {
    fn certainty(&self) -> Certainty {
        match self {
            Self::Plan(_) | Self::Mac(_) | Self::Turn(_) | Self::Listing(_) => Certainty::NotDone,
            Self::Making { made, source, .. } => {
                if made.is_empty() {
                    source.certainty()
                } else {
                    Certainty::MaybeDone
                }
            }
        }
    }
}

/// Why a temporary machine wasn't made. Nothing is left behind: a fork
/// that failed partway is removed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TemporaryMachineError {
    #[error("machine '{0}' does not exist")]
    NoSuchMachine(MachineName),
    #[error(
        "machine '{0}' isn't a golden image: `doppel golden prepare {0}` makes it one, or name one `doppel ls` lists as golden"
    )]
    NotGolden(MachineName),
    #[error("machine '{machine}' has a lineage file that can't be read ({reason})")]
    LineageUnreadable {
        machine: MachineName,
        reason: String,
    },
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Mac(#[from] MacError),
    #[error(transparent)]
    Turn(CallError),
    #[error(transparent)]
    Listing(MachineError),
    #[error("making {name} failed")]
    Making {
        name: MachineName,
        #[source]
        source: Box<CallError>,
    },
}

impl Failure for TemporaryMachineError {
    fn certainty(&self) -> Certainty {
        match self {
            Self::NoSuchMachine(_)
            | Self::NotGolden(_)
            | Self::LineageUnreadable { .. }
            | Self::Plan(_)
            | Self::Mac(_)
            | Self::Turn(_)
            | Self::Listing(_) => Certainty::NotDone,
            // A fork that failed partway is removed before this is answered,
            // or left marked temporary for the daemon's next start.
            Self::Making { source, .. } => source.certainty(),
        }
    }
}

impl<C, F> Daemon<C, F>
where
    C: Clock + Clone + Send + Sync,
    F: Filesystem + Sync,
{
    /// Makes a fresh fork of `golden`'s template, under `golden`'s turn,
    /// marked [`Role::Temporary`] as it is made. Refused before anything is
    /// made for a machine that isn't a golden image, or when one more
    /// wouldn't fit beside the running machines.
    pub(crate) fn make_temporary_machine(
        &self,
        golden: &MachineName,
        call: &Call,
    ) -> Result<MachineName, TemporaryMachineError> {
        let _turn = self
            .turn(golden, call)
            .map_err(TemporaryMachineError::Turn)?;
        let mut existing = Vec::new();
        let mut running = 0;
        let mut found = None;
        for listing in self
            .machines
            .list(self.machines.deadline())
            .map_err(TemporaryMachineError::Listing)?
        {
            match listing {
                Listing::Machine {
                    name,
                    status,
                    lineage,
                } => {
                    if matches!(status, Ok(Status::Live(_))) {
                        running += 1;
                    }
                    if name == *golden {
                        found = Some(lineage);
                    }
                    existing.push(name);
                }
                Listing::Unrecognized(_) => {}
            }
        }
        let template = match found {
            None => return Err(TemporaryMachineError::NoSuchMachine(golden.clone())),
            Some(RecordedLineage::Unreadable(reason)) => {
                return Err(TemporaryMachineError::LineageUnreadable {
                    machine: golden.clone(),
                    reason,
                });
            }
            Some(RecordedLineage::Known(lineage)) => match lineage.role {
                Role::Golden { template, .. } => template,
                Role::Working | Role::Temporary => {
                    return Err(TemporaryMachineError::NotGolden(golden.clone()));
                }
            },
        };
        let plan = ForkPlan::new(golden, &template, 1, &existing, running)?;
        let name = plan
            .names()
            .first()
            .cloned()
            .ok_or(TemporaryMachineError::Plan(PlanError::NoForks))?;
        let mac = self.macs.mac()?;
        let making = || -> Result<(), CallError> {
            let from = self.machines.claim(golden, self.claim_deadline())?;
            let target = self.machines.claim(&name, self.claim_deadline())?;
            Ok(from.fork_fresh(&template, &target, mac, Role::Temporary)?)
        };
        if let Err(error) = making() {
            self.remove_temporary_machine(&name);
            return Err(TemporaryMachineError::Making {
                name,
                source: Box::new(error),
            });
        }
        Ok(name)
    }

    /// Powers a temporary machine off and deletes it. One already gone
    /// counts as removed; one no longer temporary (its role was changed
    /// since, to keep it) is kept. One that can't be removed stays, for the
    /// daemon's next start.
    pub(crate) fn remove_temporary_machine(&self, machine: &MachineName) {
        let call = self.scheduler.alone();
        match self.power_off_and_delete(machine, &call) {
            Ok(Removed::Gone) => eprintln!("doppeld: {machine} removed: its connection closed"),
            Ok(Removed::Kept) => eprintln!("doppeld: {machine} kept: it is no longer temporary"),
            Err(error) => eprintln!(
                "doppeld: {machine} wasn't removed ({error}); the daemon tries again when it next starts"
            ),
        }
    }

    /// Removes every temporary machine, when the daemon starts: none
    /// survives its connection, and no connection survives a daemon, so
    /// each one found was left behind. It reads only the store: a VMM is
    /// reached only to power a temporary machine off.
    pub(crate) fn remove_leftover_temporary_machines(&self) {
        let store = self.machines.store();
        let listed = match store.machines() {
            Ok(listed) => listed,
            Err(error) => {
                eprintln!("doppeld: temporary machines left behind aren't removed: {error}");
                return;
            }
        };
        for entry in listed {
            match entry {
                Listed::Machine(name) => match store.read_lineage(&store.bundle(&name)) {
                    RecordedLineage::Known(lineage) if lineage.role == Role::Temporary => {
                        self.remove_temporary_machine(&name);
                    }
                    RecordedLineage::Known(_) | RecordedLineage::Unreadable(_) => {}
                },
                Listed::Unrecognized(_) => {}
            }
        }
    }

    /// Its role is read first, so a machine kept since it was made isn't
    /// deleted.
    fn power_off_and_delete(
        &self,
        machine: &MachineName,
        call: &Call,
    ) -> Result<Removed, CallError> {
        let gone = |error: &CallError| {
            matches!(
                error,
                CallError::Machine(MachineError::Store(StoreError::NoSuchMachine(_)))
            )
        };
        let store = self.machines.store();
        let bundle = store.bundle(machine);
        match store.read_spec(&bundle) {
            Ok(_) => {}
            Err(StoreError::NoSuchMachine(_)) => return Ok(Removed::Gone),
            Err(error) => return Err(CallError::Machine(MachineError::Store(error))),
        }
        match store.read_lineage(&bundle) {
            RecordedLineage::Known(lineage) if lineage.role == Role::Temporary => {}
            RecordedLineage::Known(_) | RecordedLineage::Unreadable(_) => return Ok(Removed::Kept),
        }
        match self.stop(machine, StopMode::Force, call) {
            Ok(_) => {}
            Err(CallError::Machine(MachineError::NotRunning(_))) => {}
            Err(error) if gone(&error) => return Ok(Removed::Gone),
            Err(error) => return Err(error),
        }
        match self.delete(machine, call) {
            Ok(()) => Ok(Removed::Gone),
            Err(error) if gone(&error) => Ok(Removed::Gone),
            Err(error) => Err(error),
        }
    }
}

/// What removing a temporary machine did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Removed {
    /// Deleted now, or already.
    Gone,
    /// Not temporary any more, or its role can't be read: left as it is.
    Kept,
}

/// How one fresh fork's start went.
#[derive(Debug)]
pub(crate) enum ForkStart {
    /// Started and at its desktop.
    Desktop(Desktop),
    Start(Unconfirmed<CallError>),
    /// Started, but its desktop didn't come.
    AwaitDesktop(CallError),
    /// The thread starting it panicked: whether it started is unknown.
    Interrupted,
}

impl<C, F> Daemon<C, F>
where
    C: Clock + Clone + Send + Sync,
    F: Filesystem + Sync,
{
    /// Makes `count` fresh forks of `source`'s checkpoint, under `source`'s
    /// turn, then starts them all at once, hidden, and waits for their
    /// desktops. Refused before anything is made when they don't fit
    /// beside the running machines.
    pub(crate) fn run_forks(
        &self,
        source: &MachineName,
        label: &CheckpointLabel,
        count: u32,
        call: &Call,
    ) -> Result<Vec<(MachineName, ForkStart)>, ForksError> {
        let names = {
            let _turn = self.turn(source, call).map_err(ForksError::Turn)?;
            let mut existing = Vec::new();
            let mut running = 0;
            for listing in self
                .machines
                .list(self.machines.deadline())
                .map_err(ForksError::Listing)?
            {
                match listing {
                    Listing::Machine { name, status, .. } => {
                        if matches!(status, Ok(Status::Live(_))) {
                            running += 1;
                        }
                        existing.push(name);
                    }
                    Listing::Unrecognized(_) => {}
                }
            }
            let plan = ForkPlan::new(source, label, count, &existing, running)?;
            let macs = plan
                .names()
                .iter()
                .map(|_| self.macs.mac())
                .collect::<Result<Vec<_>, _>>()?;
            let mut made = Vec::new();
            for (name, mac) in plan.names().iter().zip(macs) {
                let making = || -> Result<(), CallError> {
                    let from = self.machines.claim(source, self.claim_deadline())?;
                    let target = self.machines.claim(name, self.claim_deadline())?;
                    Ok(from.fork_fresh(label, &target, mac, Role::Working)?)
                };
                if let Err(error) = making() {
                    return Err(ForksError::Making {
                        made,
                        failed: name.clone(),
                        source: Box::new(error),
                    });
                }
                made.push(name.clone());
            }
            made
        };
        Ok(std::thread::scope(|scope| {
            let runs: Vec<_> = names
                .iter()
                .map(|name| scope.spawn(move || self.start_fork(name, call)))
                .collect();
            names
                .iter()
                .cloned()
                .zip(runs)
                .map(|(name, run)| (name, run.join().unwrap_or(ForkStart::Interrupted)))
                .collect()
        }))
    }

    fn start_fork(&self, name: &MachineName, call: &Call) -> ForkStart {
        if let Err(error) = self.start(name, Window::Hidden, call) {
            return ForkStart::Start(error);
        }
        match self.await_desktop(name, call) {
            Ok(outcome) => ForkStart::Desktop(outcome),
            Err(error) => ForkStart::AwaitDesktop(error),
        }
    }
}
