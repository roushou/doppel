//! Machine lifecycle operations: what the CLI (and later doppeld) calls.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io;
use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant};

use crate::provision::Setup;
use doppel_machine::bundle::{
    CheckpointLabel, FileOp, GuestRepair, KeptAux, MachineBundle, SocketPath, Trash,
};
use doppel_machine::checkpoint::{
    Action, AfterCheckpoint, Checkpoint, CheckpointPlan, Event, GuestSync, IllegalTransition,
    SyncSkipped,
};
use doppel_machine::deadline::{Deadline, Remaining};
use doppel_machine::exec::{ExecError, ExecOutput, ExecRequest, Termination};
use doppel_machine::guest_path::GuestPathError;
use doppel_machine::lineage::{CheckpointKept, Lineage, Origin, RecordedLineage, Role};
use doppel_machine::login::{LoginOutcome, LoginPolicy};
use doppel_machine::machine::{
    Hardware, HardwareModel, MacAddress, MachineIdentifier, MachineName, MachineSpec, Platform,
    SpecError,
};
use doppel_machine::outcome::{Certainty, Failure, Outcome, Unconfirmed};
use doppel_machine::startup::{
    IllegalStartupEvent, Startup, StartupAction, StartupEvent, StartupFailure,
};
use doppel_machine::stop::{IllegalStopEvent, Stop, StopAction, StopEvent};
use doppel_machine::vm::{NextStart, RunState, Status};
use doppel_protocol::convert::ParseError;
use doppel_protocol::pb::vmm::v1 as vmm;
use nix::fcntl::Flock;

use crate::agent::{AgentClient, AgentError};
use crate::agent_grant::plan::AgentGrant;
use crate::agent_grant::{AgentGrantError, DiskImage};
use crate::capabilities::Clock;
use crate::display::DisplayError;
use crate::files::{FileOpError, Filesystem};
use crate::launcher::{Boot, LaunchError, VmmBinary, Window};
use crate::login::{AwaitDesktopError, PasswordFile, PasswordFileError};
use crate::store::{Listed, Store, StoreError};
use crate::vmm::{StopMode, VmmClient, VmmError};
use doppel_protocol::transport::Cancel;

mod agent_connection;
mod machine;
mod running;

pub use agent_connection::{AgentConnection, AgentConnectionError, Paused};
pub use machine::Machine;
pub use running::RunningMachine;

/// The budget of one ordinary request: see [`Machines::deadline`].
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Saving 4 GiB of memory takes up to 12 s; leave room for larger guests.
/// A guest with little pending data syncs in well under a second.
const SYNC_TIMEOUT: Duration = Duration::from_secs(30);
/// Sending the event returns at once; the shutdown follows.
const SESSION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const SAVE_TIMEOUT: Duration = Duration::from_secs(300);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(120);
/// Enough for [`Machines::claim`] to wait out a save a killed doppel left
/// running, and resume the guest it paused.
pub const CLAIM_TIMEOUT: Duration = CALL_TIMEOUT.saturating_mul(3).saturating_add(SAVE_TIMEOUT);
/// Enough for [`Claim::start`] to give each of its steps its own limit:
/// a few requests, then the guest coming up.
pub const START_TIMEOUT: Duration = CALL_TIMEOUT
    .saturating_mul(3)
    .saturating_add(STARTUP_TIMEOUT);
/// Enough for a graceful [`Claim::stop`] to ask the session, unlock it,
/// press the power button, and wait for the guest to shut down.
pub const STOP_TIMEOUT: Duration = CALL_TIMEOUT
    .saturating_mul(6)
    .saturating_add(SHUTDOWN_TIMEOUT);
/// Enough for [`Claim::checkpoint`] to sync the guest, pause it, save its
/// memory and resume it.
pub const CHECKPOINT_TIMEOUT: Duration = CALL_TIMEOUT
    .saturating_mul(4)
    .saturating_add(SYNC_TIMEOUT)
    .saturating_add(SAVE_TIMEOUT);
pub const FINISH_RECORDING_TIMEOUT: Duration = Duration::from_secs(30);

/// How [`Machines::resume`] found the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Resumed {
    Resumed,
    /// It was running already: nothing to do.
    AlreadyRunning,
}

