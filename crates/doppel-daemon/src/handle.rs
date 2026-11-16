//! How the daemon serves each kind of request, in one place per kind:
//! who may ask, whether it waits its turn on the machine it names, what it
//! may do to the guest, and what runs it. The protocol's operation table
//! (`doppel_protocol::rpc`) ties each to its answer; [`Traits`] reads them
//! off a request whatever its kind.

use std::os::unix::ffi::OsStrExt;

use doppel_host::capabilities::WallClock;
use doppel_host::files::Filesystem;
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_protocol::pb::daemon::v1::daemon_request::Operation;
use doppel_protocol::rpc::Rpc;
use doppel_protocol::with_operation;

use crate::display::Opens;
use crate::requests::Refusal;
use crate::scheduler::Call;
use crate::{Daemon, Link};

/// Who may ask for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Askers {
    Anyone,
    /// Only this Mac's own socket: what changes who gets in, and the
    /// display in this Mac's shared memory.
    ThisMac,
}

/// Whether a request waits for its turn on the machine it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Turn {
    /// It changes or reads the machine in line with the others.
    Waits,
    /// It only looks, skips the line on purpose (a person's input,
    /// control, displays), or names no machine.
    Skips,
}

/// What a request may do to its machine's guest process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Guest {
    Kept,
    /// It may start a new VMM, which a recording follows from its start.
    MayRestart,
}

/// How the daemon serves one kind of request.
pub(crate) trait Handle: Rpc {
    const ASKERS: Askers;
    const TURN: Turn;
    const GUEST: Guest;

    /// The machine it names, as sent.
    fn machine(&self) -> Option<&str>;

    /// What it looks for in the screen's text, if it reads it.
    fn looked_for(&self) -> Option<&str> {
        None
    }

    /// The stream it opens on its connection, if it's served as one.
    fn opens(&self) -> Option<Opens<'_>> {
        None
    }

    /// Runs it: its answer, or why it didn't succeed.
    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        link: &Link,
    ) -> Result<Self::Answer, Refusal>;
}

/// What a request is, whatever its kind.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Traits<'r> {
    pub(crate) askers: Askers,
    pub(crate) turn: Turn,
    pub(crate) guest: Guest,
    pub(crate) machine: Option<&'r str>,
    pub(crate) looked_for: Option<&'r str>,
    pub(crate) opens: Option<Opens<'r>>,
}

impl<'r> Traits<'r> {
    pub(crate) fn of(operation: &'r Operation) -> Self {
        with_operation!(operation, request => Self::read(request))
    }

    fn read<R: Handle>(request: &'r R) -> Self {
        Self {
            askers: R::ASKERS,
            turn: R::TURN,
            guest: R::GUEST,
            machine: request.machine(),
            looked_for: request.looked_for(),
            opens: request.opens(),
        }
    }
}

impl Handle for daemon::ListMachines {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon
            .machines
            .list(daemon.machines.deadline())
            .map(Daemon::<C, F>::machine_list)
            .map_err(Refusal::from)
    }
}

impl Handle for daemon::GetStatus {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.status_request(&self)
    }
}

impl Handle for daemon::StartMachine {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::MayRestart;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.start_request(&self, call)
    }
}

impl Handle for daemon::AwaitDesktop {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.await_desktop_request(&self, call)
    }
}

impl Handle for daemon::StopMachine {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::MayRestart;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.stop_request(&self, call)
    }
}

impl Handle for daemon::PauseMachine {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.pause_request(&self, call)
    }
}

impl Handle for daemon::ResumeMachine {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.resume_request(&self, call)
    }
}

impl Handle for daemon::TakeCheckpoint {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::MayRestart;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.checkpoint_request(&self, call)
    }
}

impl Handle for daemon::ForkMachine {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    // The fork is a new machine; its source keeps running.
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.source)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.fork_request(&self, call)
    }
}

impl Handle for daemon::RestoreMachine {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::MayRestart;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.restore_request(&self, call)
    }
}

impl Handle for daemon::GetDaemonInfo {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        Ok(daemon::DaemonInfo {
            data_root: daemon
                .machines
                .store()
                .root()
                .path()
                .as_os_str()
                .as_bytes()
                .to_vec()
                .into(),
        })
    }
}

