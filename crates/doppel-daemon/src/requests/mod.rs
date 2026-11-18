//! One request against the machines. Every request has an outcome the
//! client sees: a failure is an outcome too, never an error of the daemon.

mod display;
pub(crate) mod forks;
pub(crate) mod handle;
pub(crate) mod operations;

use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;

use doppel_desktop::agent::AgentReport;
use doppel_desktop::ax::refs::SnapshotRefs;
use doppel_desktop::ax::tree::{SnapshotScope, SnapshotSpec};
use doppel_desktop::input::Via;
use doppel_desktop::mark::{Mark, MarkAt};
use doppel_desktop::outcome::{Failure, Unconfirmed};
use doppel_desktop::screen::ScreenshotSize;
use doppel_host::capabilities::WallClock;
use doppel_host::files::Filesystem;
use doppel_host::login::AwaitDesktopError;
use doppel_host::machines::{
    CheckpointReport, Listing, MachineError, Resumed, StopOutcome, StopReport, StopRequest,
};
use doppel_host::vmm::StopMode;
use doppel_host::vmm::launcher::Window;
use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::checkpoint::{Aftermath, Checkpoint, GuestSync};
use doppel_machine::machine::MachineName;
use doppel_protocol::convert::{Micros, ParseError};
use doppel_protocol::pb::agent::v1 as agent;
use doppel_protocol::pb::common::v1 as common;
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_protocol::pb::daemon::v1::daemon_request::Operation;
use doppel_protocol::pb::daemon::v1::daemon_response::Outcome;
use doppel_protocol::pb::vmm::v1 as vmm;
use doppel_protocol::rpc::Rpc;
use doppel_protocol::with_operation;

use doppel_gateway::{ChangeError, Token, TokenDigest, TokenId, TokenLabel, Ttl};

use crate::activity;
use crate::call_events::{Answered, CallEvent, SessionCall};
use crate::call_events::{Guest, Traced};
use crate::recording::library::{Library, LibraryError, RecordingId, TraceLine};
use crate::recording::{self, Recordings};
use crate::requests::forks::PlanError;
use crate::requests::handle::{Handle, Traits, Turn};
use crate::requests::operations::{ForkStart, ForksError, TemporaryMachineError};
use crate::scheduler::Call;
use crate::scheduler::session::SessionState;
use crate::scheduler::{Cancelled, Cleared, HandedBack, HeldUp};
use crate::web::tokens::TokenFileError;
use crate::web::{WebPort, WebSetting, WebStatus};
use crate::{Acted, CallError, Daemon, Link};

/// Why a request didn't succeed.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// The request itself was wrong.
    Argument { field: &'static str, reason: String },
    /// It ran on the host and failed.
    Failed(daemon::MachineFailure),
    /// The request doesn't fit the state things are in.
    State(String),
}

impl Refusal {
    /// Its certainty is its own: failed, or unknown.
    fn unconfirmed<E: std::error::Error + 'static>(
        error: &Unconfirmed<E>,
    ) -> daemon::MachineFailure {
        Self::described(error, error.certainty())
    }

    fn failed(error: &(impl Failure + std::error::Error)) -> Self {
        Self::Failed(Self::failure(error))
    }

    fn failure(error: &(impl Failure + std::error::Error)) -> daemon::MachineFailure {
        Self::described(error, error.certainty())
    }

    /// The error's message and its causes', so the client prints the chain
    /// as this process would.
    fn described(
        error: &dyn std::error::Error,
        certainty: doppel_desktop::outcome::Certainty,
    ) -> daemon::MachineFailure {
        let causes = std::iter::successors(error.source(), |cause| cause.source())
            .map(ToString::to_string)
            .collect();
        daemon::MachineFailure {
            message: error.to_string(),
            certainty: daemon::Certainty::from(certainty) as i32,
            causes,
        }
    }

    pub(crate) fn argument(field: &'static str, reason: &dyn std::fmt::Display) -> Self {
        Self::Argument {
            field,
            reason: reason.to_string(),
        }
    }

    pub(crate) fn name(field: &'static str, raw: &str) -> Result<MachineName, Self> {
        raw.parse().map_err(|error| Self::argument(field, &error))
    }

    fn label(field: &'static str, raw: &str) -> Result<CheckpointLabel, Self> {
        raw.parse().map_err(|error| Self::argument(field, &error))
    }

    pub(crate) fn outcome(self) -> Outcome {
        match self {
            Self::Argument { field, reason } => Outcome::Error(common::Error {
                kind: Some(common::error::Kind::InvalidArgument(
                    common::InvalidArgument {
                        field: field.into(),
                        reason,
                    },
                )),
            }),
            Self::Failed(failure) => Outcome::MachineFailure(failure),
            Self::State(reason) => Outcome::Error(common::Error {
                kind: Some(common::error::Kind::InvalidState(common::InvalidState {
                    reason,
                })),
            }),
        }
    }
}

impl From<CallError> for Refusal {
    fn from(error: CallError) -> Self {
        Self::failed(&error)
    }
}

/// Its certainty is its own: failed, or unknown.
impl From<Unconfirmed<CallError>> for Refusal {
    fn from(error: Unconfirmed<CallError>) -> Self {
        Self::Failed(Self::unconfirmed(&error))
    }
}

impl From<MachineError> for Refusal {
    fn from(error: MachineError) -> Self {
        Self::failed(&error)
    }
}

impl From<AwaitDesktopError> for Refusal {
    fn from(error: AwaitDesktopError) -> Self {
        Self::failed(&error)
    }
}

/// Its certainty is its own: failed, or unknown.
impl From<Unconfirmed<MachineError>> for Refusal {
    fn from(error: Unconfirmed<MachineError>) -> Self {
        Self::Failed(Self::unconfirmed(&error))
    }
}

impl From<ParseError> for Refusal {
    fn from(error: ParseError) -> Self {
        Self::Argument {
            field: "request",
            reason: error.to_string(),
        }
    }
}