#[derive(Debug, thiserror::Error)]
pub enum MachineError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Password(#[from] PasswordFileError),
    #[error(transparent)]
    Launch(#[from] LaunchError),
    #[error(transparent)]
    Vmm(#[from] VmmError),
    #[error(transparent)]
    Files(#[from] FileOpError),
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("machine '{0}' already exists")]
    AlreadyExists(MachineName),
    #[error("machine '{0}' has no macOS installed yet")]
    NotInstalled(MachineName),
    #[error("machine '{0}' is running; stop it first")]
    Running(MachineName),
    #[error("machine '{0}' is not running")]
    NotRunning(MachineName),
    #[error(
        "machine '{machine}' can't resume its saved state while '{running}' runs: they share a machine identity (forks of one machine), and VZ can't restore beside it. Stop '{running}' first"
    )]
    IdentityInUse {
        machine: MachineName,
        running: MachineName,
    },
    #[error("machine '{machine}' has no checkpoint '{label}'")]
    NoSuchCheckpoint { machine: MachineName, label: String },
    #[error("machine '{machine}' already has a checkpoint '{label}'")]
    CheckpointExists { machine: MachineName, label: String },
    #[error("machine '{machine}' keeps checkpoint '{label}': {why}")]
    CheckpointKept {
        machine: MachineName,
        label: CheckpointLabel,
        why: CheckpointKept,
    },
    #[error(
        "machine '{machine}' has a lineage file that can't be read ({reason}); its role can't be changed until it's fixed or removed"
    )]
    LineageUnreadable {
        machine: MachineName,
        reason: String,
    },
    #[error("{path} is empty or unreadable as a VZ platform blob")]
    InvalidBlob { path: PathBuf },
    #[error("doppel-vmm exited during startup ({status}). Last log lines:\n{log_tail}")]
    StartupFailed { status: String, log_tail: String },
    #[error("VM reported state {0} while starting. See the VMM log.")]
    StartupError(RunState),
    #[error("VM did not reach 'running' within {0:?}; its state is unknown. See the VMM log")]
    StartupTimeout(Duration),
    #[error("startup procedure: {0}")]
    StartupProcedure(#[from] IllegalStartupEvent),
    #[error("startup stopped short, in state {0}; the VM's state is unknown")]
    StartupStalled(String),
    #[error("stop procedure: {0}")]
    StopProcedure(#[from] IllegalStopEvent),
    #[error("stop stopped short, in state {0}; the VM's state is unknown")]
    StopStalled(String),
    #[error("checkpoint procedure: {0}")]
    Illegal(#[source] Box<IllegalTransition<MachineError>>),
    #[error(transparent)]
    Login(#[from] doppel_machine::login::IllegalLoginEvent),
    #[error(
        "cannot reach doppel-agent in '{machine}': {source}. Is it running in the guest's logged-in session?"
    )]
    AgentUnreachable {
        machine: MachineName,
        #[source]
        source: VmmError,
    },
    #[error("doppel-agent in '{machine}' didn't complete its handshake: {source}")]
    AgentHandshake {
        machine: MachineName,
        #[source]
        source: AgentError,
    },
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("VMM sent an invalid reply: {0}")]
    Parse(#[from] ParseError),
    #[error("invalid guest command: {0}")]
    GuestCommand(#[from] ExecError),
    #[error("invalid guest program: {0}")]
    GuestProgram(#[from] GuestPathError),
    #[error("cancelled")]
    Cancelled,
    #[error(
        "machine '{machine}' is still {state}: something it was doing hasn't finished. Check the VMM log"
    )]
    StillChanging {
        machine: MachineName,
        state: RunState,
    },
    #[error(transparent)]
    Display(#[from] DisplayError),
}

/// Boxed: the transition carries the procedure's errors, which are these.
impl From<IllegalTransition<Self>> for MachineError {
    fn from(error: IllegalTransition<Self>) -> Self {
        Self::Illegal(Box::new(error))
    }
}

/// Whether the operation that failed may have changed the machine. Checks
/// and local file operations fail without touching the VM. A timeout or a
/// broken exchange with the VMM or the agent leaves that unknown.
impl Failure for MachineError {
    fn certainty(&self) -> Certainty {
        match self {
            Self::Vmm(error) => error.certainty(),
            Self::Agent(error) => error.certainty(),
            Self::StartupTimeout(_)
            | Self::StartupProcedure(_)
            | Self::StartupStalled(_)
            | Self::StopProcedure(_)
            | Self::StopStalled(_)
            | Self::Illegal(_)
            | Self::Login(_)
            | Self::Parse(_)
            | Self::Cancelled
            | Self::StillChanging { .. } => Certainty::MaybeDone,
            Self::Store(_)
            | Self::Password(_)
            | Self::Launch(_)
            | Self::Files(_)
            | Self::Spec(_)
            | Self::Io { .. }
            | Self::AlreadyExists(_)
            | Self::NotInstalled(_)
            | Self::Running(_)
            | Self::NotRunning(_)
            | Self::IdentityInUse { .. }
            | Self::NoSuchCheckpoint { .. }
            | Self::CheckpointExists { .. }
            | Self::CheckpointKept { .. }
            | Self::LineageUnreadable { .. }
            | Self::InvalidBlob { .. }
            | Self::StartupFailed { .. }
            | Self::StartupError(_)
            | Self::AgentUnreachable { .. }
            | Self::AgentHandshake { .. }
            | Self::GuestCommand(_)
            | Self::GuestProgram(_)
            | Self::Display(_) => Certainty::NotDone,
        }
    }
}

/// An entry of the machines directory and, for a machine, its status or
/// why it couldn't be read, and its lineage.
#[derive(Debug)]
pub enum Listing {
    Machine {
        name: MachineName,
        status: Result<Status, MachineError>,
        lineage: RecordedLineage,
    },
    /// Not a valid machine name: ignored.
    Unrecognized(std::ffi::OsString),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    Stopped,
    /// Graceful shutdown was requested but the guest hasn't finished.
    ShutdownPending,
}

/// How the guest was asked to stop; when through the power button, why
/// the session couldn't be asked.
pub type StopRequest = doppel_machine::stop::StopRequest<SessionUnavailable>;

/// Why the logged-in session couldn't be asked to shut down.
#[derive(Debug, thiserror::Error)]
pub enum SessionUnavailable {
    /// No agent to ask: the login window, or a guest without one.
    #[error(transparent)]
    NotRun(MachineError),
    #[error("osascript {termination}: {stderr}")]
    Ended {
        termination: Termination,
        stderr: String,
    },
    /// It refused, and the screen is locked: unlocking it didn't work.
    #[error("the screen is locked: {0}")]
    Locked(LoginOutcome),
    #[error("the screen is locked, and unlocking it failed: {0}")]
    Unlock(#[source] Box<AwaitDesktopError>),
}

#[derive(Debug)]
#[must_use]
pub struct StopReport {
    pub request: StopRequest,
    pub outcome: StopOutcome,
}

/// The finished checkpoint procedure plus how long the guest was paused.
#[derive(Debug)]
pub struct CheckpointReport {
    pub result: Checkpoint<MachineError>,
    pub paused_for: Option<Duration>,
    /// Cleanup that ran after a failure, and how it went.
    pub cleanup: Vec<(Action, Outcome<(), MachineError>)>,
}

/// Proof that this process owns a machine: it holds the machine's lock, and
/// whatever an interrupted change left in its files has been settled. Every
/// operation that changes a machine takes one, so none runs unowned, and one
/// claim covers a sequence (stop, restore, start) with no gap between.
/// Dropping it releases the machine.
#[derive(Debug)]
#[must_use = "dropping a claim releases the machine"]
pub struct Claim<'m, C, F> {
    machines: &'m Machines<C, F>,
    bundle: MachineBundle,
    _lock: Flock<File>,
}

impl<C: Clock, F: Filesystem> Claim<'_, C, F> {
    #[must_use]
    pub fn name(&self) -> &MachineName {
        self.bundle.name()
    }

    /// Installs macOS into this new machine. A failed install leaves the
    /// bundle in place for diagnosis; it is never silently overwritten.
    ///
    /// With [`Setup::Provision`], the account's password becomes the
    /// machine's login password and the machine boots for the first time,
    /// the one boot on which macOS creates the account. It's then running
    /// (`Some`); with [`Setup::Assistant`] it's stopped (`None`). It takes
    /// no deadline: an install's length can't be known, and the first boot
    /// gets [`START_TIMEOUT`] from its end.
    ///
    /// # Errors
    /// See [`MachineError`]. A first boot whose outcome is unknown may
    /// have started: check before installing again.
    pub fn create(
        &self,
        hardware: Hardware,
        ipsw: &std::path::Path,
        setup: &Setup,
        progress: impl FnMut(f64),
    ) -> Outcome<Option<RunState>, MachineError> {
        self.machines.create(self, hardware, ipsw, setup, progress)
    }

    /// Adopts existing VZ machine files as this new machine. Disk and aux
    /// storage are cloned (copy-on-write), so the source is left untouched.
    /// The machine appears complete or not at all.
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn import(&self, hardware: Hardware, source: &ImportSource) -> Result<(), MachineError> {
        self.machines.import(self, hardware, source)
    }

    /// Starts the VMM and waits until the guest runs, through the
    /// [`Startup`] state machine. If the machine has a saved memory state,
    /// it resumes from it instead of booting. Give it [`START_TIMEOUT`].
    ///
    /// # Errors
    /// See [`MachineError`]. [`Unconfirmed::Unknown`] when a VMM may still
    /// be bringing the guest up.
    pub fn start(
        &self,
        window: Window,
        by: Deadline,
    ) -> Outcome<(RunState, NextStart), MachineError> {
        self.machines.start(self, window, by)
    }

    /// Stops the machine through the [`Stop`] state machine: forced, it is
    /// powered off; gracefully, its session is asked to shut down (see
    /// [`doppel_machine::stop`]). Give it [`STOP_TIMEOUT`].
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn stop(&self, mode: StopMode, by: Deadline) -> Result<StopReport, MachineError> {
        self.machines.stop(self, mode, by)
    }

    /// # Errors
    /// See [`MachineError`].
    pub fn pause(&self, by: Deadline) -> Result<(), MachineError> {
        self.machines.pause(self, by)
    }

    /// # Errors
    /// See [`MachineError`].
    pub fn resume(&self, by: Deadline) -> Result<Resumed, MachineError> {
        self.machines.resume(self, by)
    }

    /// Takes a checkpoint of the running machine, through the core
    /// checkpoint state machine. Give it [`CHECKPOINT_TIMEOUT`].
    ///
    /// # Errors
    /// Setup problems only. Procedure failures are reported in the
    /// [`CheckpointReport`], as are unknown outcomes.
    pub fn checkpoint(
        &self,
        label: &CheckpointLabel,
        then: AfterCheckpoint,
        by: Deadline,
    ) -> Result<CheckpointReport, MachineError> {
        self.machines.checkpoint(self, label, then, by)
    }

    /// Rolls the stopped machine back to one of its checkpoints, discarding
    /// its current disk.
    ///
    /// # Errors
    /// See [`MachineError`]. [`Unconfirmed::Unknown`] once the restore's
    /// commit marker may exist: the next claim of the machine finishes it.
    pub fn restore(&self, label: &CheckpointLabel, by: Deadline) -> Outcome<(), MachineError> {
        self.machines.restore(self, label, by)
    }

    /// Deletes one of the machine's checkpoints, running or not, unless the
    /// machine keeps it as its golden image's template.
    ///
    /// # Errors
    /// [`StoreError::NoSuchMachine`], [`MachineError::NoSuchCheckpoint`],
    /// [`MachineError::CheckpointKept`], or a failure moving or removing its
    /// files; a failure after the move leaves it gone from the list, and the
    /// next claim removes the rest.
    pub fn delete_checkpoint(&self, label: &CheckpointLabel) -> Result<(), MachineError> {
        self.machines.delete_checkpoint(self, label)
    }

    /// Makes `into` from this machine's checkpoint `label`. It resumes
    /// exactly at the checkpoint when started, sharing this machine's
    /// identifier and MAC address: VZ refuses to restore with new ones.
    ///
    /// # Errors
    /// See [`MachineError`].
    pub fn fork(&self, label: &CheckpointLabel, into: &Self) -> Result<(), MachineError> {
        self.machines.fork(self, label, into)
    }

    /// Makes `into` a fresh fork of this machine's checkpoint `label` in
    /// `role`: its disk, login and identity at `mac`, with no memory state,
    /// so it cold-boots and can run beside this one and other fresh forks.
    /// Its role is written with it, so it never appears without one.
    ///
    /// # Errors
    /// See [`MachineError`]. Interrupted, it leaves a staging directory the
    /// next claim of `into` removes; `into` appears whole or not at all.
    pub fn fork_fresh(
        &self,
        label: &CheckpointLabel,
        into: &Self,
        mac: MacAddress,
        role: Role,
    ) -> Result<(), MachineError> {
        self.machines.fork_fresh(self, label, into, mac, role)
    }

    /// Makes this machine a golden image whose new machines start from
    /// `template`, one of its checkpoints. Its origin stays.
    ///
    /// # Errors
    /// [`MachineError::NoSuchCheckpoint`], [`MachineError::LineageUnreadable`]
    /// (rewriting it would lose an origin nobody could read), or I/O.
    pub fn mark_golden(&self, template: &CheckpointLabel) -> Result<Lineage, MachineError> {
        self.machines.mark_golden(self, template)
    }

    /// Writes `grant` into this machine's disk: the guest boots with the
    /// agent's Accessibility and Screen Recording, and no prompt is answered
    /// by a person. The machine must be stopped with no saved memory, which
    /// expects its disk as it was.
    ///
    /// # Errors
    /// [`AgentGrantError`]: running, saved memory, no such user, a record
    /// of a shape this Doppel doesn't know (nothing is written then), or
    /// the disk tools failing.
    pub fn grant_agent(&self, grant: &AgentGrant, by: Deadline) -> Result<(), AgentGrantError> {
        let machines = self.machines;
        let bundle = &self.bundle;
        let name = bundle.name();
        machines
            .store
            .read_spec(bundle)
            .map_err(MachineError::from)?;
        if machines.connect(name, machines.step(by))?.is_some() {
            return Err(MachineError::Running(name.clone()).into());
        }
        if machines
            .files
            .exists(&bundle.suspended_state())
            .map_err(MachineError::from)?
        {
            return Err(AgentGrantError::SavedState(name.clone()));
        }
        let mount = machines
            .store
            .runtime()
            .path()
            .join(format!("grant-{name}"));
        grant.apply(name, DiskImage::at(&bundle.image().disk), &mount)
    }

    /// Makes a golden image a working machine again. Its origin stays.
    ///
    /// # Errors
    /// [`MachineError::LineageUnreadable`], or I/O.
    pub fn mark_working(&self) -> Result<Lineage, MachineError> {
        self.machines.mark_working(self)
    }

    /// Deletes the stopped machine, its disk and checkpoints, for good. The
    /// claim is used up: the machine is gone. Its recordings stay.
    ///
    /// # Errors
    /// [`StoreError::NoSuchMachine`], [`MachineError::Running`], or a
    /// failure moving or removing its files; a failure after the move
    /// leaves the machine gone from the list, its files in `deleting/`.
    pub fn delete(self, by: Deadline) -> Result<(), MachineError> {
        let machines = self.machines;
        machines.delete(self, by)
    }
}

/// Existing VZ machine files to adopt (for example from another tool).
#[derive(Debug, Clone)]
pub struct ImportSource {
    pub hardware_model: PathBuf,
    pub machine_identifier: PathBuf,
    pub aux: PathBuf,
    pub disk: PathBuf,
}

/// A VMM process being started, and what a startup has of it so far.
struct Launched<'c, C> {
    child: Child,
    socket: SocketPath,
    client: Option<VmmClient<'c, C>>,
    started: Instant,
    by: Deadline,
}

