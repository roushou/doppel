//! One machine of the store, by name: what can be read of it without
//! owning it, and a connection to it while it runs.

use std::path::Path;

use crate::agent_install::plan::{AgentBuild, StepName};
use crate::golden::plan::GoldenStepName;
use doppel_desktop::deadline::Deadline;
use doppel_desktop::guest_user::GuestUserName;
use doppel_machine::bundle::{CheckpointLabel, CheckpointPresence};
use doppel_machine::login::{LoginOutcome, LoginPolicy};
use doppel_machine::machine::MachineName;
use doppel_machine::screen_probe::HostScreen;
use doppel_machine::vm::{NextStart, Status, Window};

use super::running::RunningMachine;
use super::{MachineError, Machines};
use crate::agent_install::{AgentInstallError, Installed};
use crate::bootstrap::{AgentArchive, BootstrapError, Bootstrapped, Progress};
use crate::capabilities::Clock;
use crate::files::Filesystem;
use crate::golden::GoldenPrepareError;
use crate::login::AwaitDesktopError;
use crate::vmm::VmmClient;
use crate::vmm::display::DisplayStream;
use doppel_protocol::transport::Cancel;

/// A machine, named: nothing about it is checked until asked. Changing it
/// takes a [`super::Claim`].
#[derive(Debug)]
pub struct Machine<'m, C, F> {
    machines: &'m Machines<C, F>,
    name: MachineName,
}

impl<C: Clock, F: Filesystem> Machines<C, F> {
    /// The machine called `name`, which may not exist.
    #[must_use]
    pub fn machine(&self, name: &MachineName) -> Machine<'_, C, F> {
        Machine {
            machines: self,
            name: name.clone(),
        }
    }
}

impl<'m, C: Clock, F: Filesystem> Machine<'m, C, F> {
    #[must_use]
    pub const fn name(&self) -> &MachineName {
        &self.name
    }

    /// Whether it runs, and if not how it will next start.
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn status(&self, by: Deadline) -> Result<Status, MachineError> {
        let store = self.machines.store();
        let bundle = store.bundle(&self.name);
        store.read_spec(&bundle)?;
        match self.machines.connect(&self.name, by)? {
            Some(mut client) => Ok(Status::Live(client.state(by)?)),
            None if bundle.suspended_state().exists() => {
                Ok(Status::Stopped(NextStart::ResumeSavedState))
            }
            None => Ok(Status::Stopped(NextStart::ColdBoot)),
        }
    }

    /// How its VMM shows it, while it runs: `None` when it doesn't run, or
    /// its VMM doesn't say.
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn window(&self, by: Deadline) -> Result<Option<Window>, MachineError> {
        match self.machines.connect(&self.name, by)? {
            Some(mut client) => Ok(client.window(by)?),
            None => Ok(None),
        }
    }

    /// Its checkpoints' labels, and the names of entries that aren't one.
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn checkpoints(
        &self,
    ) -> Result<Vec<Result<CheckpointLabel, std::ffi::OsString>>, MachineError> {
        let store = self.machines.store();
        let bundle = store.bundle(&self.name);
        store.read_spec(&bundle)?;
        Ok(store.checkpoints(&bundle)?)
    }

    /// Whether its checkpoint `label` is all there.
    ///
    /// # Errors
    /// Its files' existence couldn't be determined.
    pub fn checkpoint_presence(
        &self,
        label: &CheckpointLabel,
    ) -> Result<CheckpointPresence, MachineError> {
        let checkpoint = self.machines.store().bundle(&self.name).checkpoint(label);
        self.machines.checkpoint_presence(&checkpoint)
    }

