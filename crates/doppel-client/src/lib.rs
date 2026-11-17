//! A client of doppeld, the daemon that owns a user's machines
//! (`doppel.daemon.v1` over its unix socket).
//!
//! Blocking I/O around the sans-IO codec. One [`Client`] may be shared by
//! threads: their requests run concurrently on its connection. Every
//! request carries a [`Deadline`]; each read and write gets at most the
//! time left before it.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use doppel_gateway::{TokenDigest, TokenId, TokenLabel, Ttl};
use doppel_machine::ax::tree::SnapshotSpec;
use doppel_machine::input::Via;
use doppel_machine::screen::ScreenshotSize;
use doppel_machine::vm::MachineReport;

use doppel_machine::ax::tree::{ActOn, AxSnapshot};
use doppel_machine::bundle::CheckpointLabel;
use doppel_machine::checkpoint::AfterCheckpoint;
use doppel_machine::deadline::Deadline;
use doppel_machine::exec::{ExecOutput, ExecRequest};
use doppel_machine::files::{ReadRequest, WriteRequest};
use doppel_machine::input::Input;
use doppel_machine::lineage::RecordedLineage;
use doppel_machine::login::LoginOutcome;
use doppel_machine::machine::MachineName;
use doppel_machine::screen::Screenshot;
use doppel_machine::screen_text::{ScreenText, TextQuery};
use doppel_machine::vm::{NextStart, RunState, Status};
use doppel_machine::wait::{Condition, TextCondition, WaitOutcome, WaitSpec};
use doppel_protocol::convert::{ParseError, RemoteError};
use doppel_protocol::exchange::ExchangeError;
use doppel_protocol::frame::{EncodeError, StreamError};
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_protocol::pb::vmm::v1 as vmm;
use doppel_protocol::rpc::Rpc;
use doppel_protocol::transport::Cancel;
use doppel_protocol::transport::ChannelError;

mod activity;
mod connection;
mod display;

use connection::Connection;
pub use connection::Notes;

pub use activity::ActivityStream;
pub use display::{
    Announced, Display, DisplayLayout, EncodedDisplay, EncodedFrame, EncodedSize, FrameKind,
};
pub use doppel_machine::activity::{
    Activity, Callout, CalloutLasts, CalloutShape, Ended, Ending, Gesture, OnScreen, Phase, Target,
    TextRead,
};
pub use doppel_machine::recording::{
    NotARecording, Recording, RecordingId, RecordingSummary, RecordingVideo, TraceActor, TraceCall,
    TraceLine, TraceOutcome, VideoFile,
};

pub use doppel_protocol::report::{
    Acted, CheckpointReport, CheckpointResult, Cleanup, HandedBack, MachineFailure, Resumed,
    SessionInfo, SessionStatus, StopMode, StopOutcome, StopReport, StopRequest, Window,
};

/// An issued token the daemon knows, never the token itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenEntry {
    pub id: TokenId,
    pub label: Option<TokenLabel>,
    pub issued: SystemTime,
    pub expires: SystemTime,
}

impl TokenEntry {
    fn from_wire(entry: daemon::TokenEntry) -> Result<Self, ParseError> {
        let invalid = |field| {
            move |error: doppel_gateway::TokenError| ParseError::Invalid {
                field,
                reason: error.to_string(),
            }
        };
        let at = |seconds| UNIX_EPOCH + Duration::from_secs(seconds);
        Ok(Self {
            id: entry.id.parse().map_err(invalid("token.id"))?,
            label: match entry.label.as_str() {
                "" => None,
                raw => Some(raw.parse().map_err(invalid("token.label"))?),
            },
            issued: at(entry.issued_unix_seconds),
            expires: at(entry.expires_unix_seconds),
        })
    }
}

/// What the browser viewer's listener is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebStatus {
    Off,
    Listening {
        port: u16,
        /// The certificate's SHA-256, as browsers show it.
        fingerprint: String,
        /// This Mac's names and addresses, its `.local` name first.
        names: Vec<String>,
    },
    /// The setting is on, and listening failed.
    Failed {
        port: u16,
        reason: String,
    },
}