impl<'c, C: Clock> Launched<'c, C> {
    /// The connection, which the startup makes before it reads anything.
    fn client(&mut self, bundle: &MachineBundle) -> Result<&mut VmmClient<'c, C>, MachineError> {
        self.client
            .as_mut()
            .ok_or_else(|| MachineError::NotRunning(bundle.name().clone()))
    }
}

#[derive(Debug)]
pub struct Machines<C, F> {
    store: Store,
    vmm: VmmBinary,
    clock: C,
    files: F,
}

impl<C: Clock, F: Filesystem> Machines<C, F> {
    #[must_use]
    /// The clock this instance was given.
    pub const fn clock(&self) -> &C {
        &self.clock
    }

    pub const fn new(store: Store, vmm: VmmBinary, clock: C, files: F) -> Self {
        Self {
            store,
            vmm,
            clock,
            files,
        }
    }

    #[must_use]
    pub const fn store(&self) -> &Store {
        &self.store
    }

    /// When an ordinary request must have finished: [`CALL_TIMEOUT`] from
    /// now. Requests that are slow by nature get their own budget.
    #[must_use]
    pub fn deadline(&self) -> Deadline {
        self.within(CALL_TIMEOUT)
    }

    /// `budget` from now.
    #[must_use]
    pub fn within(&self, budget: Duration) -> Deadline {
        Deadline::after(self.clock.now(), budget)
    }

    /// A step of an operation due `by`: it ends by then, and within its
    /// own `limit`.
    fn capped(&self, by: Deadline, limit: Duration) -> Deadline {
        by.earlier(self.within(limit))
    }