impl Handle for daemon::OpenSession {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.open_session(self, link)
    }
}

impl Handle for daemon::ListSessions {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        Ok(daemon.session_list())
    }
}

impl Handle for daemon::GetMachineReport {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.report_request(&self, call)
    }
}

impl Handle for daemon::TakeScreenshot {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.screenshot_request(&self, call)
    }
}

impl Handle for daemon::ReadScreenText {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn looked_for(&self) -> Option<&str> {
        self.query.as_deref()
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.screen_text_request(&self, call)
    }
}

impl Handle for daemon::SendInput {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.input_request(self, call)
    }
}

impl Handle for daemon::TakeSnapshot {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.snapshot_request(&self, call)
    }
}

impl Handle for daemon::ActOnElement {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.act_request(self, call)
    }
}

impl Handle for daemon::WaitForElement {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.wait_request(self, call)
    }
}

impl Handle for daemon::WaitForText {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn looked_for(&self) -> Option<&str> {
        use daemon::wait_for_text::Condition;
        match &self.condition {
            Some(Condition::Appears(text) | Condition::Disappears(text)) => Some(text),
            None => None,
        }
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.text_wait_request(self, call)
    }
}

impl Handle for daemon::RunProgram {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.exec_request(self, call)
    }
}

impl Handle for daemon::ReadGuestFile {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.read_request(self, call)
    }
}

impl Handle for daemon::WriteGuestFile {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.write_request(self, call)
    }
}

impl Handle for daemon::RollBack {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    const GUEST: Guest = Guest::MayRestart;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.rollback_request(&self, call)
    }
}

impl Handle for daemon::OpenDisplay {
    const ASKERS: Askers = Askers::ThisMac;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn opens(&self) -> Option<Opens<'_>> {
        Some(Opens::Shared(self))
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        _daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        Err(Refusal::argument(
            "open_display",
            &"a display is served on a connection, not in a session",
        ))
    }
}

impl Handle for daemon::SendHostInput {
    const ASKERS: Askers = Askers::Anyone;
    // A person's: it doesn't wait behind the agents.
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.host_input_request(self, link)
    }
}

impl Handle for daemon::TakeControl {
    const ASKERS: Askers = Askers::Anyone;
    // A person's: it doesn't wait behind the agents.
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.take_control_request(&self, link)
    }
}

impl Handle for daemon::HandBack {
    const ASKERS: Askers = Askers::Anyone;
    // A person's: it doesn't wait behind the agents.
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.hand_back_request(&self, link)
    }
}

impl Handle for daemon::PauseSession {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon
            .scheduler
            .pause(self.session)
            .map(|()| daemon::Done {})
            .map_err(|error| Refusal::State(error.to_string()))
    }
}

impl Handle for daemon::ResumeSession {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon
            .scheduler
            .resume(self.session)
            .map(|()| daemon::Done {})
            .map_err(|error| Refusal::State(error.to_string()))
    }
}

impl Handle for daemon::OpenEncodedDisplay {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn opens(&self) -> Option<Opens<'_>> {
        Some(Opens::Encoded(self))
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        _daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        Err(Refusal::argument(
            "open_encoded_display",
            &"a display is served on a connection, not in a session",
        ))
    }
}

impl Handle for daemon::RunForks {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    // It starts the forks, not the machine it names.
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.run_forks_request(&self, call)
    }
}

impl Handle for daemon::IssueToken {
    const ASKERS: Askers = Askers::ThisMac;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.issue_token_request(&self)
    }
}

impl Handle for daemon::ListTokens {
    const ASKERS: Askers = Askers::ThisMac;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.list_tokens_request()
    }
}

impl Handle for daemon::RevokeToken {
    const ASKERS: Askers = Askers::ThisMac;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.revoke_token_request(&self)
    }
}

impl Handle for daemon::ConfigureWeb {
    const ASKERS: Askers = Askers::ThisMac;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.configure_web_request(&self)
    }
}

impl Handle for daemon::GetWebStatus {
    const ASKERS: Askers = Askers::ThisMac;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        Ok(Daemon::<C, F>::web_status(&daemon.web.status()))
    }
}