impl WebStatus {
    fn from_wire(status: daemon::WebStatus) -> Result<Self, ParseError> {
        use daemon::web_status::State;
        let port = |port: u32| {
            u16::try_from(port).map_err(|_| ParseError::Invalid {
                field: "web_status.port",
                reason: format!("{port} isn't a TCP port"),
            })
        };
        Ok(
            match status
                .state
                .ok_or(ParseError::Missing("web_status.state"))?
            {
                State::Off(_) => Self::Off,
                State::Listening(listening) => Self::Listening {
                    port: port(listening.port)?,
                    fingerprint: listening.fingerprint,
                    names: listening.names,
                },
                State::Failed(failed) => Self::Failed {
                    port: port(failed.port)?,
                    reason: failed.reason,
                },
            },
        )
    }
}

/// A fresh fork a run made, and how its start went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkRun {
    pub name: MachineName,
    /// Its desktop, once it started; or why it didn't get there.
    pub desktop: Result<LoginOutcome, MachineFailure>,
}

/// Whether a session is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Record {
    Off,
    /// Into the daemon's store.
    IntoStore,
}

/// A session the daemon opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedSession {
    pub id: u64,
    /// Where it is recorded, when it is.
    pub recording: Option<PathBuf>,
}

/// The time requests are measured against: a port, so callers pass their
/// own clock in.
pub use doppel_machine::deadline::Clock;

