//! Each daemon operation tied to the one kind of outcome that answers it.
//!
//! The table at the bottom is the only list of operations: it implements
//! [`Rpc`] for every request message and defines [`with_operation!`](crate::with_operation), which
//! runs code generic over [`Rpc`] on whichever request an [`Operation`]
//! holds. A new operation in `daemon.proto` fails to compile everywhere
//! `with_operation!` is used until it's added here.

use std::fmt;

use crate::pb::agent::v1 as agent;
use crate::pb::common::v1 as common;
use crate::pb::daemon::v1 as daemon;
use crate::pb::vmm::v1 as vmm;
use daemon::daemon_request::Operation;
use daemon::daemon_response::Outcome;

/// A daemon request, and what answers it when it succeeds. A failure is
/// answered with [`Outcome::Error`] or [`Outcome::MachineFailure`], whatever
/// the request.
pub trait Rpc: fmt::Debug + Sized {
    /// What a successful answer carries.
    type Answer: fmt::Debug;

    /// The operation's name, as `daemon.proto` spells its message.
    const NAME: &'static str;

    /// The request, ready to send.
    fn into_operation(self) -> Operation;

    /// A successful answer, ready to send.
    fn answer(answer: Self::Answer) -> Outcome;

    /// The answer an outcome carries; `None` when it's another kind.
    fn answered(outcome: Outcome) -> Option<Self::Answer>;
}

/// Runs `$body` with `$request` bound to the request an [`Operation`] (or a
/// reference to one) holds, whichever type it is: code generic over
/// [`Rpc`] dispatched once, exhaustively.
///
/// ```ignore
/// with_operation!(operation, request => self.run(request))
/// ```
#[macro_export]
macro_rules! with_operation {
    ($operation:expr, $request:ident => $body:expr) => {
        $crate::rpc::__each_operation!($operation, $request => $body)
    };
}

macro_rules! operations {
    ($d:tt $( $request:ident => $outcome:ident($answer:ty) ),* $(,)?) => {
        $(
            impl Rpc for daemon::$request {
                type Answer = $answer;
                const NAME: &'static str = stringify!($request);

                fn into_operation(self) -> Operation {
                    Operation::$request(self)
                }

                fn answer(answer: Self::Answer) -> Outcome {
                    Outcome::$outcome(answer)
                }

                fn answered(outcome: Outcome) -> Option<Self::Answer> {
                    match outcome {
                        Outcome::$outcome(answer) => Some(answer),
                        // Any other kind answers another operation.
                        _ => None,
                    }
                }
            }
        )*

        #[doc(hidden)]
        #[macro_export]
        macro_rules! __each_operation {
            ($d operation:expr, $d request:ident => $d body:expr) => {
                match $d operation {
                    $(
                        $crate::pb::daemon::v1::daemon_request::Operation::$request($d request) => $d body,
                    )*
                }
            };
        }
        #[doc(hidden)]
        pub use __each_operation;
    };
}

operations! {$
    ListMachines => MachineList(daemon::MachineList),
    GetStatus => MachineStatus(daemon::MachineStatus),
    StartMachine => MachineStarted(daemon::MachineStarted),
    AwaitDesktop => LoginResult(daemon::LoginResult),
    StopMachine => MachineStopped(daemon::MachineStopped),
    PauseMachine => Done(daemon::Done),
    ResumeMachine => MachineResumed(daemon::MachineResumed),
    TakeCheckpoint => CheckpointReport(daemon::CheckpointReport),
    ForkMachine => Done(daemon::Done),
    RestoreMachine => Done(daemon::Done),
    GetDaemonInfo => DaemonInfo(daemon::DaemonInfo),
    OpenSession => SessionOpened(daemon::SessionOpened),
    ListSessions => SessionList(daemon::SessionList),
    GetMachineReport => MachineReport(daemon::MachineReport),
    TakeScreenshot => Image(common::Image),
    ReadScreenText => ScreenText(vmm::RecognizedText),
    SendInput => Done(daemon::Done),
    TakeSnapshot => AxSnapshot(agent::AxSnapshot),
    ActOnElement => ActResult(daemon::ActResult),
    WaitForElement => ElementWait(daemon::ElementWaitResult),
    WaitForText => TextWait(daemon::TextWaitResult),
    RunProgram => ExecResult(agent::ExecResult),
    ReadGuestFile => FileContents(agent::FileContents),
    WriteGuestFile => Done(daemon::Done),
    RollBack => LoginResult(daemon::LoginResult),
    // The display's shared memory comes with the answer's frame.
    OpenDisplay => DisplayLayout(vmm::DisplayLayout),
    SendHostInput => Done(daemon::Done),
    TakeControl => Done(daemon::Done),
    HandBack => Done(daemon::Done),
    PauseSession => Done(daemon::Done),
    ResumeSession => Done(daemon::Done),
    // Then the stream's frames, as events.
    OpenEncodedDisplay => EncodedDisplay(vmm::EncodedDisplayOpened),
    RunForks => ForksRun(daemon::ForksRun),
    IssueToken => Token(daemon::TokenEntry),
    ListTokens => TokenList(daemon::TokenList),
    RevokeToken => Token(daemon::TokenEntry),
    ConfigureWeb => WebStatus(daemon::WebStatus),
    GetWebStatus => WebStatus(daemon::WebStatus),
    ListRecordings => RecordingList(daemon::RecordingList),
    OpenRecording => Recording(daemon::Recording),
    DeleteMachine => Done(daemon::Done),
    // Then the activity, as events.
    WatchActivity => Done(daemon::Done),
    OpenSeenSnapshot => AxSnapshot(agent::AxSnapshot),
    NameElements => Done(daemon::Done),
    ObserveElements => AxSnapshot(agent::AxSnapshot),
    ObserveText => ScreenText(vmm::RecognizedText),
    MarkScreen => Done(daemon::Done),
    DeleteCheckpoint => Done(daemon::Done),
    MakeTemporaryMachine => TemporaryMachineMade(daemon::TemporaryMachineMade),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_answered_by_its_own_outcome_only() {
        let snapshot = agent::AxSnapshot::default();
        let outcome = daemon::TakeSnapshot::answer(snapshot.clone());
        assert_eq!(daemon::TakeSnapshot::answered(outcome), Some(snapshot));
        let done = Outcome::Done(daemon::Done {});
        assert_eq!(daemon::TakeSnapshot::answered(done.clone()), None);
        assert_eq!(daemon::PauseMachine::answered(done), Some(daemon::Done {}));
    }

    #[test]
    fn every_operation_dispatches_to_its_own_type() {
        let operation = daemon::GetStatus { name: "dev".into() }.into_operation();
        let name = with_operation!(&operation, request => name_of(request));
        assert_eq!(name, "GetStatus");
    }

    fn name_of<R: Rpc>(_: &R) -> &'static str {
        R::NAME
    }
}
