//! Client for one VMM process over its unix socket.
//!
//! Typed requests over a [`Channel`]: descriptors the VMM attaches
//! (`SCM_RIGHTS`, for the display's memory and agent channels) come with the
//! response they were sent with. Every request carries a [`Deadline`].

use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use doppel_machine::bundle::SocketPath;
use doppel_machine::deadline::Deadline;
use doppel_machine::outcome::{Certainty, Failure};
use doppel_machine::vm::RunState;
use doppel_protocol::convert::RemoteError;
use doppel_protocol::exchange::{ExchangeConfig, ExchangeError};
use doppel_protocol::frame::{EncodeError, StreamError};
use doppel_protocol::pb::common::v1 as common;
use doppel_protocol::pb::vmm::v1 as vmm;
use doppel_protocol::transport::{Channel, ChannelError, Waited, Wakeup};

use crate::capabilities::Clock;

/// What [`VmmClient::start_recording`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum RecordingStart {
    Started,
    /// A recording to this file was already running; nothing changed.
    AlreadyRecording(PathBuf),
}

/// A finished recording.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingSummary {
    pub path: PathBuf,
    /// Frames encoded: one per change of the screen, not a fixed rate.
    pub frames: u64,
    pub seconds: f64,
    pub bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum VmmError {
    #[error("cannot reach the VMM: {0}")]
    Connect(#[source] io::Error),
    #[error("VMM speaks protocol v{theirs}; this build speaks v{ours}")]
    VersionMismatch { ours: u32, theirs: u32 },
    #[error("VMM connection failed: {0}")]
    Io(#[source] io::Error),
    #[error("VMM closed the connection")]
    Closed,
    #[error("timed out waiting for the VMM")]
    Timeout,
    #[error(transparent)]
    Stream(#[from] StreamError),
    #[error(transparent)]
    Encode(#[from] EncodeError),
    #[error("VMM sent an unexpected message: {0}")]
    Protocol(&'static str),
    #[error("VMM refused: {0}")]
    Remote(#[from] RemoteError),
}

/// Once a request may have reached the VMM, a broken or garbled exchange
/// leaves its state unknown. Failing to connect or to encode sent nothing.
impl Failure for VmmError {
    fn certainty(&self) -> Certainty {
        match self {
            Self::Io(_) | Self::Closed | Self::Timeout | Self::Stream(_) | Self::Protocol(_) => {
                Certainty::MaybeDone
            }
            Self::Connect(_) | Self::VersionMismatch { .. } | Self::Encode(_) => Certainty::NotDone,
            Self::Remote(error) => error.certainty(),
        }
    }
}

/// Each way a channel fails is one of the VMM client's.
impl From<ChannelError> for VmmError {
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
            // Only a channel shared between callers fails these ways; this
            // client owns its own.
            ChannelError::Lost(why) => Self::Io(io::Error::other(why)),
            ChannelError::Cancelled => Self::Closed,
        }
    }
}

pub use doppel_machine::stop::StopMode;

#[derive(Debug)]
pub struct VmmClient<'c, C> {
    channel: Channel<vmm::VmmFrame, &'c C>,
}

impl<'c, C: Clock> VmmClient<'c, C> {
    /// Connects and performs the `Hello` exchange (VMM speaks first).
    ///
    /// # Errors
    /// Unreachable socket, timeout, or protocol version mismatch.
    pub fn connect(socket: &SocketPath, clock: &'c C, by: Deadline) -> Result<Self, VmmError> {
        let stream = UnixStream::connect(socket.path()).map_err(VmmError::Connect)?;
        let channel = Channel::open(stream, clock, ExchangeConfig::STRICT, by)?;
        Ok(Self { channel })
    }

    /// Sends a request and waits for its outcome, with the descriptors that
    /// came with it. Events that arrive meanwhile are queued for
    /// [`Self::next_event`].
    fn call(
        &mut self,
        operation: vmm::vmm_request::Operation,
        by: Deadline,
    ) -> Result<(vmm::vmm_response::Outcome, Vec<OwnedFd>), VmmError> {
        let answer = self.channel.call(operation, by)?;
        match answer.response.outcome {
            Some(vmm::vmm_response::Outcome::Error(error)) => Err(RemoteError::from(error).into()),
            Some(outcome) => Ok((outcome, answer.attached)),
            None => Err(VmmError::Protocol("response without outcome")),
        }
    }

    /// The next pushed event, waiting until `by` at most.
    ///
    /// # Errors
    /// Timeout, or the connection closed (the VMM exits when the VM stops).
    pub fn next_event(&mut self, by: Deadline) -> Result<vmm::VmmEvent, VmmError> {
        self.channel.next_event(by)?.ok_or(VmmError::Timeout)
    }

    /// The next pushed event, however long it takes, unless `wakeup` is
    /// woken first: then `None`.
    ///
    /// # Errors
    /// The connection closed (the VMM exits when the VM stops).
    pub fn wait_event(&mut self, wakeup: &Wakeup) -> Result<Option<vmm::VmmEvent>, VmmError> {
        match self.channel.wait_event(None, wakeup)? {
            Waited::Event(event) => Ok(Some(event)),
            Waited::WokenUp => Ok(None),
            // It had no deadline.
            Waited::TimedOut => Err(VmmError::Timeout),
        }
    }

    /// The outcome of a request that passes no descriptors.
    fn call_outcome(
        &mut self,
        operation: vmm::vmm_request::Operation,
        by: Deadline,
    ) -> Result<vmm::vmm_response::Outcome, VmmError> {
        self.call(operation, by).map(|(outcome, _)| outcome)
    }

    fn expect_done(outcome: vmm::vmm_response::Outcome) -> Result<(), VmmError> {
        match outcome {
            vmm::vmm_response::Outcome::Done(_) => Ok(()),
            vmm::vmm_response::Outcome::Error(_)
            | vmm::vmm_response::Outcome::State(_)
            | vmm::vmm_response::Outcome::StateSaved(_)
            | vmm::vmm_response::Outcome::Image(_)
            | vmm::vmm_response::Outcome::AgentChannelOpened(_)
            | vmm::vmm_response::Outcome::RecognizedText(_)
            | vmm::vmm_response::Outcome::DisplayOpened(_)
            | vmm::vmm_response::Outcome::EncodedDisplayOpened(_)
            | vmm::vmm_response::Outcome::RecordingStarted(_)
            | vmm::vmm_response::Outcome::RecordingStopped(_) => {
                Err(VmmError::Protocol("expected Done"))
            }
        }
    }

    /// # Errors
    /// See [`VmmError`].
    pub fn state(&mut self, by: Deadline) -> Result<RunState, VmmError> {
        match self.call_outcome(vmm::vmm_request::Operation::GetState(vmm::GetState {}), by)? {
            vmm::vmm_response::Outcome::State(state) => Ok(state.into()),
            vmm::vmm_response::Outcome::Error(_)
            | vmm::vmm_response::Outcome::Done(_)
            | vmm::vmm_response::Outcome::StateSaved(_)
            | vmm::vmm_response::Outcome::Image(_)
            | vmm::vmm_response::Outcome::AgentChannelOpened(_)
            | vmm::vmm_response::Outcome::RecognizedText(_)
            | vmm::vmm_response::Outcome::DisplayOpened(_)
            | vmm::vmm_response::Outcome::EncodedDisplayOpened(_)
            | vmm::vmm_response::Outcome::RecordingStarted(_)
            | vmm::vmm_response::Outcome::RecordingStopped(_) => {
                Err(VmmError::Protocol("expected State"))
            }
        }
    }

    /// # Errors
    /// See [`VmmError`].
    pub fn pause(&mut self, by: Deadline) -> Result<(), VmmError> {
        let outcome = self.call_outcome(vmm::vmm_request::Operation::Pause(vmm::Pause {}), by)?;
        Self::expect_done(outcome)
    }

    /// # Errors
    /// See [`VmmError`].
    pub fn resume(&mut self, by: Deadline) -> Result<(), VmmError> {
        let outcome = self.call_outcome(vmm::vmm_request::Operation::Resume(vmm::Resume {}), by)?;
        Self::expect_done(outcome)
    }

    /// # Errors
    /// See [`VmmError`].
    pub fn stop(&mut self, mode: StopMode, by: Deadline) -> Result<(), VmmError> {
        let mode = match mode {
            StopMode::Graceful => vmm::StopMode::Graceful,
            StopMode::Force => vmm::StopMode::Force,
        };
        let outcome = self.call_outcome(
            vmm::vmm_request::Operation::Stop(vmm::Stop { mode: mode as i32 }),
            by,
        )?;
        Self::expect_done(outcome)
    }

    /// Saves memory state. The VM must be paused.
    ///
    /// # Errors
    /// See [`VmmError`].
    pub fn save_state(&mut self, path: PathBuf, by: Deadline) -> Result<u64, VmmError> {
        let path = path.to_string_lossy().into_owned();
        match self.call_outcome(
            vmm::vmm_request::Operation::SaveState(vmm::SaveState { path }),
            by,
        )? {
            vmm::vmm_response::Outcome::StateSaved(saved) => Ok(saved.bytes),
            vmm::vmm_response::Outcome::Error(_)
            | vmm::vmm_response::Outcome::State(_)
            | vmm::vmm_response::Outcome::Done(_)
            | vmm::vmm_response::Outcome::Image(_)
            | vmm::vmm_response::Outcome::AgentChannelOpened(_)
            | vmm::vmm_response::Outcome::RecognizedText(_)
            | vmm::vmm_response::Outcome::DisplayOpened(_)
            | vmm::vmm_response::Outcome::EncodedDisplayOpened(_)
            | vmm::vmm_response::Outcome::RecordingStarted(_)
            | vmm::vmm_response::Outcome::RecordingStopped(_) => {
                Err(VmmError::Protocol("expected StateSaved"))
            }
        }
    }

    /// Host-side screen capture: works before login, needs no guest agent.
    ///
    /// # Errors
    /// See [`VmmError`].
    pub fn capture_display_png(
        &mut self,
        max_dimension_pixels: u32,
        by: Deadline,
    ) -> Result<common::Image, VmmError> {
        let request = vmm::CaptureDisplay {
            encoding: common::ImageEncoding::Png as i32,
            max_dimension_pixels,
        };
        match self.call_outcome(vmm::vmm_request::Operation::CaptureDisplay(request), by)? {
            vmm::vmm_response::Outcome::Image(image) => Ok(image),
            vmm::vmm_response::Outcome::Error(_)
            | vmm::vmm_response::Outcome::State(_)
            | vmm::vmm_response::Outcome::Done(_)
            | vmm::vmm_response::Outcome::StateSaved(_)
            | vmm::vmm_response::Outcome::AgentChannelOpened(_)
            | vmm::vmm_response::Outcome::RecognizedText(_)
            | vmm::vmm_response::Outcome::DisplayOpened(_)
            | vmm::vmm_response::Outcome::EncodedDisplayOpened(_)
            | vmm::vmm_response::Outcome::RecordingStarted(_)
            | vmm::vmm_response::Outcome::RecordingStopped(_) => {
                Err(VmmError::Protocol("expected Image"))
            }
        }
    }

    /// Text recognition on the VM's display (Vision, in the VMM): works
    /// wherever host capture does.
    ///
    /// # Errors
    /// See [`VmmError`].
    pub fn recognize_text(&mut self, by: Deadline) -> Result<vmm::RecognizedText, VmmError> {
        match self.call_outcome(
            vmm::vmm_request::Operation::RecognizeText(vmm::RecognizeText {}),
            by,
        )? {
            vmm::vmm_response::Outcome::RecognizedText(text) => Ok(text),
            vmm::vmm_response::Outcome::Error(_)
            | vmm::vmm_response::Outcome::State(_)
            | vmm::vmm_response::Outcome::Done(_)
            | vmm::vmm_response::Outcome::StateSaved(_)
            | vmm::vmm_response::Outcome::Image(_)
            | vmm::vmm_response::Outcome::AgentChannelOpened(_)
            | vmm::vmm_response::Outcome::DisplayOpened(_)
            | vmm::vmm_response::Outcome::EncodedDisplayOpened(_)
            | vmm::vmm_response::Outcome::RecordingStarted(_)
            | vmm::vmm_response::Outcome::RecordingStopped(_) => {
                Err(VmmError::Protocol("expected RecognizedText"))
            }
        }
    }

    /// Host-side input: keyboard and pointer events into the VM's devices.
    ///
    /// # Errors
    /// See [`VmmError`].
    pub fn host_input(&mut self, input: common::Input, by: Deadline) -> Result<(), VmmError> {
        let outcome = self.call_outcome(vmm::vmm_request::Operation::HostInput(input), by)?;
        Self::expect_done(outcome)
    }

    /// Starts recording the display to `path`, an absolute path that must
    /// not exist yet. Recording continues after this connection closes,
    /// until [`Self::stop_recording`] or the VM stops.
    ///
    /// # Errors
    /// See [`VmmError`].
    pub fn start_recording(
        &mut self,
        path: &Path,
        by: Deadline,
    ) -> Result<RecordingStart, VmmError> {
        let path = path.to_string_lossy().into_owned();
        match self.call_outcome(
            vmm::vmm_request::Operation::StartRecording(vmm::StartRecording { path }),
            by,
        )? {
            vmm::vmm_response::Outcome::RecordingStarted(started) => {
                match vmm::RecordingStart::try_from(started.start) {
                    Ok(vmm::RecordingStart::Started) => Ok(RecordingStart::Started),
                    Ok(vmm::RecordingStart::AlreadyRecording) => {
                        Ok(RecordingStart::AlreadyRecording(started.path.into()))
                    }
                    Ok(vmm::RecordingStart::Unspecified) | Err(_) => {
                        Err(VmmError::Protocol("RecordingStarted without a known start"))
                    }
                }
            }
            vmm::vmm_response::Outcome::Error(_)
            | vmm::vmm_response::Outcome::State(_)
            | vmm::vmm_response::Outcome::Done(_)
            | vmm::vmm_response::Outcome::StateSaved(_)
            | vmm::vmm_response::Outcome::Image(_)
            | vmm::vmm_response::Outcome::RecognizedText(_)
            | vmm::vmm_response::Outcome::AgentChannelOpened(_)
            | vmm::vmm_response::Outcome::DisplayOpened(_)
            | vmm::vmm_response::Outcome::EncodedDisplayOpened(_)
            | vmm::vmm_response::Outcome::RecordingStopped(_) => {
                Err(VmmError::Protocol("expected RecordingStarted"))
            }
        }
    }

    /// Stops the recording to `path`, or with `None` whichever is running,
    /// once its file is complete.
    ///
    /// # Errors
    /// See [`VmmError`]; refused when no such recording is running.
    pub fn stop_recording(
        &mut self,
        path: Option<&Path>,
        by: Deadline,
    ) -> Result<RecordingSummary, VmmError> {
        let path = path.map(|path| path.to_string_lossy().into_owned());
        match self.call_outcome(
            vmm::vmm_request::Operation::StopRecording(vmm::StopRecording { path }),
            by,
        )? {
            vmm::vmm_response::Outcome::RecordingStopped(stopped) => Ok(RecordingSummary {
                path: stopped.path.into(),
                frames: stopped.frames,
                seconds: stopped.seconds,
                bytes: stopped.bytes,
            }),
            vmm::vmm_response::Outcome::Error(_)
            | vmm::vmm_response::Outcome::State(_)
            | vmm::vmm_response::Outcome::Done(_)
            | vmm::vmm_response::Outcome::StateSaved(_)
            | vmm::vmm_response::Outcome::Image(_)
            | vmm::vmm_response::Outcome::RecognizedText(_)
            | vmm::vmm_response::Outcome::AgentChannelOpened(_)
            | vmm::vmm_response::Outcome::DisplayOpened(_)
            | vmm::vmm_response::Outcome::EncodedDisplayOpened(_)
            | vmm::vmm_response::Outcome::RecordingStarted(_) => {
                Err(VmmError::Protocol("expected RecordingStopped"))
            }
        }
    }

    /// Opens the shared display: its layout and the shared memory holding
    /// the frames. `DisplayFrame` events then arrive on this connection.
    ///
    /// # Errors
    /// See [`VmmError`].
    pub fn open_display(
        &mut self,
        by: Deadline,
    ) -> Result<(vmm::DisplayLayout, OwnedFd), VmmError> {
        let (outcome, attached) = self.call(
            vmm::vmm_request::Operation::OpenDisplay(vmm::OpenDisplay {}),
            by,
        )?;
        match outcome {
            vmm::vmm_response::Outcome::DisplayOpened(opened) => {
                let layout = opened
                    .layout
                    .ok_or(VmmError::Protocol("DisplayOpened without a layout"))?;
                let memory = attached.into_iter().next().ok_or(VmmError::Protocol(
                    "DisplayOpened without a file descriptor",
                ))?;
                Ok((layout, memory))
            }
            vmm::vmm_response::Outcome::Error(_)
            | vmm::vmm_response::Outcome::State(_)
            | vmm::vmm_response::Outcome::Done(_)
            | vmm::vmm_response::Outcome::StateSaved(_)
            | vmm::vmm_response::Outcome::Image(_)
            | vmm::vmm_response::Outcome::RecognizedText(_)
            | vmm::vmm_response::Outcome::AgentChannelOpened(_)
            | vmm::vmm_response::Outcome::RecordingStarted(_)
            | vmm::vmm_response::Outcome::EncodedDisplayOpened(_)
            | vmm::vmm_response::Outcome::RecordingStopped(_) => {
                Err(VmmError::Protocol("expected DisplayOpened"))
            }
        }
    }

    /// Opens the display as an H.264 stream: its size. `EncodedFrame`
    /// events then arrive on this connection, starting with a key frame.
    ///
    /// # Errors
    /// See [`VmmError`].
    pub fn open_encoded_display(
        &mut self,
        by: Deadline,
    ) -> Result<vmm::EncodedDisplayOpened, VmmError> {
        match self.call_outcome(
            vmm::vmm_request::Operation::OpenEncodedDisplay(vmm::OpenEncodedDisplay {}),
            by,
        )? {
            vmm::vmm_response::Outcome::EncodedDisplayOpened(opened) => Ok(opened),
            vmm::vmm_response::Outcome::Error(_)
            | vmm::vmm_response::Outcome::State(_)
            | vmm::vmm_response::Outcome::Done(_)
            | vmm::vmm_response::Outcome::StateSaved(_)
            | vmm::vmm_response::Outcome::Image(_)
            | vmm::vmm_response::Outcome::RecognizedText(_)
            | vmm::vmm_response::Outcome::AgentChannelOpened(_)
            | vmm::vmm_response::Outcome::DisplayOpened(_)
            | vmm::vmm_response::Outcome::RecordingStarted(_)
            | vmm::vmm_response::Outcome::RecordingStopped(_) => {
                Err(VmmError::Protocol("expected EncodedDisplayOpened"))
            }
        }
    }

    /// Connects to a vsock port in the guest. The VMM hands the connected
    /// socket over; from here on, traffic doesn't pass through the VMM.
    ///
    /// # Errors
    /// Nothing listens on the port, or no descriptor arrived.
    pub fn open_agent_channel(
        &mut self,
        vsock_port: u32,
        by: Deadline,
    ) -> Result<OwnedFd, VmmError> {
        let (outcome, attached) = self.call(
            vmm::vmm_request::Operation::OpenAgentChannel(vmm::OpenAgentChannel { vsock_port }),
            by,
        )?;
        match outcome {
            vmm::vmm_response::Outcome::AgentChannelOpened(_) => attached.into_iter().next().ok_or(
                VmmError::Protocol("AgentChannelOpened without a file descriptor"),
            ),
            vmm::vmm_response::Outcome::Error(_)
            | vmm::vmm_response::Outcome::State(_)
            | vmm::vmm_response::Outcome::Done(_)
            | vmm::vmm_response::Outcome::StateSaved(_)
            | vmm::vmm_response::Outcome::Image(_)
            | vmm::vmm_response::Outcome::RecognizedText(_)
            | vmm::vmm_response::Outcome::DisplayOpened(_)
            | vmm::vmm_response::Outcome::EncodedDisplayOpened(_)
            | vmm::vmm_response::Outcome::RecordingStarted(_)
            | vmm::vmm_response::Outcome::RecordingStopped(_) => {
                Err(VmmError::Protocol("expected AgentChannelOpened"))
            }
        }
    }
}