impl<C, F> Daemon<C, F>
where
    C: WallClock + Clone + Send + Sync,
    F: Filesystem + Sync,
{
    /// Answers one request of a connection. A session's call is noted in
    /// the session, and its moments are published as [`CallEvent`]s.
    pub(crate) fn answer(
        &self,
        request: daemon::DaemonRequest,
        call: &Call,
        link: &Link,
    ) -> (Outcome, Option<HandedBack>) {
        let session = *link.session();
        let Some(session) = session else {
            return (self.respond(request.operation, call, link), None);
        };
        // Not a call: it takes no turn, and is neither traced nor shown.
        if let Some(Operation::NameElements(named)) = &request.operation {
            let outcome = self
                .name_elements(session, request.id, named)
                .map_or_else(Refusal::outcome, daemon::NameElements::answer);
            return (outcome, None);
        }
        let traits = request.operation.as_ref().map(Traits::of);
        let machine = traits
            .and_then(|traits| traits.machine)
            .and_then(|raw| raw.parse::<MachineName>().ok());
        call.join(session, machine.clone(), request.caption.clone());
        let session_call = Arc::new(SessionCall {
            session,
            request: request.id,
            client: self.scheduler.client(session).unwrap_or_default(),
            caption: request.caption.clone(),
            acted: machine
                .as_ref()
                .zip(request.operation.as_ref())
                .map(|(machine, operation)| daemon::CallStarted {
                    why: request.why.trim().to_owned(),
                    ..activity::started(operation, |snapshot, index| {
                        self.elements.find(machine, snapshot, index)
                    })
                }),
            machine,
            looked_for: traits
                .and_then(|traits| traits.looked_for)
                .map(str::to_owned),
            guest: traits.map_or(Guest::Kept, |traits| traits.guest),
        });
        // A paused session, or a machine a person has, holds the call here.
        let arrived = self.machines.clock().now();
        let (note, waited) = match call.act() {
            Ok(Cleared { note, held_up }) => (
                note,
                match held_up {
                    HeldUp::No => None,
                    HeldUp::Yes => Some(
                        self.machines
                            .clock()
                            .now()
                            .saturating_duration_since(arrived),
                    ),
                },
            ),
            Err(Cancelled) => {
                return (Refusal::from(CallError::Cancelled).outcome(), None);
            }
        };
        call.is_session_call(Arc::clone(&session_call));
        self.publish(&CallEvent::Running {
            call: &session_call,
        });
        let ran_from = self.machines.clock().now();
        let outcome = self.respond(request.operation, call, link);
        self.publish(&CallEvent::Ended {
            call: &session_call,
            ran_from,
            began: call.began(),
            ended: self.machines.clock().now(),
            waited,
            answered: Answered {
                traced: Self::traced(&outcome),
                seen: Self::seen(&outcome),
                read: Self::read(&outcome)
                    .map(|text| activity::read(text, session_call.looked_for.as_deref())),
            },
        });
        (outcome, note)
    }

    /// The snapshot an outcome carries: a snapshot's, or the last one an
    /// element wait took.
    const fn seen(outcome: &Outcome) -> Option<&agent::AxSnapshot> {
        match outcome {
            Outcome::AxSnapshot(snapshot)
            | Outcome::ElementWait(daemon::ElementWaitResult {
                seen: Some(snapshot),
                ..
            }) => Some(snapshot),
            Outcome::ElementWait(daemon::ElementWaitResult { seen: None, .. })
            | Outcome::MachineFailure(_)
            | Outcome::Error(_)
            | Outcome::TextWait(_)
            | Outcome::Done(_)
            | Outcome::MachineList(_)
            | Outcome::MachineStatus(_)
            | Outcome::MachineStarted(_)
            | Outcome::LoginResult(_)
            | Outcome::MachineStopped(_)
            | Outcome::MachineResumed(_)
            | Outcome::CheckpointReport(_)
            | Outcome::DaemonInfo(_)
            | Outcome::SessionOpened(_)
            | Outcome::SessionList(_)
            | Outcome::MachineReport(_)
            | Outcome::Image(_)
            | Outcome::ScreenText(_)
            | Outcome::ActResult(_)
            | Outcome::ExecResult(_)
            | Outcome::FileContents(_)
            | Outcome::DisplayLayout(_)
            | Outcome::EncodedDisplay(_)
            | Outcome::ForksRun(_)
            | Outcome::TemporaryMachineMade(_)
            | Outcome::Token(_)
            | Outcome::TokenList(_)
            | Outcome::RecordingList(_)
            | Outcome::Recording(_)
            | Outcome::WebStatus(_) => None,
        }
    }

    /// The text an outcome read off the screen: a text read's, or the last
    /// poll of a text wait.
    const fn read(outcome: &Outcome) -> Option<&vmm::RecognizedText> {
        match outcome {
            Outcome::ScreenText(text)
            | Outcome::TextWait(daemon::TextWaitResult {
                seen: Some(text), ..
            }) => Some(text),
            Outcome::TextWait(daemon::TextWaitResult { seen: None, .. })
            | Outcome::MachineFailure(_)
            | Outcome::Error(_)
            | Outcome::ElementWait(_)
            | Outcome::AxSnapshot(_)
            | Outcome::Done(_)
            | Outcome::MachineList(_)
            | Outcome::MachineStatus(_)
            | Outcome::MachineStarted(_)
            | Outcome::LoginResult(_)
            | Outcome::MachineStopped(_)
            | Outcome::MachineResumed(_)
            | Outcome::CheckpointReport(_)
            | Outcome::DaemonInfo(_)
            | Outcome::SessionOpened(_)
            | Outcome::SessionList(_)
            | Outcome::MachineReport(_)
            | Outcome::Image(_)
            | Outcome::ActResult(_)
            | Outcome::ExecResult(_)
            | Outcome::FileContents(_)
            | Outcome::DisplayLayout(_)
            | Outcome::EncodedDisplay(_)
            | Outcome::ForksRun(_)
            | Outcome::TemporaryMachineMade(_)
            | Outcome::Token(_)
            | Outcome::TokenList(_)
            | Outcome::RecordingList(_)
            | Outcome::Recording(_)
            | Outcome::WebStatus(_) => None,
        }
    }

    /// How a request ended, for the trace: a wait answered with its
    /// timeout or failure didn't succeed, though the request was answered.
    fn traced(outcome: &Outcome) -> Traced {
        use daemon::element_wait_result::End as Element;
        use daemon::text_wait_result::End as Text;
        let timed_out = |end: &daemon::WaitTimedOut| Traced::TimedOut(end.last_error.clone());
        let failed = |end: &daemon::WaitFailed| Traced::Failed(end.reason.clone());
        match outcome {
            Outcome::MachineFailure(failure) => Traced::Failed(failure.message.clone()),
            Outcome::Error(error) => Traced::Refused(
                doppel_protocol::convert::RemoteError::from(error.clone()).to_string(),
            ),
            Outcome::ElementWait(wait) => match &wait.end {
                Some(Element::TimedOut(end)) => timed_out(end),
                Some(Element::Failed(end)) => failed(end),
                Some(Element::Satisfied(_)) | None => Traced::Done,
            },
            Outcome::TextWait(wait) => match &wait.end {
                Some(Text::TimedOut(end)) => timed_out(end),
                Some(Text::Failed(end)) => failed(end),
                Some(Text::Satisfied(_)) | None => Traced::Done,
            },
            // A run's forks each say how they started; the run itself
            // was done.
            Outcome::Done(_)
            | Outcome::MachineList(_)
            | Outcome::MachineStatus(_)
            | Outcome::MachineStarted(_)
            | Outcome::LoginResult(_)
            | Outcome::MachineStopped(_)
            | Outcome::MachineResumed(_)
            | Outcome::CheckpointReport(_)
            | Outcome::DaemonInfo(_)
            | Outcome::SessionOpened(_)
            | Outcome::SessionList(_)
            | Outcome::MachineReport(_)
            | Outcome::Image(_)
            | Outcome::ScreenText(_)
            | Outcome::AxSnapshot(_)
            | Outcome::ActResult(_)
            | Outcome::ExecResult(_)
            | Outcome::FileContents(_)
            | Outcome::DisplayLayout(_)
            | Outcome::EncodedDisplay(_)
            | Outcome::ForksRun(_)
            | Outcome::TemporaryMachineMade(_)
            | Outcome::Token(_)
            | Outcome::TokenList(_)
            | Outcome::RecordingList(_)
            | Outcome::Recording(_)
            | Outcome::WebStatus(_) => Traced::Done,
        }
    }

    /// Runs one request. Changes wait for the machine's turn, then take its
    /// claim for their whole run, so a second process gets "busy".
    pub(crate) fn respond(
        &self,
        operation: Option<Operation>,
        call: &Call,
        link: &Link,
    ) -> Outcome {
        // Prost leaves it unset for an operation this build doesn't know.
        let Some(operation) = operation else {
            return Outcome::Error(common::Error {
                kind: Some(common::error::Kind::Unsupported(common::Unsupported {
                    what: "an operation this daemon doesn't know; is it older than the client?"
                        .into(),
                })),
            });
        };
        with_operation!(operation, request => self.run(request, call, link))
    }

    /// Runs one kind of request; its answer is the outcome that kind gets.
    fn run<R: Handle>(&self, request: R, call: &Call, link: &Link) -> Outcome {
        request
            .run(self, call, link)
            .map_or_else(Refusal::outcome, R::answer)
    }

    pub(super) fn status_request(
        &self,
        request: &daemon::GetStatus,
    ) -> Result<daemon::MachineStatus, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let status = self
            .machines
            .machine(&name)
            .status(self.machines.deadline())?;
        Ok(status.into())
    }

    pub(super) fn start_request(
        &self,
        request: &daemon::StartMachine,
        call: &Call,
    ) -> Result<daemon::MachineStarted, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let window = match daemon::Window::try_from(request.window) {
            Ok(daemon::Window::Hidden) => Window::Hidden,
            Ok(daemon::Window::Shown) => Window::Shown,
            Ok(daemon::Window::Unspecified) | Err(_) => {
                return Err(Refusal::argument("window", &request.window));
            }
        };
        let (state, next) = self.start(&name, window, call)?;
        Ok(daemon::MachineStarted {
            state: Some(state.into()),
            started_by: daemon::NextStart::from(next) as i32,
        })
    }

    pub(super) fn await_desktop_request(
        &self,
        request: &daemon::AwaitDesktop,
        call: &Call,
    ) -> Result<daemon::LoginResult, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let outcome = self.await_desktop(&name, call)?;
        Ok(daemon::LoginResult::in_micros(&outcome))
    }

    pub(super) fn stop_request(
        &self,
        request: &daemon::StopMachine,
        call: &Call,
    ) -> Result<daemon::MachineStopped, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let mode = match vmm::StopMode::try_from(request.mode) {
            Ok(vmm::StopMode::Graceful) => StopMode::Graceful,
            Ok(vmm::StopMode::Force) => StopMode::Force,
            Ok(vmm::StopMode::Unspecified) | Err(_) => {
                return Err(Refusal::argument("mode", &request.mode));
            }
        };
        let report = self.stop(&name, mode, call)?;
        Ok(Self::stopped(report))
    }

    pub(super) fn pause_request(
        &self,
        request: &daemon::PauseMachine,
        call: &Call,
    ) -> Result<daemon::Done, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        self.pause(&name, call)?;
        Ok(daemon::Done {})
    }

    pub(super) fn resume_request(
        &self,
        request: &daemon::ResumeMachine,
        call: &Call,
    ) -> Result<daemon::MachineResumed, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let outcome = match self.resume(&name, call)? {
            Resumed::Resumed => daemon::ResumeOutcome::Resumed,
            Resumed::AlreadyRunning => daemon::ResumeOutcome::AlreadyRunning,
        };
        Ok(daemon::MachineResumed {
            outcome: outcome as i32,
        })
    }

    pub(super) fn checkpoint_request(
        &self,
        request: &daemon::TakeCheckpoint,
        call: &Call,
    ) -> Result<daemon::CheckpointReport, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let label = Refusal::label("label", &request.label)?;
        let then = ParseError::after_checkpoint("then", request.then)?;
        let report = self.checkpoint(&name, &label, then, call)?;
        Ok(Self::checkpoint_report(report))
    }

    pub(super) fn delete_request(
        &self,
        request: &daemon::DeleteMachine,
        call: &Call,
    ) -> Result<daemon::Done, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        self.delete(&name, call)?;
        Ok(daemon::Done {})
    }

    pub(super) fn delete_checkpoint_request(
        &self,
        request: &daemon::DeleteCheckpoint,
        call: &Call,
    ) -> Result<daemon::Done, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let label = Refusal::label("label", &request.label)?;
        self.delete_checkpoint(&name, &label, call)?;
        Ok(daemon::Done {})
    }

    pub(super) fn fork_request(
        &self,
        request: &daemon::ForkMachine,
        call: &Call,
    ) -> Result<daemon::Done, Refusal> {
        let source = Refusal::name("source", &request.source)?;
        let label = Refusal::label("label", &request.label)?;
        let into = Refusal::name("into", &request.into)?;
        self.fork(&source, &label, &into, call)?;
        Ok(daemon::Done {})
    }

    /// Records a token the client made, by its digest.
    pub(super) fn issue_token_request(
        &self,
        request: &daemon::IssueToken,
    ) -> Result<daemon::TokenEntry, Refusal> {
        let digest = TokenDigest::from_bytes(&request.sha256)
            .map_err(|error| Refusal::argument("sha256", &error))?;
        let ttl = Ttl::from_secs(u64::from(request.ttl_seconds))
            .map_err(|error| Refusal::argument("ttl_seconds", &error))?;
        let label = match request.label.as_str() {
            "" => None,
            raw => Some(
                raw.parse::<TokenLabel>()
                    .map_err(|error| Refusal::argument("label", &error))?,
            ),
        };
        let now = self.machines.clock().wall();
        let token = self
            .tokens
            .change(|book| book.add(digest, label, ttl, now))
            .map_err(Self::token_refusal)?;
        Ok(Self::token_entry(&token))
    }

    pub(super) fn list_tokens_request(&self) -> Result<daemon::TokenList, Refusal> {
        let book = self.tokens.read().map_err(Self::token_refusal)?;
        let now = self.machines.clock().wall();
        Ok(daemon::TokenList {
            tokens: book.live(now).map(Self::token_entry).collect(),
        })
    }

    pub(super) fn revoke_token_request(
        &self,
        request: &daemon::RevokeToken,
    ) -> Result<daemon::TokenEntry, Refusal> {
        let id: TokenId = request
            .id
            .parse()
            .map_err(|error| Refusal::argument("id", &error))?;
        let token = self
            .tokens
            .change(|book| book.revoke(&id))
            .map_err(Self::token_refusal)?;
        // Its browsers are closed now, not at their next check.
        self.web.revoke(&id);
        Ok(Self::token_entry(&token))
    }

    pub(super) fn configure_web_request(
        &self,
        request: &daemon::ConfigureWeb,
    ) -> Result<daemon::WebStatus, Refusal> {
        let setting = match &request.listen {
            Some(daemon::configure_web::Listen::On(on)) => WebSetting::On(
                WebPort::try_from(on.port).map_err(|error| Refusal::argument("port", &error))?,
            ),
            Some(daemon::configure_web::Listen::Off(_)) => WebSetting::Off,
            None => return Err(Refusal::argument("listen", &"missing")),
        };
        let status = self.web.set(setting).map_err(|error| {
            Refusal::Failed(Refusal::described(
                &error,
                doppel_desktop::outcome::Certainty::MaybeDone,
            ))
        })?;
        Ok(Self::web_status(&status))
    }

    pub(super) fn web_status(status: &WebStatus) -> daemon::WebStatus {
        use daemon::web_status::State;
        let state = match status {
            WebStatus::Off => State::Off(daemon::Done {}),
            WebStatus::Listening {
                port,
                fingerprint,
                names,
            } => State::Listening(daemon::WebListening {
                port: u32::from(port.get()),
                fingerprint: fingerprint.to_string(),
                names: names.clone(),
            }),
            WebStatus::Failed { port, reason } => State::Failed(daemon::WebFailed {
                port: u32::from(port.get()),
                reason: reason.clone(),
            }),
        };
        daemon::WebStatus { state: Some(state) }
    }

    fn token_refusal(error: TokenFileError) -> Refusal {
        match error {
            TokenFileError::Change(ChangeError::Unknown(_)) => Refusal::argument("id", &error),
            TokenFileError::Change(ChangeError::Duplicate(_)) => Refusal::State(error.to_string()),
            TokenFileError::Read { .. }
            | TokenFileError::Book { .. }
            | TokenFileError::Write { .. } => Refusal::failed(&error),
        }
    }

    fn token_entry(token: &Token) -> daemon::TokenEntry {
        let unix = |at: std::time::SystemTime| {
            at.duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs())
        };
        daemon::TokenEntry {
            id: token.id().to_string(),
            label: token
                .label()
                .map_or_else(String::new, |label| label.to_string()),
            issued_unix_seconds: unix(token.issued()),
            expires_unix_seconds: unix(token.expires()),
        }
    }

    pub(super) fn run_forks_request(
        &self,
        request: &daemon::RunForks,
        call: &Call,
    ) -> Result<daemon::ForksRun, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let label = Refusal::label("label", &request.label)?;
        let forks =
            self.run_forks(&name, &label, request.count, call)
                .map_err(|error| match error {
                    ForksError::Plan(PlanError::NoForks) => Refusal::argument("count", &error),
                    ForksError::Plan(PlanError::Name(_)) => Refusal::argument("name", &error),
                    ForksError::Plan(PlanError::TooMany { .. }) => {
                        Refusal::State(error.to_string())
                    }
                    ForksError::Mac(_)
                    | ForksError::Turn(_)
                    | ForksError::Listing(_)
                    | ForksError::Making { .. } => Refusal::failed(&error),
                })?;
        Ok(daemon::ForksRun {
            forks: forks
                .into_iter()
                .map(|(name, start)| {
                    use daemon::run_fork::Outcome as Wire;
                    let outcome = match start {
                        ForkStart::Desktop(outcome) => {
                            Wire::Desktop(daemon::LoginResult::in_micros(&outcome))
                        }
                        ForkStart::Start(error) => Wire::Failure(Refusal::unconfirmed(&error)),
                        ForkStart::AwaitDesktop(error) => Wire::Failure(Refusal::failure(&error)),
                        ForkStart::Interrupted => Wire::Failure(daemon::MachineFailure {
                            message: format!("starting {name} was interrupted; check its status"),
                            certainty: daemon::Certainty::MaybeDone as i32,
                            causes: Vec::new(),
                        }),
                    };
                    daemon::RunFork {
                        name: name.to_string(),
                        outcome: Some(outcome),
                    }
                })
                .collect(),
        })
    }

    /// Makes a temporary machine and holds it on `link`, which removes it
    /// when the connection closes.
    pub(super) fn make_temporary_machine_request(
        &self,
        request: &daemon::MakeTemporaryMachine,
        call: &Call,
        link: &Link,
    ) -> Result<daemon::TemporaryMachineMade, Refusal> {
        let golden = Refusal::name("golden", &request.golden)?;
        let name =
            self.make_temporary_machine(&golden, call)
                .map_err(|error| match error {
                    TemporaryMachineError::NoSuchMachine(_)
                    | TemporaryMachineError::NotGolden(_) => Refusal::argument("golden", &error),
                    TemporaryMachineError::Plan(PlanError::NoForks | PlanError::Name(_)) => {
                        Refusal::argument("golden", &error)
                    }
                    TemporaryMachineError::Plan(PlanError::TooMany { .. })
                    | TemporaryMachineError::LineageUnreadable { .. } => {
                        Refusal::State(error.to_string())
                    }
                    TemporaryMachineError::Mac(_)
                    | TemporaryMachineError::Turn(_)
                    | TemporaryMachineError::Listing(_)
                    | TemporaryMachineError::Making { .. } => Refusal::failed(&error),
                })?;
        link.temporary_machines().push(name.clone());
        eprintln!("doppeld: {name} made from {golden}, temporary");
        Ok(daemon::TemporaryMachineMade {
            name: name.to_string(),
        })
    }

    pub(super) fn restore_request(
        &self,
        request: &daemon::RestoreMachine,
        call: &Call,
    ) -> Result<daemon::Done, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let label = Refusal::label("label", &request.label)?;
        self.restore(&name, &label, call)?;
        Ok(daemon::Done {})
    }

    pub(super) fn machine_list(listings: Vec<Listing>) -> daemon::MachineList {
        let mut list = daemon::MachineList::default();
        for listing in listings {
            match listing {
                Listing::Machine {
                    name,
                    status,
                    lineage,
                } => list.machines.push(daemon::MachineEntry {
                    name: name.to_string(),
                    status: Some(match status {
                        Ok(status) => daemon::machine_entry::Status::MachineStatus(status.into()),
                        Err(error) => {
                            daemon::machine_entry::Status::Failure(Refusal::failure(&error))
                        }
                    }),
                    recorded_lineage: Some((&lineage).into()),
                }),
                Listing::Unrecognized(raw) => {
                    list.unrecognized.push(raw.as_bytes().to_vec().into());
                }
            }
        }
        list
    }

    fn stopped(report: StopReport) -> daemon::MachineStopped {
        use daemon::machine_stopped::Request;
        let request = match report.request {
            StopRequest::Forced => Request::Forced(daemon::Done {}),
            StopRequest::Session => Request::Session(daemon::Done {}),
            StopRequest::SessionAfterUnlock { unlocked_after } => {
                Request::SessionAfterUnlockMicros(Micros::saturating(unlocked_after).get())
            }
            StopRequest::PowerButton { session } => Request::PowerButton(session.to_string()),
        };
        let outcome = match report.outcome {
            StopOutcome::Stopped => daemon::StopOutcome::Stopped,
            StopOutcome::ShutdownPending => daemon::StopOutcome::ShutdownPending,
        };
        daemon::MachineStopped {
            request: Some(request),
            outcome: outcome as i32,
        }
    }

    fn checkpoint_report(report: CheckpointReport) -> daemon::CheckpointReport {
        use daemon::checkpoint_report::Result as Wire;
        let screen = report.screen.as_ref().map(daemon::HostScreen::in_micros);
        let result = match report.result {
            Checkpoint::Completed {
                memory_bytes,
                guest_sync,
                aftermath,
                ..
            } => Wire::Completed(daemon::CheckpointCompleted {
                memory_bytes,
                sync_skipped: match guest_sync {
                    GuestSync::Synced => None,
                    GuestSync::Skipped(why) => Some(why.to_string()),
                },
                left_paused: match aftermath {
                    Aftermath::AsRequested(_) => None,
                    Aftermath::LeftPaused(why) => Some(Refusal::unconfirmed(&why)),
                },
                screen,
            }),
            Checkpoint::Failed {
                at, error, guest, ..
            } => Wire::Failed(daemon::CheckpointFailed {
                at: daemon::CheckpointStep::from(at) as i32,
                error: Some(Refusal::failure(&error)),
                guest: daemon::GuestAfterFailure::from(guest) as i32,
            }),
            Checkpoint::Unknown { at, error, .. } => Wire::Unknown(daemon::CheckpointUnknown {
                at: daemon::CheckpointStep::from(at) as i32,
                error: Some(Refusal::failure(&error)),
            }),
            Checkpoint::InProgress { awaiting, .. } => {
                Wire::InProgress(daemon::CheckpointStep::from(awaiting.step()) as i32)
            }
        };
        daemon::CheckpointReport {
            result: Some(result),
            paused_for_micros: report
                .paused_for
                .map(|paused| Micros::saturating(paused).get()),
            cleanup: report
                .cleanup
                .iter()
                .map(|(action, result)| daemon::CheckpointCleanup {
                    action: format!("{action:?}"),
                    failure: result.as_ref().err().map(Refusal::unconfirmed),
                })
                .collect(),
        }
    }
}