    /// A connection to it while it runs.
    ///
    /// # Errors
    /// [`MachineError::NotRunning`], or a failed connection.
    pub fn running(&self, by: Deadline) -> Result<RunningMachine<'m, C>, MachineError> {
        let client = self.machines.running(&self.name, by)?;
        Ok(RunningMachine::new(
            self.name.clone(),
            client,
            self.machines.clock(),
        ))
    }

    /// A VMM connection of its own, timed by `clock` rather than borrowing
    /// the machines: for a long-lived user such as a console.
    ///
    /// # Errors
    /// [`MachineError::NotRunning`], or a failed connection.
    pub fn vmm<'c, K: Clock>(
        &self,
        clock: &'c K,
        by: Deadline,
    ) -> Result<VmmClient<'c, K>, MachineError> {
        // Through the usual connection: tells a stopped machine from a
        // leftover socket.
        drop(self.machines.running(&self.name, by)?);
        Ok(VmmClient::connect(
            &self.machines.store().socket(&self.name)?,
            clock,
            by,
        )?)
    }

    /// Brings the running guest to an active session: waits for the agent
    /// and, if the session is locked, types the stored password once. See
    /// [`doppel_machine::login`] for when it types and when it refuses to.
    ///
    /// # Errors
    /// [`AwaitDesktopError`]: a bad password file, the machine not
    /// running, host input failing, or the caller cancelling; every other
    /// result, ready or not, is a [`LoginOutcome`].
    pub fn await_desktop(
        &self,
        policy: LoginPolicy,
        cancel: &Cancel,
    ) -> Result<LoginOutcome, AwaitDesktopError> {
        self.machines.await_desktop(&self.name, policy, cancel)
    }

    /// Waits until screenshots show the guest's screen as it is now, not as
    /// it was before a resume from saved memory, with a probe drawn by the
    /// agent (see [`doppel_machine::screen_probe`]). Unknown when the agent
    /// can't draw one or a step fails: the [`HostScreen`] says.
    ///
    /// # Errors
    /// [`MachineError::NotRunning`], a failed VMM connection, or the caller
    /// cancelling (once the probe is hidden).
    pub fn catch_up_screen(&self, cancel: &Cancel) -> Result<HostScreen, MachineError> {
        self.machines.catch_up_screen(&self.name, cancel)
    }

    /// The build the running machine's agent says it is: `None` for a bare
    /// binary, which has none.
    ///
    /// # Errors
    /// [`MachineError`]: not running, or no agent answers.
    pub fn agent_build(&self, by: Deadline) -> Result<Option<AgentBuild>, MachineError> {
        let agent = self.machines.agent(&self.name, by)?;
        Ok(agent.peer_build().cloned())
    }

    /// Applies the golden settings to the running, logged-in machine.
    ///
    /// # Errors
    /// [`GoldenPrepareError`]: no stored password, no agent, or a failed
    /// step (named, with its stderr; never the password).
    pub fn golden_prepare(
        &self,
        progress: impl FnMut(GoldenStepName),
    ) -> Result<(), GoldenPrepareError> {
        self.machines.golden_prepare(&self.name, progress)
    }

    /// Installs `archive` (a zipped `Doppel Agent.app`) as the machine's
    /// LaunchAgent, through whatever agent answers now.
    ///
    /// # Errors
    /// [`AgentInstallError`]: no agent, a failed step, or no answer from the
    /// new build in time (its state is then unknown: look at its log).
    pub fn install_agent(
        &self,
        archive: Vec<u8>,
        build: AgentBuild,
        progress: impl FnMut(StepName),
    ) -> Result<Installed, AgentInstallError> {
        self.machines
            .install_agent(&self.name, archive, build, progress)
    }

    /// Gets an agent into a machine that may have none, then installs
    /// `archive` as its LaunchAgent (see [`crate::bootstrap`]).
    ///
    /// # Errors
    /// [`BootstrapError`]. A failure after the staging directory exists
    /// still tries to remove it.
    pub fn bootstrap_agent(
        &self,
        user: GuestUserName,
        archive: &AgentArchive,
        askpass: &Path,
        progress: impl FnMut(Progress),
    ) -> Result<Bootstrapped, BootstrapError> {
        self.machines
            .bootstrap_agent(&self.name, user, archive, askpass, progress)
    }

    /// Its display, on a VMM connection of its own (see [`Self::vmm`]).
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn display<'c, K: Clock>(
        &self,
        clock: &'c K,
        by: Deadline,
    ) -> Result<DisplayStream<'c, K>, MachineError> {
        let client = self.vmm(clock, by)?;
        Ok(DisplayStream::open(client, by)?)
    }
}
