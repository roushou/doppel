//! A machine's display, on a connection of its own: the daemon answers
//! `OpenDisplay` with the layout and the shared memory holding the frames,
//! then announces each frame. Mapping the memory and reading the frames is
//! the caller's: this crate does no shared memory. Across a network, where
//! memory can't be shared, `OpenEncodedDisplay` sends the frames themselves,
//! as H.264; decoding them is the caller's too.

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

use doppel_machine::deadline::Deadline;
use doppel_machine::geometry::Size;
use doppel_machine::machine::MachineName;
use doppel_protocol::convert::RemoteError;
use doppel_protocol::exchange::{DaemonCall, ExchangeConfig, Unrecognized};
use doppel_protocol::pb::daemon::v1 as daemon;
use doppel_protocol::pb::daemon::v1::daemon_response::Outcome;
use doppel_protocol::pb::vmm::v1 as vmm;
use doppel_protocol::rpc::Rpc;
use doppel_protocol::transport::Channel;

use crate::{ClientError, Clock, MachineFailure};

/// Where frames are in the display's shared memory, as the VMM lays them
/// out (see `DisplayLayout` in vmm.proto).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayLayout {
    pub width_pixels: u32,
    pub height_pixels: u32,
    pub slot_count: u32,
    pub slot_stride: u64,
    pub luma_offset: u64,
    pub luma_bytes_per_row: u32,
    pub chroma_offset: u64,
    pub chroma_bytes_per_row: u32,
    /// The display's size in points, the coordinate space of input, when
    /// the daemon said.
    pub points: Option<Size>,
}

/// A frame ready in its slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Announced {
    pub slot: u32,
    pub sequence: u64,
}

/// An open display: its announcements, until the machine stops.
#[derive(Debug)]
pub struct Display<C> {
    channel: Stream<C>,
}

impl<C: Clock> Display<C> {
    /// Connects to the daemon on `socket` and opens `name`'s display:
    /// returns it, the layout, and the shared memory to map.
    ///
    /// # Errors
    /// See [`ClientError`]: the daemon refuses a machine that isn't running.
    pub fn open(
        socket: &Path,
        clock: C,
        name: &MachineName,
        by: Deadline,
    ) -> Result<(Self, DisplayLayout, OwnedFd), ClientError> {
        let stream = UnixStream::connect(socket).map_err(ClientError::from_connect)?;
        Self::over(stream, clock, name, by)
    }

    /// As [`Display::open`], on a connection already made, such as one end
    /// of a socket pair.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn over(
        stream: UnixStream,
        clock: C,
        name: &MachineName,
        by: Deadline,
    ) -> Result<(Self, DisplayLayout, OwnedFd), ClientError> {
        let (channel, layout, attached) = Stream::open(
            stream,
            clock,
            daemon::OpenDisplay {
                name: name.to_string(),
            },
            by,
        )?;
        // The descriptor comes with the response.
        let memory = attached
            .into_iter()
            .next()
            .ok_or(ClientError::Protocol("the display's memory didn't come"))?;
        let points = layout
            .display_points
            .and_then(|size| Size::try_from(size).ok());
        let layout = DisplayLayout {
            width_pixels: layout.width_pixels,
            height_pixels: layout.height_pixels,
            slot_count: layout.slot_count,
            slot_stride: layout.slot_stride,
            luma_offset: layout.luma_offset,
            luma_bytes_per_row: layout.luma_bytes_per_row,
            chroma_offset: layout.chroma_offset,
            chroma_bytes_per_row: layout.chroma_bytes_per_row,
            points,
        };
        Ok((Self { channel }, layout, memory))
    }

    /// The next frame announced.
    ///
    /// # Errors
    /// [`ClientError::Timeout`] when none comes by `by` (the screen didn't
    /// change); [`ClientError::Closed`] once the machine stops.
    pub fn next_frame(&mut self, by: Deadline) -> Result<Announced, ClientError> {
        loop {
            if let Some(daemon::daemon_event::Kind::DisplayFrame(frame)) = self.channel.event(by)? {
                return Ok(Announced {
                    slot: frame.slot,
                    sequence: frame.sequence,
                });
            }
        }
    }
}

/// The size of an encoded display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EncodedSize {
    pub width_pixels: u32,
    pub height_pixels: u32,
    /// The display's size in points, the coordinate space of input, when
    /// the daemon said.
    pub points: Option<Size>,
}

/// One H.264 frame, in AVCC: each NAL unit prefixed by its length (4 bytes,
/// big endian). It needs every frame since the last key frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    /// Increases from frame to frame; a gap means frames were skipped,
    /// which happens only before a key frame.
    pub sequence: u64,
    pub kind: FrameKind,
    pub data: Vec<u8>,
}

/// Whether a frame starts afresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameKind {
    /// Decodable alone, with its parameter sets: the SPS, then the PPS,
    /// each without a length prefix.
    Key { parameter_sets: Vec<Vec<u8>> },
    /// Decodable after the frames before it.
    Delta,
}

/// An open encoded display: its frames, until the machine stops.
#[derive(Debug)]
pub struct EncodedDisplay<C> {
    channel: Stream<C>,
}