impl Handle for daemon::ListRecordings {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.list_recordings_request()
    }
}

impl Handle for daemon::OpenRecording {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.open_recording_request(&self)
    }
}

impl Handle for daemon::DeleteMachine {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    // It ends the machine: no VMM follows.
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.delete_request(&self, call)
    }
}

impl Handle for daemon::DeleteCheckpoint {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Waits;
    // Only files beside the image: the guest isn't touched.
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.delete_checkpoint_request(&self, call)
    }
}

impl Handle for daemon::WatchActivity {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn opens(&self) -> Option<Opens<'_>> {
        Some(Opens::Activity)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        _daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        Err(Refusal::argument(
            "watch_activity",
            &"activity is served on a connection, not in a session",
        ))
    }
}

impl Handle for daemon::OpenSeenSnapshot {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.seen_snapshot_request(&self)
    }
}

impl Handle for daemon::NameElements {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    // Not a call: it names a snapshot's elements for viewers.
    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        _daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        // Answered for the connection's session, before it runs.
        Err(Refusal::argument(
            "name_elements",
            &"a session names elements: open one on this connection first",
        ))
    }
}

impl Handle for daemon::ObserveElements {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    // A viewer looking: not a call on the machine, so not the session's.
    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.observe_request(&self)
    }
}

impl Handle for daemon::ObserveText {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    // A viewer looking: not a call on the machine, so not the session's.
    fn machine(&self) -> Option<&str> {
        None
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        _link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.observe_text_request(&self)
    }
}

impl Handle for daemon::MarkScreen {
    const ASKERS: Askers = Askers::Anyone;
    const TURN: Turn = Turn::Skips;
    const GUEST: Guest = Guest::Kept;

    fn machine(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn run<C: WallClock + Clone + Send + Sync, F: Filesystem + Sync>(
        self,
        daemon: &Daemon<C, F>,
        _call: &Call,
        link: &Link,
    ) -> Result<Self::Answer, Refusal> {
        daemon.mark_request(&self, link)
    }
}

#[cfg(test)]
mod tests {
    use doppel_protocol::rpc::Rpc;

    use super::*;

    fn traits_of(request: impl Rpc, check: impl FnOnce(Traits<'_>)) {
        check(Traits::of(&request.into_operation()));
    }

    #[test]
    fn who_gets_in_and_the_local_display_are_this_mac_s_only() {
        traits_of(daemon::IssueToken::default(), |t| {
            assert_eq!(t.askers, Askers::ThisMac);
        });
        traits_of(daemon::OpenDisplay::default(), |t| {
            assert_eq!(t.askers, Askers::ThisMac);
            assert!(matches!(t.opens, Some(Opens::Shared(_))));
        });
        traits_of(daemon::OpenEncodedDisplay::default(), |t| {
            assert_eq!(t.askers, Askers::Anyone);
            assert!(matches!(t.opens, Some(Opens::Encoded(_))));
        });
    }

    #[test]
    fn changes_wait_their_turn_on_the_machine_they_name() {
        traits_of(
            daemon::StartMachine {
                name: "dev".into(),
                ..Default::default()
            },
            |t| {
                assert_eq!(
                    (t.turn, t.guest, t.machine),
                    (Turn::Waits, Guest::MayRestart, Some("dev"))
                );
                assert!(t.opens.is_none());
            },
        );
        traits_of(
            daemon::ForkMachine {
                source: "dev".into(),
                ..Default::default()
            },
            |t| assert_eq!((t.turn, t.machine), (Turn::Waits, Some("dev"))),
        );
        traits_of(daemon::TakeControl { name: "dev".into() }, |t| {
            assert_eq!(t.turn, Turn::Skips);
        });
    }

    #[test]
    fn a_text_wait_looks_for_its_text() {
        use daemon::wait_for_text::Condition;
        traits_of(
            daemon::WaitForText {
                name: "dev".into(),
                condition: Some(Condition::Disappears("Saving".into())),
                ..Default::default()
            },
            |t| assert_eq!(t.looked_for, Some("Saving")),
        );
    }
}