/// The operations of a session's tools.
impl<C, F> Daemon<C, F>
where
    C: WallClock + Clone + Send + Sync,
    F: Filesystem + Sync,
{
    /// Where a request read by a connection stands among the connection's
    /// requests: those that take a turn on the machine they name enter its
    /// line in the order they arrived (see [`crate::scheduler`]): the machine
    /// whose line it takes its place in.
    pub(crate) fn reserves(operation: Option<&Operation>) -> Option<MachineName> {
        let traits = operation.map(Traits::of)?;
        let machine = match traits.turn {
            Turn::Waits => traits.machine?.parse::<MachineName>().ok()?,
            Turn::Skips => return None,
        };
        Some(machine)
    }

    pub(super) fn open_session(
        &self,
        request: daemon::OpenSession,
        link: &Link,
    ) -> Result<daemon::SessionOpened, Refusal> {
        let mut session = link.session();
        if session.is_some() {
            return Err(Refusal::argument(
                "open_session",
                &"this connection already has a session",
            ));
        }
        let id = self.scheduler.open(request.client.clone());
        let recording = if request.record {
            let clock = self.machines.clock();
            let dir =
                Recordings::folder(self.machines.store().root().path(), clock.wall(), id.get());
            match Recordings::open(&dir, clock.now()) {
                Ok(recordings) => {
                    let dir = recordings.dir().to_path_buf();
                    self.scheduler.record(id, recordings);
                    Some(dir)
                }
                Err(error) => {
                    self.scheduler.discard(id);
                    return Err(Refusal::Failed(Refusal::described(
                        &error,
                        doppel_desktop::outcome::Certainty::NotDone,
                    )));
                }
            }
        } else {
            None
        };
        match &recording {
            Some(dir) => eprintln!(
                "doppeld: {id} opened by {}, recorded into {}",
                request.client,
                dir.display()
            ),
            None => eprintln!("doppeld: {id} opened by {}", request.client),
        }
        *session = Some(id);
        Ok(daemon::SessionOpened {
            session: id.get(),
            recording: recording.map(|dir| dir.as_os_str().as_bytes().to_vec().into()),
        })
    }

    fn library(&self) -> Library {
        Library::in_root(self.machines.store().root().path())
    }

    fn library_refusal(error: &LibraryError) -> Refusal {
        match error {
            LibraryError::Missing(_) => Refusal::argument("id", error),
            LibraryError::NoSnapshot { .. } => Refusal::argument("line", error),
            LibraryError::Io { .. } => Refusal::State(error.to_string()),
        }
    }

    pub(super) fn list_recordings_request(&self) -> Result<daemon::RecordingList, Refusal> {
        let recordings = self
            .library()
            .list()
            .map_err(|error| Self::library_refusal(&error))?;
        Ok(daemon::RecordingList {
            recordings: recordings
                .iter()
                .map(|recording| daemon::RecordingSummary {
                    id: recording.id.to_string(),
                    started_unix_seconds: i64::try_from(recording.id.started_unix_seconds())
                        .unwrap_or(i64::MAX),
                    session: recording.id.session(),
                    machines: recording
                        .machines()
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    videos: u32::try_from(recording.videos.len()).unwrap_or(u32::MAX),
                    calls: u32::try_from(recording.trace.len()).unwrap_or(u32::MAX),
                    traced_ms: recording.traced_ms(),
                })
                .collect(),
        })
    }

    pub(super) fn open_recording_request(
        &self,
        request: &daemon::OpenRecording,
    ) -> Result<daemon::Recording, Refusal> {
        let id: RecordingId = request
            .id
            .parse()
            .map_err(|error| Refusal::argument("id", &error))?;
        let recording = self
            .library()
            .open(id)
            .map_err(|error| Self::library_refusal(&error))?;
        Ok(daemon::Recording {
            id: recording.id.to_string(),
            videos: recording
                .videos
                .iter()
                .map(|video| daemon::RecordingVideo {
                    file: video.file.to_string(),
                    machine: video.file.machine().to_string(),
                    start_ms: video.start_ms,
                    display_points: video.points.map(|points| common::Size {
                        width: points.width,
                        height: points.height,
                    }),
                })
                .collect(),
            trace: recording.trace.into_iter().map(Self::trace_line).collect(),
        })
    }

    /// A person's own read of the screen's text.
    pub(super) fn observe_text_request(
        &self,
        request: &daemon::ObserveText,
    ) -> Result<vmm::RecognizedText, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        Ok((&self.observe_text(&name)?).into())
    }

    /// A mark for the people watching: checked here, drawn by consoles from
    /// the call's activity. Only a session's calls are shown, so outside one
    /// it would go nowhere.
    pub(super) fn mark_request(
        &self,
        request: &daemon::MarkScreen,
        link: &Link,
    ) -> Result<daemon::Done, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        if link.session().is_none() {
            return Err(Refusal::State(
                "a mark is shown to the people watching a session; open one first".into(),
            ));
        }
        let mark = Mark::try_from(request)?;
        match mark.at() {
            MarkAt::Element(element)
                if self
                    .elements
                    .find(&name, element.snapshot.get(), element.node.get())
                    .is_none() =>
            {
                Err(Refusal::State(
                    "that element's snapshot is no longer kept, or the element has no frame; \
                     take a new snapshot and mark one of its elements"
                        .into(),
                ))
            }
            MarkAt::Element(_) | MarkAt::Point(_) | MarkAt::Area(_) => Ok(daemon::Done {}),
        }
    }

    /// A person's look: redacted like every snapshot that leaves for a console.
    pub(super) fn observe_request(
        &self,
        request: &daemon::ObserveElements,
    ) -> Result<agent::AxSnapshot, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let snapshot = self.observe(&name)?;
        Ok(activity::shown(&(&snapshot).into()))
    }

    /// Keeps what the session named the elements of a snapshot its calls
    /// saw, in its recording, and tells consoles.
    pub(super) fn name_elements(
        &self,
        session: crate::scheduler::SessionId,
        request: u64,
        named: &daemon::NameElements,
    ) -> Result<daemon::Done, Refusal> {
        let machine = Refusal::name("machine", &named.machine)?;
        let refs = SnapshotRefs::try_from(
            named
                .refs
                .clone()
                .ok_or_else(|| Refusal::argument("refs", &"missing"))?,
        )?;
        let refs = daemon::ElementRefs::from(&refs);
        self.publish(&CallEvent::Named {
            session,
            request,
            client: self.scheduler.client(session).unwrap_or_default(),
            machine,
            refs: &refs,
            at: self.machines.clock().now(),
        });
        Ok(daemon::Done {})
    }

    pub(super) fn seen_snapshot_request(
        &self,
        request: &daemon::OpenSeenSnapshot,
    ) -> Result<agent::AxSnapshot, Refusal> {
        let id = request
            .recording
            .parse()
            .map_err(|error| Refusal::argument("recording", &error))?;
        self.library()
            .seen(id, request.line)
            .map_err(|error| Self::library_refusal(&error))
    }

    fn trace_line(line: TraceLine) -> daemon::TraceLine {
        use daemon::trace_line::Line;
        daemon::TraceLine {
            line: Some(match line {
                TraceLine::Unreadable(raw) => Line::Unreadable(raw),
                TraceLine::Call(call) => Line::Call(daemon::TraceCall {
                    start_ms: call.start_ms,
                    end_ms: call.end_ms,
                    machine: call.machine,
                    call: call.call,
                    outcome: match call.outcome {
                        recording::Outcome::Done => daemon::TraceOutcome::Done,
                        recording::Outcome::Failed => daemon::TraceOutcome::Failed,
                        recording::Outcome::TimedOut => daemon::TraceOutcome::TimedOut,
                        recording::Outcome::Refused => daemon::TraceOutcome::Refused,
                    } as i32,
                    error: call.error,
                    waited_ms: call.waited_ms,
                    queued_ms: call.queued_ms,
                    by: match call.by {
                        recording::Actor::Session => daemon::TraceActor::Session,
                        recording::Actor::Person => daemon::TraceActor::Person,
                    } as i32,
                    acted: call.acted.map(|acted| *acted),
                    seen: call.seen,
                    refs: call.refs.map(|refs| *refs),
                    read: call.read.map(|read| *read),
                }),
            }),
        }
    }

    pub(super) fn session_list(&self) -> daemon::SessionList {
        daemon::SessionList {
            sessions: self
                .scheduler
                .list()
                .into_iter()
                .map(|session| daemon::SessionEntry {
                    id: session.id.get(),
                    client: session.client,
                    machines: session.machines.iter().map(ToString::to_string).collect(),
                    answered: session.answered,
                    running: session.running,
                    state: match session.state {
                        SessionState::Active => daemon::SessionState::Active,
                        SessionState::Paused => daemon::SessionState::Paused,
                        // Ended sessions leave the table.
                        SessionState::Ended(_) => daemon::SessionState::Unspecified,
                    } as i32,
                })
                .collect(),
        }
    }

    fn via(raw: i32) -> Result<Via, Refusal> {
        match daemon::Via::try_from(raw) {
            Ok(daemon::Via::Agent) => Ok(Via::Agent),
            Ok(daemon::Via::Host) => Ok(Via::Host),
            Ok(daemon::Via::Unspecified) | Err(_) => Err(Refusal::argument("via", &raw)),
        }
    }

    fn spec(spec: Option<daemon::SnapshotSpec>) -> Result<SnapshotSpec, Refusal> {
        let spec = spec.ok_or(ParseError::Missing("spec"))?;
        let scope = match daemon::SnapshotScope::try_from(spec.scope) {
            Ok(daemon::SnapshotScope::FocusedWindow) => SnapshotScope::FocusedWindow,
            Ok(daemon::SnapshotScope::FrontmostApp) => SnapshotScope::FrontmostApp,
            Ok(daemon::SnapshotScope::Unspecified) | Err(_) => {
                return Err(Refusal::argument("spec.scope", &spec.scope));
            }
        };
        Ok(SnapshotSpec {
            scope,
            menus: ParseError::menus("spec.menus", spec.menus)?,
        })
    }

    pub(super) fn report_request(
        &self,
        request: &daemon::GetMachineReport,
        call: &Call,
    ) -> Result<daemon::MachineReport, Refusal> {
        use daemon::machine_report::Agent;
        let name = Refusal::name("name", &request.name)?;
        let report = self.status(&name, call)?;
        Ok(daemon::MachineReport {
            status: Some(report.status.into()),
            agent: Some(match &report.agent {
                AgentReport::Connected(status) => Agent::Connected(status.into()),
                AgentReport::Unreachable(why) => Agent::Unreachable(why.clone()),
                AgentReport::NotRunning => Agent::NotRunning(daemon::Done {}),
            }),
            checkpoints: report.checkpoints.iter().map(ToString::to_string).collect(),
        })
    }

    pub(super) fn screenshot_request(
        &self,
        request: &daemon::TakeScreenshot,
        call: &Call,
    ) -> Result<common::Image, Refusal> {
        use daemon::take_screenshot::Size;
        let name = Refusal::name("name", &request.name)?;
        let size = match request.size {
            Some(Size::MatchPoints(_)) => ScreenshotSize::MatchPoints,
            Some(Size::MaxPixels(n)) => ScreenshotSize::MaxPixels(n),
            None => return Err(ParseError::Missing("size").into()),
        };
        let shot = self.screenshot(&name, Self::via(request.via)?, size, call)?;
        Ok((&shot).into())
    }

    pub(super) fn screen_text_request(
        &self,
        request: &daemon::ReadScreenText,
        call: &Call,
    ) -> Result<vmm::RecognizedText, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let text = self.recognize_text(&name, call)?;
        Ok((&text).into())
    }

    pub(super) fn input_request(
        &self,
        request: daemon::SendInput,
        call: &Call,
    ) -> Result<daemon::Done, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let input = request
            .input
            .ok_or(ParseError::Missing("input"))?
            .try_into()?;
        self.input(&name, Self::via(request.via)?, &input, call)?;
        Ok(daemon::Done {})
    }

    pub(super) fn take_control_request(
        &self,
        request: &daemon::TakeControl,
        link: &Link,
    ) -> Result<daemon::Done, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        self.scheduler
            .take_control(link.connection(), &name, self.machines.clock().now())
            .map_err(|error| Refusal::State(format!("{name}: {error}")))?;
        eprintln!("doppeld: {name}: a person took control");
        Ok(daemon::Done {})
    }

    pub(super) fn hand_back_request(
        &self,
        request: &daemon::HandBack,
        link: &Link,
    ) -> Result<daemon::Done, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        self.scheduler
            .hand_back(link.connection(), &name, self.machines.clock().now())
            .map_err(|error| Refusal::State(format!("{name}: {error}")))?;
        eprintln!("doppeld: {name}: handed back to the agents");
        Ok(daemon::Done {})
    }

    pub(super) fn host_input_request(
        &self,
        request: daemon::SendHostInput,
        link: &Link,
    ) -> Result<daemon::Done, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        if !self.scheduler.holds(link.connection(), &name) {
            return Err(Refusal::State(format!(
                "{name}: take control of it before sending input"
            )));
        }
        let input = request
            .input
            .ok_or(ParseError::Missing("input"))?
            .try_into()?;
        self.host_input(&name, &input)?;
        Ok(daemon::Done {})
    }

    pub(super) fn snapshot_request(
        &self,
        request: &daemon::TakeSnapshot,
        call: &Call,
    ) -> Result<agent::AxSnapshot, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let snapshot = self.snapshot(&name, Self::spec(request.spec)?, call)?;
        Ok((&snapshot).into())
    }

    pub(super) fn act_request(
        &self,
        request: daemon::ActOnElement,
        call: &Call,
    ) -> Result<daemon::ActResult, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let act = request
            .action
            .ok_or(ParseError::Missing("action"))?
            .try_into()?;
        let outcome = match self.act(&name, &act, call)? {
            Acted::Done => daemon::ActOutcome::Done,
            Acted::StaleReference => daemon::ActOutcome::StaleReference,
        };
        Ok(daemon::ActResult {
            outcome: outcome as i32,
        })
    }

    pub(super) fn wait_request(
        &self,
        request: daemon::WaitForElement,
        call: &Call,
    ) -> Result<daemon::ElementWaitResult, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let wait = ParseError::element_wait(request.condition, request.timeout_ms)?;
        let outcome = self.wait_for(&name, wait, Self::spec(request.spec)?, call)?;
        Ok(daemon::ElementWaitResult::in_micros(&outcome))
    }

    pub(super) fn text_wait_request(
        &self,
        request: daemon::WaitForText,
        call: &Call,
    ) -> Result<daemon::TextWaitResult, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let wait = ParseError::text_wait(request.condition, request.timeout_ms)?;
        let outcome = self.wait_for_text(&name, wait, call)?;
        Ok(daemon::TextWaitResult::in_micros(&outcome))
    }

    pub(super) fn exec_request(
        &self,
        request: daemon::RunProgram,
        call: &Call,
    ) -> Result<agent::ExecResult, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let exec = request
            .exec
            .ok_or(ParseError::Missing("exec"))?
            .try_into()?;
        let output = self.exec(&name, &exec, call)?;
        Ok((&output).into())
    }

    pub(super) fn read_request(
        &self,
        request: daemon::ReadGuestFile,
        call: &Call,
    ) -> Result<agent::FileContents, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let read = request
            .read
            .ok_or(ParseError::Missing("read"))?
            .try_into()?;
        let data = self.read_file(&name, &read, call)?;
        Ok(doppel_protocol::pb::agent::v1::FileContents { data: data.into() })
    }

    pub(super) fn write_request(
        &self,
        request: daemon::WriteGuestFile,
        call: &Call,
    ) -> Result<daemon::Done, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let write = request
            .write
            .ok_or(ParseError::Missing("write"))?
            .try_into()?;
        self.write_file(&name, &write, call)?;
        Ok(daemon::Done {})
    }

    pub(super) fn rollback_request(
        &self,
        request: &daemon::RollBack,
        call: &Call,
    ) -> Result<daemon::LoginResult, Refusal> {
        let name = Refusal::name("name", &request.name)?;
        let label = Refusal::label("label", &request.label)?;
        let desktop = self.rollback(&name, &label, call)?;
        Ok(daemon::LoginResult::in_micros(&desktop))
    }
}