impl<C: Clock> EncodedDisplay<C> {
    /// Connects to the daemon on `socket` (which may be forwarded from
    /// another host) and opens `name`'s display as a stream.
    ///
    /// # Errors
    /// See [`ClientError`]: the daemon refuses a machine that isn't running.
    pub fn open(
        socket: &Path,
        clock: C,
        name: &MachineName,
        by: Deadline,
    ) -> Result<(Self, EncodedSize), ClientError> {
        let stream = UnixStream::connect(socket).map_err(ClientError::from_connect)?;
        Self::over(stream, clock, name, by)
    }

    /// As [`EncodedDisplay::open`], on a connection already made.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub fn over(
        stream: UnixStream,
        clock: C,
        name: &MachineName,
        by: Deadline,
    ) -> Result<(Self, EncodedSize), ClientError> {
        let (channel, opened, _) = Stream::open(
            stream,
            clock,
            daemon::OpenEncodedDisplay {
                name: name.to_string(),
            },
            by,
        )?;
        let points = opened
            .display_points
            .and_then(|size| Size::try_from(size).ok());
        let size = EncodedSize {
            width_pixels: opened.width_pixels,
            height_pixels: opened.height_pixels,
            points,
        };
        Ok((Self { channel }, size))
    }

    /// The next frame.
    ///
    /// # Errors
    /// [`ClientError::Timeout`] when none comes by `by` (the screen didn't
    /// change); [`ClientError::Closed`] once the machine stops;
    /// [`ClientError::Protocol`] for a key frame without parameter sets.
    pub fn next_frame(&mut self, by: Deadline) -> Result<EncodedFrame, ClientError> {
        loop {
            if let Some(daemon::daemon_event::Kind::EncodedFrame(frame)) = self.channel.event(by)? {
                return EncodedFrame::from_wire(frame);
            }
        }
    }
}

impl EncodedFrame {
    fn from_wire(frame: vmm::EncodedFrame) -> Result<Self, ClientError> {
        let kind = if frame.key {
            if frame.parameter_sets.is_empty() {
                return Err(ClientError::Protocol(
                    "a key frame without its parameter sets",
                ));
            }
            FrameKind::Key {
                parameter_sets: frame.parameter_sets.into_iter().map(Vec::from).collect(),
            }
        } else {
            FrameKind::Delta
        };
        Ok(Self {
            sequence: frame.sequence,
            kind,
            data: Vec::from(frame.data),
        })
    }
}

/// A connection to the daemon that carries one streaming request: greeted,
/// then the request's response, with the descriptors sent with it, and its
/// events. Kinds a newer daemon adds are passed over.
#[derive(Debug)]
pub(crate) struct Stream<C> {
    channel: Channel<daemon::DaemonFrame, C>,
}

impl<C: Clock> Stream<C> {
    /// Greets the daemon on `stream` and sends `request`: the stream, its
    /// answer and the descriptors that came with it.
    pub(crate) fn open<R: Rpc>(
        stream: UnixStream,
        clock: C,
        request: R,
        by: Deadline,
    ) -> Result<(Self, R::Answer, Vec<OwnedFd>), ClientError> {
        let config = ExchangeConfig {
            unrecognized: Unrecognized::Skip,
            ..ExchangeConfig::STRICT
        };
        let mut channel = Channel::<daemon::DaemonFrame, C>::open(stream, clock, config, by)?;
        let call = DaemonCall {
            caption: String::new(),
            operation: request.into_operation(),
        };
        let answer = channel.call(call, by)?;
        let answered = match answer.response.outcome {
            Some(Outcome::Error(error)) => return Err(RemoteError::from(error).into()),
            Some(Outcome::MachineFailure(failure)) => {
                return Err(MachineFailure::from_wire(failure).into());
            }
            Some(outcome) => R::answered(outcome).ok_or(ClientError::WrongAnswer(R::NAME))?,
            None => return Err(ClientError::Protocol("response without outcome")),
        };
        Ok((Self { channel }, answered, answer.attached))
    }

    /// The next event's content.
    ///
    /// # Errors
    /// [`ClientError::Timeout`] when none comes by `by`.
    pub(crate) fn event(
        &mut self,
        by: Deadline,
    ) -> Result<Option<daemon::daemon_event::Kind>, ClientError> {
        match self.channel.next_event(by)? {
            Some(event) => Ok(event.kind),
            None => Err(ClientError::Timeout),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_frame_carries_its_parameter_sets_and_a_delta_none() {
        let wire = |key, parameter_sets: Vec<Vec<u8>>| vmm::EncodedFrame {
            sequence: 4,
            key,
            parameter_sets: parameter_sets.into_iter().map(Into::into).collect(),
            data: vec![0, 0, 0, 1, 0x65].into(),
        };
        assert!(matches!(
            EncodedFrame::from_wire(wire(true, Vec::new())),
            Err(ClientError::Protocol(_))
        ));
        assert_eq!(
            EncodedFrame::from_wire(wire(true, vec![vec![0x67], vec![0x68]])).ok(),
            Some(EncodedFrame {
                sequence: 4,
                kind: FrameKind::Key {
                    parameter_sets: vec![vec![0x67], vec![0x68]],
                },
                data: vec![0, 0, 0, 1, 0x65],
            })
        );
        assert_eq!(
            EncodedFrame::from_wire(wire(false, vec![vec![0x67]]))
                .ok()
                .map(|frame| frame.kind),
            Some(FrameKind::Delta),
            "a delta's stray parameter sets are dropped"
        );
    }
}