/// An entry of the daemon's machines directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    Machine {
        name: MachineName,
        status: Result<Status, MachineFailure>,
        /// Its role and origin, or why they couldn't be read.
        lineage: RecordedLineage,
    },
    /// Not a valid machine name: ignored.
    Unrecognized(OsString),
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// No daemon listens on the socket: none was started, or it exited.
    #[error("no daemon is running ({0})")]
    NoDaemon(#[source] io::Error),
    #[error("cannot reach the daemon: {0}")]
    Connect(#[source] io::Error),
    #[error("the daemon speaks protocol v{theirs}; this build speaks v{ours}")]
    VersionMismatch { ours: u32, theirs: u32 },
    #[error("daemon connection failed: {0}")]
    Io(#[source] io::Error),
    #[error("the daemon closed the connection")]
    Closed,
    #[error("the daemon connection failed: {0}")]
    Lost(String),
    #[error("timed out waiting for the daemon")]
    Timeout,
    #[error(transparent)]
    Stream(#[from] StreamError),
    #[error(transparent)]
    Encode(#[from] EncodeError),
    #[error("the daemon sent an unexpected message: {0}")]
    Protocol(&'static str),
    #[error("the daemon answered {0} with another operation's outcome")]
    WrongAnswer(&'static str),
    #[error("the daemon sent an invalid message: {0}")]
    Parse(#[from] ParseError),
    #[error("the daemon refused: {0}")]
    Remote(#[from] RemoteError),
    #[error(transparent)]
    Machine(#[from] MachineFailure),
}

/// Each way a channel fails is one of the client's.
impl From<ChannelError> for ClientError {
    fn from(error: ChannelError) -> Self {
        match error {
            ChannelError::Io(error) => Self::Io(error),
            ChannelError::Closed => Self::Closed,
            ChannelError::Timeout => Self::Timeout,
            ChannelError::Encode(error) => Self::Encode(error),
            ChannelError::Exchange(ExchangeError::Stream(error)) => Self::Stream(error),
            ChannelError::Exchange(ExchangeError::VersionMismatch { ours, theirs }) => {
                Self::VersionMismatch { ours, theirs }
            }
            ChannelError::Exchange(ExchangeError::Unexpected(why)) => Self::Protocol(why),
            ChannelError::Lost(why) => Self::Lost(why),
            // The daemon's protocol can ask it to stop a request, so a
            // cancel never gives one up on this side.
            ChannelError::Cancelled => Self::Lost("the request was cancelled".into()),
        }
    }
}

impl ClientError {
    /// A socket file that's missing, or left by a daemon that exited.
    fn from_connect(error: io::Error) -> Self {
        let kind = error.kind();
        if kind == io::ErrorKind::NotFound || kind == io::ErrorKind::ConnectionRefused {
            Self::NoDaemon(error)
        } else {
            Self::Connect(error)
        }
    }
}

/// How to send one request: when to give up, how the caller describes it
/// (for a session's recording; leave out secrets), what cancels it, and
/// where the hand-back notes that come with its answer go.
/// Cancelling asks the daemon to stop it; the call still returns the
/// daemon's answer, a failure if it stopped before the end.
#[derive(Debug, Clone, Copy)]
pub struct Call<'a> {
    pub by: Deadline,
    pub caption: &'a str,
    /// Why the client makes it, in its own words, for the people watching;
    /// empty when it didn't say.
    pub why: &'a str,
    pub cancel: &'a Cancel,
    /// `None` drops them: only a session is sent any.
    pub notes: Option<&'a Notes>,
}

/// What a plain request is cancelled by: nothing.
static NEVER: Cancel = Cancel::never();

/// A plain request: uncaptioned and never cancelled.
impl From<Deadline> for Call<'static> {
    fn from(by: Deadline) -> Self {
        Self {
            by,
            caption: "",
            why: "",
            cancel: &NEVER,
            notes: None,
        }
    }
}

/// A connection to the daemon.
#[derive(Debug)]
pub struct Client<C> {
    connection: Connection<C>,
}

impl<C: Clock> Client<C> {
    /// Connects and performs the `Hello` exchange (the daemon speaks first).
    ///
    /// # Errors
    /// [`ClientError::NoDaemon`] when nothing listens on `socket`; a
    /// timeout, or a protocol version mismatch.
    pub fn connect(socket: &Path, clock: C, by: Deadline) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(socket).map_err(ClientError::from_connect)?;
        Self::over(stream, clock, by)
    }

    /// The same, over a stream already connected to a daemon, such as one
    /// end of a socket pair whose other end a daemon in this process serves.
    ///
    /// # Errors
    /// A timeout, or a protocol version mismatch.
    pub fn over(stream: UnixStream, clock: C, by: Deadline) -> Result<Self, ClientError> {
        Ok(Self {
            connection: Connection::open(stream, clock, by)?,
        })
    }

    /// Every machine, with its status or why it couldn't be read.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn machines<'a>(&self, call: impl Into<Call<'a>>) -> Result<Vec<Listing>, ClientError> {
        let list = self.call(daemon::ListMachines {}, call)?;
        let mut listings = list
            .machines
            .into_iter()
            .map(Self::listing)
            .collect::<Result<Vec<_>, _>>()?;
        listings.extend(
            list.unrecognized
                .into_iter()
                .map(|raw| Listing::Unrecognized(OsString::from_vec(raw.to_vec()))),
        );
        Ok(listings)
    }

    fn listing(entry: daemon::MachineEntry) -> Result<Listing, ClientError> {
        let name: MachineName = entry.name.parse().map_err(ParseError::from)?;
        let lineage = entry.recorded_lineage();
        let status = match entry.status {
            Some(daemon::machine_entry::Status::MachineStatus(status)) => {
                Ok(Status::try_from(status)?)
            }
            Some(daemon::machine_entry::Status::Failure(failure)) => {
                Err(MachineFailure::from_wire(failure))
            }
            None => return Err(ParseError::Missing("machine_entry.status").into()),
        };
        Ok(Listing::Machine {
            name,
            status,
            lineage,
        })
    }

    /// # Errors
    /// [`ClientError::Machine`] when the daemon couldn't read it; see
    /// [`ClientError`].
    pub fn status<'a>(
        &self,
        name: &MachineName,
        call: impl Into<Call<'a>>,
    ) -> Result<Status, ClientError> {
        let status = self.call(
            daemon::GetStatus {
                name: name.to_string(),
            },
            call,
        )?;
        Ok(status.try_into()?)
    }

    /// The data root of the store the daemon serves.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn data_root<'a>(&self, call: impl Into<Call<'a>>) -> Result<PathBuf, ClientError> {
        let info = self.call(daemon::GetDaemonInfo {}, call)?;
        Ok(PathBuf::from(OsString::from_vec(info.data_root.to_vec())))
    }

    /// Starts a stopped machine: from its saved memory if it has one.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn start<'a>(
        &self,
        name: &MachineName,
        window: Window,
        call: impl Into<Call<'a>>,
    ) -> Result<(RunState, NextStart), ClientError> {
        let window = match window {
            Window::Hidden => daemon::Window::Hidden,
            Window::Shown => daemon::Window::Shown,
        };
        let started = self.call(
            daemon::StartMachine {
                name: name.to_string(),
                window: window as i32,
            },
            call,
        )?;
        let state = started
            .state
            .ok_or(ParseError::Missing("machine_started.state"))?;
        Ok((
            <(RunState, Option<doppel_machine::vm::Window>)>::from(state).0,
            ParseError::next_start("machine_started.started_by", started.started_by)?,
        ))
    }

    /// Waits for an agent-ready desktop, typing the stored password once if
    /// the guest is locked. Every result, ready or not, is an outcome.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn await_desktop<'a>(
        &self,
        name: &MachineName,
        call: impl Into<Call<'a>>,
    ) -> Result<LoginOutcome, ClientError> {
        let result = self.call(
            daemon::AwaitDesktop {
                name: name.to_string(),
            },
            call,
        )?;
        Ok(result.try_into()?)
    }

    /// # Errors
    /// See [`ClientError`].
    pub fn stop<'a>(
        &self,
        name: &MachineName,
        mode: StopMode,
        call: impl Into<Call<'a>>,
    ) -> Result<StopReport, ClientError> {
        let mode = match mode {
            StopMode::Graceful => vmm::StopMode::Graceful,
            StopMode::Force => vmm::StopMode::Force,
        };
        let stopped = self.call(
            daemon::StopMachine {
                name: name.to_string(),
                mode: mode as i32,
            },
            call,
        )?;
        Ok(StopReport::from_wire(stopped)?)
    }

    /// # Errors
    /// See [`ClientError`].
    pub fn pause<'a>(
        &self,
        name: &MachineName,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(
            daemon::PauseMachine {
                name: name.to_string(),
            },
            call,
        )
    }

    /// # Errors
    /// See [`ClientError`].
    pub fn resume<'a>(
        &self,
        name: &MachineName,
        call: impl Into<Call<'a>>,
    ) -> Result<Resumed, ClientError> {
        let resumed = self.call(
            daemon::ResumeMachine {
                name: name.to_string(),
            },
            call,
        )?;
        match daemon::ResumeOutcome::try_from(resumed.outcome) {
            Ok(daemon::ResumeOutcome::Resumed) => Ok(Resumed::Resumed),
            Ok(daemon::ResumeOutcome::AlreadyRunning) => Ok(Resumed::AlreadyRunning),
            Ok(daemon::ResumeOutcome::Unspecified) | Err(_) => Err(ParseError::UnknownEnum {
                field: "machine_resumed.outcome",
                value: resumed.outcome,
            }
            .into()),
        }
    }

    /// # Errors
    /// See [`ClientError`]. A checkpoint that ran and failed is a report,
    /// not an error.
    pub fn checkpoint<'a>(
        &self,
        name: &MachineName,
        label: &CheckpointLabel,
        then: AfterCheckpoint,
        call: impl Into<Call<'a>>,
    ) -> Result<CheckpointReport, ClientError> {
        let report = self.call(
            daemon::TakeCheckpoint {
                name: name.to_string(),
                label: label.to_string(),
                then: daemon::AfterCheckpoint::from(then) as i32,
            },
            call,
        )?;
        Ok(CheckpointReport::from_wire(report)?)
    }

    /// Makes `into`, a new machine, from `source`'s checkpoint.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn fork<'a>(
        &self,
        source: &MachineName,
        label: &CheckpointLabel,
        into: &MachineName,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(
            daemon::ForkMachine {
                source: source.to_string(),
                label: label.to_string(),
                into: into.to_string(),
            },
            call,
        )
    }

    /// Makes `count` fresh forks of `source`'s checkpoint, starts them
    /// side by side and waits for their desktops. Refused before any is
    /// made when they don't fit beside the running machines.
    ///
    /// # Errors
    /// See [`ClientError`]; a fork that didn't start is in its [`ForkRun`].
    pub fn run_forks<'a>(
        &self,
        source: &MachineName,
        label: &CheckpointLabel,
        count: u32,
        call: impl Into<Call<'a>>,
    ) -> Result<Vec<ForkRun>, ClientError> {
        let run = self.call(
            daemon::RunForks {
                name: source.to_string(),
                label: label.to_string(),
                count,
            },
            call,
        )?;
        run.forks.into_iter().map(Self::fork_run).collect()
    }

    /// Turns the browser viewer's listener on, on `port`, or off (`None`),
    /// and saves that; answers once it has started, failed or stopped.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn configure_web<'a>(
        &self,
        port: Option<u16>,
        call: impl Into<Call<'a>>,
    ) -> Result<WebStatus, ClientError> {
        let listen = match port {
            Some(port) => daemon::configure_web::Listen::On(daemon::WebPort {
                port: u32::from(port),
            }),
            None => daemon::configure_web::Listen::Off(daemon::Done {}),
        };
        let status = self.call(
            daemon::ConfigureWeb {
                listen: Some(listen),
            },
            call,
        )?;
        Ok(WebStatus::from_wire(status)?)
    }

    /// What the browser viewer's listener is doing.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn web_status<'a>(&self, call: impl Into<Call<'a>>) -> Result<WebStatus, ClientError> {
        let status = self.call(daemon::GetWebStatus {}, call)?;
        Ok(WebStatus::from_wire(status)?)
    }

    /// Records a token by its digest, for as long as `ttl`. The token
    /// itself never reaches the daemon.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn issue_token<'a>(
        &self,
        digest: &TokenDigest,
        ttl: Ttl,
        label: Option<&TokenLabel>,
        call: impl Into<Call<'a>>,
    ) -> Result<TokenEntry, ClientError> {
        let ttl_seconds = u32::try_from(ttl.as_duration().as_secs())
            .map_err(|_| ClientError::Protocol("a token's lifetime fits 32 bits"))?;
        let entry = self.call(
            daemon::IssueToken {
                sha256: digest.as_bytes().to_vec().into(),
                ttl_seconds,
                label: label.map_or_else(String::new, ToString::to_string),
            },
            call,
        )?;
        Ok(TokenEntry::from_wire(entry)?)
    }

    /// The tokens that let their holders in now, oldest first.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn tokens<'a>(&self, call: impl Into<Call<'a>>) -> Result<Vec<TokenEntry>, ClientError> {
        let list = self.call(daemon::ListTokens {}, call)?;
        Ok(list
            .tokens
            .into_iter()
            .map(TokenEntry::from_wire)
            .collect::<Result<_, _>>()?)
    }

    /// Revokes a token: it lets no one in from now on.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn revoke_token<'a>(
        &self,
        id: &TokenId,
        call: impl Into<Call<'a>>,
    ) -> Result<TokenEntry, ClientError> {
        let entry = self.call(daemon::RevokeToken { id: id.to_string() }, call)?;
        Ok(TokenEntry::from_wire(entry)?)
    }

    fn fork_run(fork: daemon::RunFork) -> Result<ForkRun, ClientError> {
        let name: MachineName = fork.name.parse().map_err(ParseError::from)?;
        let desktop = match fork.outcome {
            Some(daemon::run_fork::Outcome::Desktop(result)) => Ok(result.try_into()?),
            Some(daemon::run_fork::Outcome::Failure(failure)) => {
                Err(MachineFailure::from_wire(failure))
            }
            None => return Err(ParseError::Missing("run_fork.outcome").into()),
        };
        Ok(ForkRun { name, desktop })
    }

    /// Makes a stopped machine's next start resume at its checkpoint.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn restore<'a>(
        &self,
        name: &MachineName,
        label: &CheckpointLabel,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(
            daemon::RestoreMachine {
                name: name.to_string(),
                label: label.to_string(),
            },
            call,
        )
    }
    /// Opens a session on this connection: the requests sent on it are
    /// the session's, and it ends when the connection closes. With
    /// [`Record::IntoStore`], the daemon records each machine the session
    /// touches into its store, captioned with each request's caption, with a
    /// trace of the requests.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn open_session<'a>(
        &self,
        client: &str,
        record: Record,
        call: impl Into<Call<'a>>,
    ) -> Result<OpenedSession, ClientError> {
        let opened = self.call(
            daemon::OpenSession {
                client: client.to_owned(),
                record: match record {
                    Record::Off => false,
                    Record::IntoStore => true,
                },
            },
            call,
        )?;
        Ok(OpenedSession {
            id: opened.session,
            recording: opened
                .recording
                .map(|raw| PathBuf::from(OsString::from_vec(raw.to_vec()))),
        })
    }

    /// # Errors
    /// See [`ClientError`].
    pub fn pause_session<'a>(
        &self,
        session: u64,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(daemon::PauseSession { session }, call)
    }

    /// # Errors
    /// See [`ClientError`].
    pub fn resume_session<'a>(
        &self,
        session: u64,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(daemon::ResumeSession { session }, call)
    }

    /// Takes control of `name` for a person on this connection: sessions'
    /// calls on it wait until [`Client::hand_back`], or until this
    /// connection closes. Only this connection may then send it
    /// [`Client::host_input`].
    ///
    /// # Errors
    /// See [`ClientError`]: refused while another connection has it.
    pub fn take_control<'a>(
        &self,
        name: &MachineName,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(
            daemon::TakeControl {
                name: name.to_string(),
            },
            call,
        )
    }

    /// # Errors
    /// See [`ClientError`].
    pub fn hand_back<'a>(
        &self,
        name: &MachineName,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(
            daemon::HandBack {
                name: name.to_string(),
            },
            call,
        )
    }

    /// Tells the daemon what this session named the elements of a snapshot
    /// of `name` one of its calls saw, so viewers show the same names.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn name_elements<'a>(
        &self,
        name: &MachineName,
        refs: &doppel_machine::ax::refs::SnapshotRefs,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(
            daemon::NameElements {
                machine: name.to_string(),
                refs: Some(refs.into()),
            },
            call,
        )
    }

    /// Puts `mark` on `name`'s screen for the people watching the session.
    ///
    /// # Errors
    /// See [`ClientError`]: refused outside a session, and for an element
    /// whose snapshot the daemon no longer keeps.
    pub fn mark<'a>(
        &self,
        name: &MachineName,
        mark: &doppel_machine::mark::Mark,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(daemon::MarkScreen::new(name.to_string(), mark), call)
    }

    /// # Errors
    /// See [`ClientError`].
    pub fn sessions<'a>(&self, call: impl Into<Call<'a>>) -> Result<Vec<SessionInfo>, ClientError> {
        let list = self.call(daemon::ListSessions {}, call)?;
        Ok(list
            .sessions
            .into_iter()
            .map(SessionInfo::from_wire)
            .collect::<Result<_, _>>()?)
    }

    /// The machine's state, its agent's, and its checkpoints.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn report<'a>(
        &self,
        name: &MachineName,
        call: impl Into<Call<'a>>,
    ) -> Result<MachineReport, ClientError> {
        let report = self.call(
            daemon::GetMachineReport {
                name: name.to_string(),
            },
            call,
        )?;
        Ok(MachineReport::try_from(report)?)
    }

    /// # Errors
    /// See [`ClientError`].
    pub fn screenshot<'a>(
        &self,
        name: &MachineName,
        via: Via,
        size: ScreenshotSize,
        call: impl Into<Call<'a>>,
    ) -> Result<Screenshot, ClientError> {
        use daemon::take_screenshot::Size;
        let image = self.call(
            daemon::TakeScreenshot {
                name: name.to_string(),
                via: daemon::Via::from(via) as i32,
                size: Some(match size {
                    ScreenshotSize::MatchPoints => Size::MatchPoints(daemon::Done {}),
                    ScreenshotSize::MaxPixels(n) => Size::MaxPixels(n),
                }),
            },
            call,
        )?;
        Ok(image.try_into()?)
    }

    /// Text recognition on the host's view of the display. `looking_for`
    /// is what the caller will look for in it, for viewers to show where it
    /// was found; the text is the same without it.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn screen_text<'a>(
        &self,
        name: &MachineName,
        looking_for: Option<&TextQuery>,
        call: impl Into<Call<'a>>,
    ) -> Result<ScreenText, ClientError> {
        let text = self.call(
            daemon::ReadScreenText {
                name: name.to_string(),
                query: looking_for.map(|query| query.as_str().to_owned()),
            },
            call,
        )?;
        Ok(text.try_into()?)
    }

    /// # Errors
    /// See [`ClientError`].
    /// Input from a person at a viewer, through the VMM's virtual devices.
    /// Unlike [`Client::input`], it doesn't wait for the machine's queue.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn host_input<'a>(
        &self,
        name: &MachineName,
        input: &Input,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(
            daemon::SendHostInput {
                name: name.to_string(),
                input: Some(input.into()),
            },
            call,
        )
    }

    pub fn input<'a>(
        &self,
        name: &MachineName,
        via: Via,
        input: &Input,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(
            daemon::SendInput {
                name: name.to_string(),
                via: daemon::Via::from(via) as i32,
                input: Some(input.into()),
            },
            call,
        )
    }

    /// # Errors
    /// See [`ClientError`].
    pub fn snapshot<'a>(
        &self,
        name: &MachineName,
        spec: SnapshotSpec,
        call: impl Into<Call<'a>>,
    ) -> Result<AxSnapshot, ClientError> {
        let snapshot = self.call(
            daemon::TakeSnapshot {
                name: name.to_string(),
                spec: Some(spec.into()),
            },
            call,
        )?;
        Ok(snapshot.try_into()?)
    }

    /// # Errors
    /// See [`ClientError`].
    pub fn act<'a>(
        &self,
        name: &MachineName,
        act: &ActOn,
        call: impl Into<Call<'a>>,
    ) -> Result<Acted, ClientError> {
        let result = self.call(
            daemon::ActOnElement {
                name: name.to_string(),
                action: Some(act.into()),
            },
            call,
        )?;
        match daemon::ActOutcome::try_from(result.outcome) {
            Ok(daemon::ActOutcome::Done) => Ok(Acted::Done),
            Ok(daemon::ActOutcome::StaleReference) => Ok(Acted::StaleReference),
            Ok(daemon::ActOutcome::Unspecified) | Err(_) => Err(ParseError::UnknownEnum {
                field: "act_result.outcome",
                value: result.outcome,
            }
            .into()),
        }
    }

    /// Waits on accessibility snapshots until the condition holds.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn wait_for<'a>(
        &self,
        name: &MachineName,
        wait: &WaitSpec<Condition>,
        spec: SnapshotSpec,
        call: impl Into<Call<'a>>,
    ) -> Result<WaitOutcome<AxSnapshot>, ClientError> {
        let result = self.call(
            daemon::WaitForElement {
                name: name.to_string(),
                condition: Some(wait.condition().into()),
                timeout_ms: Self::millis(wait.timeout()),
                spec: Some(spec.into()),
            },
            call,
        )?;
        Ok(result.try_into()?)
    }

    /// Waits on text recognition until the text appears or disappears.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn wait_for_text<'a>(
        &self,
        name: &MachineName,
        wait: &WaitSpec<TextCondition>,
        call: impl Into<Call<'a>>,
    ) -> Result<WaitOutcome<ScreenText>, ClientError> {
        let result = self.call(
            daemon::WaitForText {
                name: name.to_string(),
                condition: Some(wait.condition().into()),
                timeout_ms: Self::millis(wait.timeout()),
            },
            call,
        )?;
        Ok(result.try_into()?)
    }

    /// Runs a program; interrupting the call stops it.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn exec<'a>(
        &self,
        name: &MachineName,
        request: &ExecRequest,
        call: impl Into<Call<'a>>,
    ) -> Result<ExecOutput, ClientError> {
        let result = self.call(
            daemon::RunProgram {
                name: name.to_string(),
                exec: Some(request.into()),
            },
            call,
        )?;
        Ok(result.try_into()?)
    }

    /// # Errors
    /// See [`ClientError`].
    pub fn read_file<'a>(
        &self,
        name: &MachineName,
        request: &ReadRequest,
        call: impl Into<Call<'a>>,
    ) -> Result<Vec<u8>, ClientError> {
        let contents = self.call(
            daemon::ReadGuestFile {
                name: name.to_string(),
                read: Some(request.into()),
            },
            call,
        )?;
        Ok(contents.data.to_vec())
    }

    /// # Errors
    /// See [`ClientError`].
    pub fn write_file<'a>(
        &self,
        name: &MachineName,
        request: &WriteRequest,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(
            daemon::WriteGuestFile {
                name: name.to_string(),
                write: Some(request.into()),
            },
            call,
        )
    }

    /// The store's recordings, newest first. Entries whose names aren't a
    /// recording's are left out.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn recordings<'a>(
        &self,
        call: impl Into<Call<'a>>,
    ) -> Result<Vec<RecordingSummary>, ClientError> {
        let list = self.call(daemon::ListRecordings {}, call)?;
        Ok(list
            .recordings
            .into_iter()
            .filter_map(|summary| RecordingSummary::try_from(summary).ok())
            .collect())
    }

    /// A recording's videos and trace.
    ///
    /// # Errors
    /// See [`ClientError`]: refused for a recording the store doesn't have.
    pub fn open_recording<'a>(
        &self,
        id: &RecordingId,
        call: impl Into<Call<'a>>,
    ) -> Result<Recording, ClientError> {
        let recording = self.call(daemon::OpenRecording { id: id.to_string() }, call)?;
        Recording::try_from(recording)
            .map_err(|_| ClientError::Protocol("a recording without its id"))
    }

    /// The snapshot line `line` (from 0) of recording `id`'s trace kept.
    ///
    /// # Errors
    /// See [`ClientError`]: refused when that line kept none.
    pub fn seen_snapshot<'a>(
        &self,
        id: &RecordingId,
        line: u32,
        call: impl Into<Call<'a>>,
    ) -> Result<AxSnapshot, ClientError> {
        let snapshot = self.call(
            daemon::OpenSeenSnapshot {
                recording: id.to_string(),
                line,
            },
            call,
        )?;
        Ok(snapshot.try_into()?)
    }

    /// A person's own read of `name`'s screen text, without disturbing the
    /// agents driving it.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn observe_text<'a>(
        &self,
        name: &MachineName,
        call: impl Into<Call<'a>>,
    ) -> Result<ScreenText, ClientError> {
        let text = self.call(
            daemon::ObserveText {
                name: name.to_string(),
            },
            call,
        )?;
        Ok(text.try_into()?)
    }

    /// A person's look at `name`'s elements: its frontmost app's focused
    /// window, without disturbing the agents driving it.
    ///
    /// # Errors
    /// See [`ClientError`]: refused when the guest's agent is too old to
    /// look without keeping the snapshot.
    pub fn observe<'a>(
        &self,
        name: &MachineName,
        call: impl Into<Call<'a>>,
    ) -> Result<AxSnapshot, ClientError> {
        let snapshot = self.call(
            daemon::ObserveElements {
                name: name.to_string(),
            },
            call,
        )?;
        Ok(snapshot.try_into()?)
    }

    /// Makes a fresh fork of `golden`'s template for this connection,
    /// stopped and marked temporary. The daemon powers it off and deletes
    /// it when the connection closes, however that happens.
    ///
    /// # Errors
    /// See [`ClientError`]: refused for a machine that isn't a golden
    /// image, and when one more wouldn't fit beside the running machines.
    pub fn make_temporary_machine<'a>(
        &self,
        golden: &MachineName,
        call: impl Into<Call<'a>>,
    ) -> Result<MachineName, ClientError> {
        let made = self.call(
            daemon::MakeTemporaryMachine {
                golden: golden.to_string(),
            },
            call,
        )?;
        Ok(made.name.parse().map_err(ParseError::from)?)
    }

    /// Removes a stopped machine, its disk and its checkpoints, for good.
    ///
    /// # Errors
    /// See [`ClientError`]: refused while it runs.
    pub fn delete<'a>(
        &self,
        name: &MachineName,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(
            daemon::DeleteMachine {
                name: name.to_string(),
            },
            call,
        )
    }

    /// Deletes one of a machine's checkpoints, for good, running or not.
    ///
    /// # Errors
    /// See [`ClientError`]: refused for an unknown label, for a golden
    /// image's template, and while the machine's lineage can't be read.
    pub fn delete_checkpoint<'a>(
        &self,
        name: &MachineName,
        label: &CheckpointLabel,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        self.done(
            daemon::DeleteCheckpoint {
                name: name.to_string(),
                label: label.to_string(),
            },
            call,
        )
    }

    /// Powers off, restores the checkpoint, starts hidden and waits for the
    /// desktop.
    ///
    /// # Errors
    /// See [`ClientError`]. A failure once it has begun may have changed
    /// the machine.
    pub fn rollback<'a>(
        &self,
        name: &MachineName,
        label: &CheckpointLabel,
        call: impl Into<Call<'a>>,
    ) -> Result<LoginOutcome, ClientError> {
        let result = self.call(
            daemon::RollBack {
                name: name.to_string(),
                label: label.to_string(),
            },
            call,
        )?;
        Ok(result.try_into()?)
    }

    /// Milliseconds for the wire; waits are at most a minute.
    fn millis(duration: std::time::Duration) -> u32 {
        u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
    }

    /// Sends a request whose answer is only that it was done.
    fn done<'a, R: Rpc<Answer = daemon::Done>>(
        &self,
        request: R,
        call: impl Into<Call<'a>>,
    ) -> Result<(), ClientError> {
        let daemon::Done {} = self.call(request, call)?;
        Ok(())
    }

    fn call<'a, R: Rpc>(
        &self,
        request: R,
        call: impl Into<Call<'a>>,
    ) -> Result<R::Answer, ClientError> {
        self.connection.call(request, call.into())
    }
}