    /// An ordinary request inside an operation due `by`.
    fn step(&self, by: Deadline) -> Deadline {
        self.capped(by, CALL_TIMEOUT)
    }

    /// Takes ownership of a machine name, which need not exist yet, and
    /// settles what an interrupted change left behind (see
    /// [`doppel_machine::bundle::Leftovers`]).
    ///
    /// # Errors
    /// [`StoreError::Busy`] while another process holds it, or a failure
    /// settling its files.
    pub fn claim(&self, name: &MachineName, by: Deadline) -> Result<Claim<'_, C, F>, MachineError> {
        let bundle = self.store.bundle(name);
        let lock = self.store.lock(&bundle)?;
        // A VMM outlives a killed doppel: let what it was doing (a save into
        // the checkpoint staging) finish before settling its files.
        let state = self.settled_state(name, by)?;
        let leftovers = bundle.leftovers();
        let present = self.files.present(leftovers.probes())?;
        self.files.apply_all(&leftovers.settle(&present))?;
        match leftovers.guest(&present, state) {
            GuestRepair::Leave => {}
            GuestRepair::Resume => {
                let by = self.step(by);
                self.running(name, by)?.resume(by)?;
            }
        }
        Ok(Claim {
            machines: self,
            bundle,
            _lock: lock,
        })
    }

    /// The VM's state once it has left any transient one (`None`: not
    /// running), waiting until `by` for a save to finish.
    fn settled_state(
        &self,
        name: &MachineName,
        by: Deadline,
    ) -> Result<Option<RunState>, MachineError> {
        let Some(mut client) = self.connect(name, self.step(by))? else {
            return Ok(None);
        };
        let mut state = client.state(self.step(by))?;
        let by = self.capped(by, SAVE_TIMEOUT);
        while state.is_transient() {
            match client.next_event(by) {
                Ok(vmm::VmmEvent {
                    kind: Some(vmm::vmm_event::Kind::StateChanged(changed)),
                }) => state = changed.state(),
                Ok(_) => {}
                // The VMM exits once the VM has stopped.
                Err(VmmError::Closed) => return Ok(None),
                Err(VmmError::Timeout) => {
                    return Err(MachineError::StillChanging {
                        machine: name.clone(),
                        state,
                    });
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Some(state))
    }

    fn absent(&self, bundle: &MachineBundle) -> Result<(), MachineError> {
        if self.files.exists(bundle.dir())? {
            Err(MachineError::AlreadyExists(bundle.name().clone()))
        } else {
            Ok(())
        }
    }

    /// Installs macOS into a new machine. A failed install leaves the bundle
    /// in place for diagnosis; it is never silently overwritten.
    ///
    /// With [`Setup::Provision`], the account's password becomes the
    /// machine's login password and the machine boots for the first time,
    /// the one boot on which macOS creates the account. It's then running
    /// (`Some`); with [`Setup::Assistant`] it's stopped (`None`).
    ///
    /// # Errors
    /// See [`MachineError`]. A first boot whose outcome is unknown may
    /// have started: check before installing again.
    fn create(
        &self,
        claim: &Claim<'_, C, F>,
        hardware: Hardware,
        ipsw: &std::path::Path,
        setup: &Setup,
        progress: impl FnMut(f64),
    ) -> Outcome<Option<RunState>, MachineError> {
        self.install(claim, hardware, ipsw, progress)?;
        let bundle = &claim.bundle;
        match setup {
            Setup::Assistant => Ok(None),
            Setup::Provision(account) => {
                let password_file = PasswordFile
                    .write(bundle, &account.password)
                    .map_err(MachineError::from)?;
                let boot = Boot::Provision {
                    full_name: account.full_name.clone(),
                    user: account.user.clone(),
                    password_file,
                };
                // The install takes as long as it takes, so the first
                // boot's deadline starts once it's done.
                let by = self.within(START_TIMEOUT);
                self.bring_up(bundle, &boot, Window::Hidden, by).map(Some)
            }
        }
    }

    fn install(
        &self,
        claim: &Claim<'_, C, F>,
        hardware: Hardware,
        ipsw: &std::path::Path,
        progress: impl FnMut(f64),
    ) -> Result<(), MachineError> {
        let name = claim.name();
        let bundle = &claim.bundle;
        self.absent(bundle)?;
        self.files.apply(&FileOp::CreateDir {
            path: bundle.dir().to_owned(),
        })?;
        self.store
            .write_lineage(&bundle.lineage(), &Lineage::made(Origin::Installed))?;
        let spec = MachineSpec::new(hardware, Platform::NotInstalled)?;
        self.store.write_spec(&bundle.image().spec, &spec)?;
        self.vmm.install(&bundle.image(), ipsw, progress)?;
        // Parse what the installer wrote: it must now be installed.
        match self.store.read_spec(bundle)?.platform() {
            Platform::Installed { .. } => Ok(()),
            Platform::NotInstalled => Err(MachineError::NotInstalled(name.clone())),
        }
    }

    /// Adopts existing VZ machine files as a new machine. Disk and aux storage
    /// are cloned (copy-on-write), so the source is left untouched. The
    /// machine appears complete or not at all.
    ///
    /// # Errors
    /// See [`MachineError`].
    fn import(
        &self,
        claim: &Claim<'_, C, F>,
        hardware: Hardware,
        source: &ImportSource,
    ) -> Result<(), MachineError> {
        let read = |path: &PathBuf| {
            fs::read(path).map_err(|source| MachineError::Io {
                path: path.clone(),
                source,
            })
        };
        let model = HardwareModel::new(read(&source.hardware_model)?).ok_or_else(|| {
            MachineError::InvalidBlob {
                path: source.hardware_model.clone(),
            }
        })?;
        let identifier =
            MachineIdentifier::new(read(&source.machine_identifier)?).ok_or_else(|| {
                MachineError::InvalidBlob {
                    path: source.machine_identifier.clone(),
                }
            })?;
        let spec = MachineSpec::new(
            hardware,
            Platform::Installed {
                hardware: model,
                identifier,
            },
        )?;
        self.absent(&claim.bundle)?;
        let assembly = claim.bundle.import_from(&source.aux, &source.disk);
        self.files.apply_all(&assembly.build)?;
        self.store.write_spec(&assembly.spec, &spec)?;
        self.store
            .write_lineage(&assembly.lineage, &Lineage::made(Origin::Imported))?;
        self.files.apply(&assembly.commit)?;
        Ok(())
    }

    fn connect(
        &self,
        name: &MachineName,
        by: Deadline,
    ) -> Result<Option<VmmClient<'_, C>>, MachineError> {
        let socket = self.store.socket(name)?;
        if !socket.path().exists() {
            return Ok(None);
        }
        match VmmClient::connect(&socket, &self.clock, by) {
            Ok(client) => Ok(Some(client)),
            // A leftover socket from a VMM that's gone.
            Err(VmmError::Connect(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn running(&self, name: &MachineName, by: Deadline) -> Result<VmmClient<'_, C>, MachineError> {
        self.connect(name, by)?
            .ok_or_else(|| MachineError::NotRunning(name.clone()))
    }

    /// Every machine, each one's status read by `by`.
    ///
    /// # Errors
    /// Unreadable machines directory.
    pub fn list(&self, by: Deadline) -> Result<Vec<Listing>, MachineError> {
        Ok(self
            .store
            .machines()?
            .into_iter()
            .map(|listed| match listed {
                Listed::Machine(name) => Listing::Machine {
                    status: self.machine(&name).status(by),
                    lineage: self.store.read_lineage(&self.store.bundle(&name)),
                    name,
                },
                Listed::Unrecognized(raw) => Listing::Unrecognized(raw),
            })
            .collect())
    }

    /// Starts the VMM and waits until the guest runs. If the machine has a
    /// saved memory state, it resumes from it instead of booting.
    ///
    /// # Errors
    /// See [`MachineError`]. [`Unconfirmed::Unknown`] when a VMM may still
    /// be bringing the guest up.
    fn start(
        &self,
        claim: &Claim<'_, C, F>,
        window: Window,
        by: Deadline,
    ) -> Outcome<(RunState, NextStart), MachineError> {
        let name = claim.name();
        let bundle = &claim.bundle;
        let spec = self.store.read_spec(bundle).map_err(MachineError::from)?;
        let identifier = match spec.platform() {
            Platform::Installed { identifier, .. } => identifier,
            Platform::NotInstalled => {
                return Err(MachineError::NotInstalled(name.clone()).into());
            }
        };
        if self.connect(name, self.step(by))?.is_some() {
            return Err(MachineError::Running(name.clone()).into());
        }
        let suspended = bundle.suspended_state();
        let attempt = bundle.resume_attempt();
        let kept = if attempt.kept().exists() {
            KeptAux::Present
        } else {
            KeptAux::Absent
        };
        if !suspended.exists() {
            if let Some(op) = attempt.discard(kept) {
                self.files.apply(&op).map_err(MachineError::from)?;
            }
            let state = self.bring_up(bundle, &Boot::ColdBoot, window, by)?;
            return Ok((state, NextStart::ColdBoot));
        }

        if let Some(running) = self.running_with(identifier, name, by)? {
            return Err(MachineError::IdentityInUse {
                machine: name.clone(),
                running,
            }
            .into());
        }
        self.files
            .apply_all(&attempt.begin(kept))
            .map_err(MachineError::from)?;
        match self.bring_up(bundle, &Boot::ConsumeState(suspended), window, by) {
            Ok(state) => {
                // Not fatal if it fails: the guest runs and its state file is gone,
                // so the next start is a cold boot, which removes a leftover copy.
                let _ = self.files.apply(&attempt.succeeded());
                Ok((state, NextStart::ResumeSavedState))
            }
            Err(Unconfirmed::Failed(error)) => {
                self.files
                    .apply(&attempt.failed())
                    .map_err(MachineError::from)?;
                Err(Unconfirmed::Failed(error))
            }
            // The VMM may still be restoring: keep the copy. The next start puts it
            // back if the state file is still there (see `ResumeAttempt`).
            Err(unknown @ Unconfirmed::Unknown(_)) => Err(unknown),
        }
    }

    /// Another running machine with this identifier. Forks share their source's
    /// identifier, and VZ can't restore a saved state while one of them runs
    /// (cold boots side by side work). Machines whose spec can't be read are
    /// skipped: they can't be started either.
    fn running_with(
        &self,
        identifier: &MachineIdentifier,
        except: &MachineName,
        by: Deadline,
    ) -> Result<Option<MachineName>, MachineError> {
        for listed in self.store.machines()? {
            let other = match listed {
                Listed::Machine(other) if other != *except => other,
                Listed::Machine(_) | Listed::Unrecognized(_) => continue,
            };
            let Ok(spec) = self.store.read_spec(&self.store.bundle(&other)) else {
                continue;
            };
            let same = match spec.platform() {
                Platform::Installed {
                    identifier: theirs, ..
                } => theirs == identifier,
                Platform::NotInstalled => false,
            };
            if same && self.connect(&other, self.step(by))?.is_some() {
                return Ok(Some(other));
            }
        }
        Ok(None)
    }

    /// Launches the VMM and waits until the guest runs, driving the
    /// [`Startup`] state machine. A failure is [`Unconfirmed::Failed`] only
    /// once the VMM process is known to be gone, so nothing is using the
    /// machine's files.
    fn bring_up(
        &self,
        bundle: &MachineBundle,
        boot: &Boot,
        window: Window,
        by: Deadline,
    ) -> Outcome<RunState, MachineError> {
        let socket = self
            .store
            .socket(bundle.name())
            .map_err(|e| Unconfirmed::Failed(e.into()))?;
        let child = self
            .vmm
            .serve(&bundle.image(), &socket, boot, window, &bundle.log())
            .map_err(|e| Unconfirmed::Failed(e.into()))?;
        let mut launched = Launched {
            child,
            socket,
            client: None,
            started: self.clock.now(),
            by: self.capped(by, STARTUP_TIMEOUT),
        };
        let (mut startup, actions) = Startup::begin();
        let mut queue: VecDeque<StartupAction> = actions.into();
        while let Some(action) = queue.pop_front() {
            let event = self.perform_startup(&mut launched, bundle, action);
            let (next, more) = startup
                .on(event)
                .map_err(|error| Unconfirmed::Unknown(error.into()))?;
            startup = next;
            queue.extend(more);
        }
        let waited = self.clock.now().saturating_duration_since(launched.started);
        let failure = |failure| self.startup_error(failure, bundle, waited);
        match startup {
            Startup::Running(state) => Ok(state),
            Startup::Failed(failed) => Err(Unconfirmed::Failed(failure(failed))),
            Startup::Unknown(unknown) => Err(Unconfirmed::Unknown(failure(unknown))),
            // Every state short of its end asks for an action.
            state @ (Startup::Connecting
            | Startup::Connected
            | Startup::Waiting(_)
            | Startup::StoppingVmm(_)) => Err(Unconfirmed::Unknown(MachineError::StartupStalled(
                format!("{state:?}"),
            ))),
        }
    }

    /// Performs one action of a startup: what came of it.
    fn perform_startup<'m>(
        &'m self,
        launched: &mut Launched<'m, C>,
        bundle: &MachineBundle,
        action: StartupAction,
    ) -> StartupEvent<MachineError> {
        let by = launched.by;
        let passed = || matches!(by.remaining(self.clock.now()), Remaining::Passed);
        match action {
            StartupAction::Connect | StartupAction::ConnectAgain => {
                if action == StartupAction::ConnectAgain {
                    if passed() {
                        return StartupEvent::TimedOut;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                match launched.child.try_wait() {
                    Ok(Some(status)) => {
                        return StartupEvent::VmmExited {
                            status: status.to_string(),
                        };
                    }
                    Ok(None) => {}
                    Err(source) => {
                        return StartupEvent::Broken(MachineError::Io {
                            path: bundle.log(),
                            source,
                        });
                    }
                }
                match VmmClient::connect(&launched.socket, &self.clock, by) {
                    Ok(client) => {
                        launched.client = Some(client);
                        StartupEvent::Connected
                    }
                    Err(VmmError::Connect(_)) if passed() => StartupEvent::TimedOut,
                    Err(VmmError::Connect(_)) => StartupEvent::NotListening,
                    Err(VmmError::Timeout) => StartupEvent::TimedOut,
                    Err(e) => StartupEvent::Broken(e.into()),
                }
            }
            StartupAction::ReadState => match launched.client(bundle).map(|c| c.state(by)) {
                Ok(Ok(state)) => StartupEvent::State(state),
                Ok(Err(e)) => StartupEvent::Broken(e.into()),
                Err(e) => StartupEvent::Broken(e),
            },
            StartupAction::AwaitStateChange => {
                let client = match launched.client(bundle) {
                    Ok(client) => client,
                    Err(e) => return StartupEvent::Broken(e),
                };
                loop {
                    match client.next_event(by) {
                        Ok(vmm::VmmEvent {
                            kind: Some(vmm::vmm_event::Kind::StateChanged(state)),
                        }) => {
                            return StartupEvent::State(state.state());
                        }
                        Ok(_) => {}
                        Err(VmmError::Timeout) => return StartupEvent::TimedOut,
                        Err(VmmError::Closed) => return StartupEvent::Closed,
                        Err(e) => return StartupEvent::Broken(e.into()),
                    }
                }
            }
            StartupAction::StopVmm => match launched.child.kill().and(launched.child.wait()) {
                Ok(_) => StartupEvent::VmmStopped,
                Err(_) => StartupEvent::VmmNotStopped,
            },
        }
    }

    /// The error a startup that didn't end running reports.
    fn startup_error(
        &self,
        failure: StartupFailure<MachineError>,
        bundle: &MachineBundle,
        waited: Duration,
    ) -> MachineError {
        match failure {
            StartupFailure::VmmExited { status } => MachineError::StartupFailed {
                status,
                log_tail: Self::log_tail(bundle),
            },
            StartupFailure::VmEnded(state) => MachineError::StartupError(state),
            StartupFailure::ConnectionClosed => MachineError::StartupFailed {
                status: "connection closed".into(),
                log_tail: Self::log_tail(bundle),
            },
            StartupFailure::TimedOut => MachineError::StartupTimeout(waited),
            StartupFailure::Broken(error) => error,
        }
    }

    fn log_tail(bundle: &MachineBundle) -> String {
        let log = fs::read_to_string(bundle.log()).unwrap_or_default();
        let lines: Vec<&str> = log.lines().collect();
        lines[lines.len().saturating_sub(15)..].join("\n")
    }

    /// Stops the machine, driving the [`Stop`] state machine: forced, it is
    /// powered off; gracefully, its session is asked to shut down (see
    /// [`doppel_machine::stop`]).
    ///
    /// # Errors
    /// See [`MachineError`].
    fn stop(
        &self,
        claim: &Claim<'_, C, F>,
        mode: StopMode,
        by: Deadline,
    ) -> Result<StopReport, MachineError> {
        let name = claim.name();
        let mut client = self.running(name, self.step(by))?;
        let (mut stop, actions) = Stop::begin(mode);
        let mut queue: VecDeque<StopAction> = actions.into();
        while let Some(action) = queue.pop_front() {
            let event = self.perform_stop(name, &mut client, action, by);
            let (next, more) = stop.on(event)?;
            stop = next;
            queue.extend(more);
        }
        match stop {
            Stop::Stopped(request) => Ok(StopReport {
                request,
                outcome: StopOutcome::Stopped,
            }),
            Stop::ShutdownPending(request) => Ok(StopReport {
                request,
                outcome: StopOutcome::ShutdownPending,
            }),
            Stop::Failed(error) => Err(error),
            // Every state short of its end asks for an action.
            state @ (Stop::PoweringOff
            | Stop::AskingSession(_)
            | Stop::Unlocking { .. }
            | Stop::PressingPowerButton { .. }
            | Stop::AwaitingStop(_)) => Err(MachineError::StopStalled(format!("{state:?}"))),
        }
    }

    /// Performs one action of a stop: what came of it.
    fn perform_stop(
        &self,
        name: &MachineName,
        client: &mut VmmClient<'_, C>,
        action: StopAction,
        by: Deadline,
    ) -> StopEvent<SessionUnavailable, MachineError> {
        match action {
            StopAction::PowerOff => match client.stop(StopMode::Force, self.step(by)) {
                // A forced stop may close the connection before replying.
                Ok(()) | Err(VmmError::Closed) => StopEvent::PoweredOff,
                Err(e) => StopEvent::Broken(e.into()),
            },
            StopAction::AskSession => match self.shut_down_session(name, by) {
                Ok(()) => StopEvent::SessionAsked,
                Err(refused @ SessionUnavailable::Ended { .. }) => {
                    StopEvent::SessionRefused(refused)
                }
                Err(
                    unavailable @ (SessionUnavailable::NotRun(_)
                    | SessionUnavailable::Locked(_)
                    | SessionUnavailable::Unlock(_)),
                ) => StopEvent::SessionUnavailable(unavailable),
            },
            // A locked screen refuses to shut down, and the lock screen
            // ignores the power button: unlock it as `start` does.
            StopAction::Unlock => {
                match self.await_desktop(name, LoginPolicy::DEFAULT, &Cancel::never()) {
                    Ok(LoginOutcome::Unlocked { after }) => StopEvent::Unlocked { after },
                    Ok(LoginOutcome::AlreadyActive { .. }) => StopEvent::NotLocked,
                    Ok(
                        outcome @ (LoginOutcome::NeedsPassword
                        | LoginOutcome::PasswordNotAccepted
                        | LoginOutcome::NoAgent { .. }
                        | LoginOutcome::Unknown { .. }),
                    ) => StopEvent::NotUnlocked(SessionUnavailable::Locked(outcome)),
                    Err(error) => {
                        StopEvent::NotUnlocked(SessionUnavailable::Unlock(Box::new(error)))
                    }
                }
            }
            StopAction::PressPowerButton => match client.stop(StopMode::Graceful, self.step(by)) {
                Ok(()) => StopEvent::PowerButtonPressed,
                Err(e) => StopEvent::Broken(e.into()),
            },
            // The VMM exits once the VM has stopped, closing the connection.
            StopAction::AwaitStop => {
                let by = self.capped(by, SHUTDOWN_TIMEOUT);
                loop {
                    match client.next_event(by) {
                        Ok(_) => {}
                        Err(VmmError::Closed) => return StopEvent::VmStopped,
                        Err(VmmError::Timeout) => return StopEvent::DeadlinePassed,
                        Err(e) => return StopEvent::Broken(e.into()),
                    }
                }
            }
        }
    }

    /// Asks the logged-in session to shut down, as the Apple menu's Shut
    /// Down does but without its confirmation (loginwindow's "really shut
    /// down" event, through `osascript` run by the agent). Apps still quit
    /// normally, so one with unsaved changes can hold the shutdown up.
    fn shut_down_session(
        &self,
        name: &MachineName,
        by: Deadline,
    ) -> Result<(), SessionUnavailable> {
        let output = self
            .run_in_guest(
                name,
                "/usr/bin/osascript",
                vec![
                    "-e".into(),
                    "tell application \"loginwindow\" to «event aevtrsdn»".into(),
                ],
                SESSION_SHUTDOWN_TIMEOUT,
                by,
            )
            .map_err(SessionUnavailable::NotRun)?;
        match output.termination {
            Termination::Exited(0) => Ok(()),
            termination @ (Termination::Exited(_)
            | Termination::Signaled(_)
            | Termination::TimedOut) => Err(SessionUnavailable::Ended {
                termination,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            }),
        }
    }

    /// A fresh connection to the agent in a running machine, for the
    /// shell's own housekeeping (see [`RunningMachine::agent`]).
    pub(crate) fn agent(
        &self,
        name: &MachineName,
        by: Deadline,
    ) -> Result<AgentClient<&C>, MachineError> {
        self.machine(name).running(by)?.agent(by)
    }

    /// Runs a program in the guest through the agent, with a small output
    /// limit: for the shell's own housekeeping commands.
    fn run_in_guest(
        &self,
        name: &MachineName,
        program: &str,
        arguments: Vec<String>,
        timeout: Duration,
        by: Deadline,
    ) -> Result<ExecOutput, MachineError> {
        let request = ExecRequest::new(program.parse()?, arguments, timeout, 4 << 10)?;
        Ok(self
            .agent(name, self.step(by))?
            .exec(&request, &Cancel::never())?)
    }

    /// # Errors
    /// See [`MachineError`].
    fn pause(&self, claim: &Claim<'_, C, F>, by: Deadline) -> Result<(), MachineError> {
        let by = self.step(by);
        Ok(self.running(claim.name(), by)?.pause(by)?)
    }

    /// # Errors
    /// See [`MachineError`].
    fn resume(&self, claim: &Claim<'_, C, F>, by: Deadline) -> Result<Resumed, MachineError> {
        let by = self.step(by);
        let mut client = self.running(claim.name(), by)?;
        // The claim itself resumes a guest an interrupted checkpoint left
        // paused, so a resume asked for after one finds it running.
        match client.state(by)? {
            RunState::Running => Ok(Resumed::AlreadyRunning),
            RunState::Starting
            | RunState::Pausing
            | RunState::Paused
            | RunState::Resuming
            | RunState::Saving
            | RunState::Restoring
            | RunState::Stopping
            | RunState::Stopped
            | RunState::Error
            | RunState::Unrecognized(_) => {
                client.resume(by)?;
                Ok(Resumed::Resumed)
            }
        }
    }

    /// Runs the core checkpoint state machine against a live VM.
    ///
    /// # Errors
    /// Setup problems only. Procedure failures are reported in the
    /// [`CheckpointReport`], as are unknown outcomes.
    fn checkpoint(
        &self,
        claim: &Claim<'_, C, F>,
        label: &CheckpointLabel,
        then: AfterCheckpoint,
        by: Deadline,
    ) -> Result<CheckpointReport, MachineError> {
        let name = claim.name();
        let bundle = &claim.bundle;
        if self.files.exists(&bundle.checkpoint(label).dir)? {
            return Err(MachineError::CheckpointExists {
                machine: name.clone(),
                label: label.as_str().to_owned(),
            });
        }
        let mut client = self.running(name, self.step(by))?;
        let (mut state, actions) = Checkpoint::begin(CheckpointPlan::new(bundle, label, then));
        let mut queue: VecDeque<Action> = actions.into();
        let mut paused_at = None;
        let mut paused_for = None;
        let mut cleanup = Vec::new();

        while let Some(action) = queue.pop_front() {
            let answer = self.perform(name, &mut client, &action, by);
            match (&action, &answer) {
                (Action::PauseVm, Ok(Event::Paused)) => paused_at = Some(self.clock.now()),
                (Action::ResumeVm, Ok(Event::Resumed)) | (Action::StopVm, Ok(Event::Stopped)) => {
                    paused_for = paused_at.map(|at| self.clock.now().saturating_duration_since(at));
                }
                _ => {}
            }
            if state.is_terminal() {
                // Cleanup requested by a failure; report, don't transition.
                cleanup.push((action, answer.map(drop)));
                continue;
            }
            let (next, more) = state.on(answer)?;
            state = next;
            queue.extend(more);
        }
        Ok(CheckpointReport {
            result: state,
            paused_for,
            cleanup,
        })
    }

    /// Performs one action of the procedure. Each error says for itself
    /// whether the action may have happened (see [`MachineError`]'s
    /// [`Failure`]): a VMM that stopped answering leaves it unknown.
    fn perform(
        &self,
        name: &MachineName,
        client: &mut VmmClient<'_, C>,
        action: &Action,
        operation: Deadline,
    ) -> Outcome<Event<MachineError>, MachineError> {
        let by = self.step(operation);
        match action {
            Action::SyncGuest => Ok(Event::GuestSynced(self.sync_guest(name, operation))),
            Action::PauseVm => {
                client.pause(by).map_err(MachineError::from)?;
                Ok(Event::Paused)
            }
            Action::ResumeVm => {
                client.resume(by).map_err(MachineError::from)?;
                Ok(Event::Resumed)
            }
            // After saving memory the disk must not change again: power off, never shut down.
            Action::StopVm => match client.stop(StopMode::Force, by) {
                Ok(()) | Err(VmmError::Closed) => Ok(Event::Stopped),
                Err(e) => Err(MachineError::from(e).into()),
            },
            Action::SaveMemory { path } => {
                let bytes = client
                    .save_state(path.clone(), self.capped(operation, SAVE_TIMEOUT))
                    .map_err(MachineError::from)?;
                Ok(Event::MemorySaved { bytes })
            }
            Action::Files(ops) => {
                self.files.apply_all(ops).map_err(MachineError::from)?;
                Ok(Event::FilesDone)
            }
            Action::Discard { dir } => {
                self.files
                    .apply(&FileOp::RemoveTree { path: dir.clone() })
                    .map_err(MachineError::from)?;
                Ok(Event::FilesDone)
            }
        }
    }

    /// Has the guest write its cached data to disk (`/bin/sync` through the
    /// agent). Any failure is a skip with its reason: the checkpoint is still
    /// valid for resuming.
    fn sync_guest(&self, name: &MachineName, by: Deadline) -> GuestSync<MachineError> {
        match self.run_in_guest(name, "/bin/sync", Vec::new(), SYNC_TIMEOUT, by) {
            Ok(output) => match output.termination {
                Termination::Exited(0) => GuestSync::Synced,
                ended @ (Termination::Exited(_)
                | Termination::Signaled(_)
                | Termination::TimedOut) => GuestSync::Skipped(SyncSkipped::Ended(ended)),
            },
            Err(error) => GuestSync::Skipped(SyncSkipped::NotRun(error)),
        }
    }

    fn existing_checkpoint(
        &self,
        bundle: &MachineBundle,
        label: &CheckpointLabel,
    ) -> Result<doppel_machine::bundle::CheckpointFiles, MachineError> {
        let checkpoint = bundle.checkpoint(label);
        if checkpoint.memory.exists() && checkpoint.image.disk.exists() {
            Ok(checkpoint)
        } else {
            Err(MachineError::NoSuchCheckpoint {
                machine: bundle.name().clone(),
                label: label.as_str().to_owned(),
            })
        }
    }

    /// Deletes a stopped machine, its disk and checkpoints, for good. The
    /// claim is used up: the machine is gone. Its recordings stay.
    ///
    /// # Errors
    /// [`StoreError::NoSuchMachine`], [`MachineError::Running`], or a
    /// failure moving or removing its files; a failure after the move
    /// leaves the machine gone from the list, its files in `deleting/`.
    fn delete(&self, claim: Claim<'_, C, F>, by: Deadline) -> Result<(), MachineError> {
        let bundle = &claim.bundle;
        let name = bundle.name();
        if !self.files.exists(bundle.dir())? {
            return Err(StoreError::NoSuchMachine(name.clone()).into());
        }
        if self.connect(name, self.step(by))?.is_some() {
            return Err(MachineError::Running(name.clone()));
        }
        let trash = self.store.root().deleting(name);
        let there = if self.files.exists(&trash)? {
            Trash::Leftover
        } else if trash
            .parent()
            .map_or(Ok(false), |folder| self.files.exists(folder))?
        {
            Trash::Clear
        } else {
            Trash::NoFolder
        };
        self.files.apply_all(&bundle.delete(&trash, there))?;
        Ok(())
    }

    /// Deletes one checkpoint of a machine, unless its lineage keeps it.
    /// Any of its files may be missing: a half-written one goes too.
    ///
    /// # Errors
    /// See [`Claim::delete_checkpoint`].
    fn delete_checkpoint(
        &self,
        claim: &Claim<'_, C, F>,
        label: &CheckpointLabel,
    ) -> Result<(), MachineError> {
        let bundle = &claim.bundle;
        let name = bundle.name();
        if !self.files.exists(bundle.dir())? {
            return Err(StoreError::NoSuchMachine(name.clone()).into());
        }
        if !self.files.exists(&bundle.checkpoint(label).dir)? {
            return Err(MachineError::NoSuchCheckpoint {
                machine: name.clone(),
                label: label.as_str().to_owned(),
            });
        }
        self.store
            .read_lineage(bundle)
            .may_delete(label)
            .map_err(|why| MachineError::CheckpointKept {
                machine: name.clone(),
                label: label.clone(),
                why,
            })?;
        self.files.apply_all(&bundle.delete_checkpoint(label))?;
        Ok(())
    }

    /// Creates `into` from a checkpoint of `source`. The new machine resumes
    /// exactly at the checkpoint when started. It shares the source's machine
    /// identifier and MAC address: VZ refuses to restore with new ones.
    ///
    /// # Errors
    /// See [`MachineError`].
    fn fork(
        &self,
        source: &Claim<'_, C, F>,
        label: &CheckpointLabel,
        into: &Claim<'_, C, F>,
    ) -> Result<(), MachineError> {
        let from = &source.bundle;
        let checkpoint = self.existing_checkpoint(from, label)?;
        let to = &into.bundle;
        self.absent(to)?;
        let login = if self.files.exists(&from.login_password())? {
            doppel_machine::login::Credentials::Stored
        } else {
            doppel_machine::login::Credentials::None
        };
        let fork = from.fork_from(&checkpoint, to, login);
        self.files.apply_all(&fork.build)?;
        self.store.write_lineage(
            &fork.lineage,
            &Lineage::forked(from.name().clone(), label.clone()),
        )?;
        self.files.apply(&fork.commit)?;
        Ok(())
    }

    /// Makes `into` a fresh fork of `source`'s checkpoint: its disk, login
    /// and identity at `mac`, with no memory state, so it cold-boots and can
    /// run beside `source` and other fresh forks.
    ///
    /// # Errors
    /// See [`MachineError`]. Interrupted, it leaves a staging directory the
    /// next claim of `into` removes; `into` appears whole or not at all.
    fn fork_fresh(
        &self,
        source: &Claim<'_, C, F>,
        label: &CheckpointLabel,
        into: &Claim<'_, C, F>,
        mac: MacAddress,
        role: Role,
    ) -> Result<(), MachineError> {
        let from = &source.bundle;
        let checkpoint = self.existing_checkpoint(from, label)?;
        let to = &into.bundle;
        self.absent(to)?;
        let spec = self.store.read_checkpoint_spec(&checkpoint)?;
        let login = if self.files.exists(&from.login_password())? {
            doppel_machine::login::Credentials::Stored
        } else {
            doppel_machine::login::Credentials::None
        };
        let fork = from.fresh_fork_from(&checkpoint, to, login);
        self.files.apply_all(&fork.build)?;
        self.store.write_spec(&fork.spec, &spec.with_mac(mac))?;
        self.store.write_lineage(
            &fork.lineage,
            &Lineage::forked(from.name().clone(), label.clone()).with_role(role),
        )?;
        self.files.apply(&fork.commit)?;
        Ok(())
    }

    /// Makes a machine a golden image whose new machines start from
    /// `template`, one of its checkpoints. Its origin stays.
    ///
    /// # Errors
    /// [`MachineError::NoSuchCheckpoint`], [`MachineError::LineageUnreadable`]
    /// (rewriting it would lose an origin nobody could read), or I/O.
    fn mark_golden(
        &self,
        claim: &Claim<'_, C, F>,
        template: &CheckpointLabel,
    ) -> Result<Lineage, MachineError> {
        self.existing_checkpoint(&claim.bundle, template)?;
        self.change_role(
            claim,
            Role::Golden {
                template: template.clone(),
            },
        )
    }

    /// Makes a golden image a working machine again. Its origin stays.
    ///
    /// # Errors
    /// [`MachineError::LineageUnreadable`], or I/O.
    fn mark_working(&self, claim: &Claim<'_, C, F>) -> Result<Lineage, MachineError> {
        self.change_role(claim, Role::Working)
    }

    fn change_role(&self, claim: &Claim<'_, C, F>, role: Role) -> Result<Lineage, MachineError> {
        let bundle = &claim.bundle;
        self.store.read_spec(bundle)?;
        let lineage = match self.store.read_lineage(bundle) {
            RecordedLineage::Known(lineage) => lineage.with_role(role),
            RecordedLineage::Unreadable(reason) => {
                return Err(MachineError::LineageUnreadable {
                    machine: bundle.name().clone(),
                    reason,
                });
            }
        };
        self.store.write_lineage(&bundle.lineage(), &lineage)?;
        Ok(lineage)
    }

    /// Rolls a stopped machine back to one of its checkpoints, discarding its
    /// current disk.
    ///
    /// # Errors
    /// See [`MachineError`]. [`Unconfirmed::Unknown`] once the restore's
    /// commit marker may exist: the next claim of the machine finishes it.
    fn restore(
        &self,
        claim: &Claim<'_, C, F>,
        label: &CheckpointLabel,
        by: Deadline,
    ) -> Outcome<(), MachineError> {
        let name = claim.name();
        if self.connect(name, self.step(by))?.is_some() {
            return Err(MachineError::Running(name.clone()).into());
        }
        let bundle = &claim.bundle;
        let checkpoint = self.existing_checkpoint(bundle, label)?;
        let kept = if self
            .files
            .exists(bundle.resume_attempt().kept())
            .map_err(MachineError::from)?
        {
            KeptAux::Present
        } else {
            KeptAux::Absent
        };
        let restore = bundle.restore_from(&checkpoint, kept);
        self.files
            .apply_all(&restore.prepare)
            .map_err(|e| Unconfirmed::Failed(e.into()))?;
        // From the marker on, settling finishes the restore, not undoes it.
        let decided = |e: FileOpError| Unconfirmed::Unknown(e.into());
        self.files.apply(&restore.commit).map_err(decided)?;
        self.files.apply_all(&restore.finish).map_err(decided)?;
        Ok(())
    }
}
