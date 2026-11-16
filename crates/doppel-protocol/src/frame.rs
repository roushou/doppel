//! Length-prefixed framing: `[u32 little-endian payload length][payload]`.
//!
//! Sans-IO: callers push whatever bytes their transport produced, in chunks of
//! any size, and pull complete frames out. Output is independent of how the
//! input was split.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use prost::Message;

use crate::pb::{agent::v1::AgentFrame, daemon::v1::DaemonFrame, vmm::v1::VmmFrame};

const LENGTH_PREFIX: usize = 4;

/// Largest accepted payload. Bounds memory a peer can make us allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxFrameLen(u32);

impl MaxFrameLen {
    /// 64 MiB: a full-resolution PNG screenshot fits with headroom.
    pub const DEFAULT: Self = Self(64 << 20);

    #[must_use]
    pub const fn new(bytes: u32) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn bytes(self) -> u32 {
        self.0
    }
}

/// The undecoded payload of one frame (length prefix excluded). Kept so that
/// nothing a peer sends is silently dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame(Bytes);

impl RawFrame {
    #[must_use]
    pub fn payload(&self) -> &Bytes {
        &self.0
    }
}

/// A top-level message type that can tell whether it understood its payload.
///
/// prost skips unknown fields, so a `oneof` variant added by a newer peer
/// decodes to an envelope with no variant set. That case must surface as
/// [`Received::Unrecognized`] rather than look like an empty message.
pub trait Envelope: Message + Default {
    fn is_recognized(&self) -> bool;
}

impl Envelope for AgentFrame {
    fn is_recognized(&self) -> bool {
        self.kind.is_some()
    }
}

impl Envelope for VmmFrame {
    fn is_recognized(&self) -> bool {
        self.kind.is_some()
    }
}

impl Envelope for DaemonFrame {
    fn is_recognized(&self) -> bool {
        self.kind.is_some()
    }
}

/// One complete frame. Every variant keeps the raw payload.
#[derive(Debug, Clone, PartialEq)]
pub enum Received<M> {
    Recognized {
        message: M,
        raw: RawFrame,
    },
    /// Well-formed protobuf, but of a kind this build doesn't know.
    Unrecognized {
        raw: RawFrame,
    },
    /// The frame boundary was intact but the payload isn't valid protobuf.
    /// The stream stays usable: the next frame decodes normally.
    Malformed {
        raw: RawFrame,
        error: prost::DecodeError,
    },
}

/// Errors after which the byte stream can't be resynchronised.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    #[error("frame of {len} bytes exceeds the {limit}-byte limit")]
    FrameTooLarge { len: u32, limit: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error("message of {len} bytes exceeds the {limit}-byte frame limit")]
    TooLarge { len: usize, limit: u32 },
}

#[derive(Debug)]
enum DecoderState {
    Reading,
    /// Terminal: a fatal error was returned, and is returned again forever.
    Failed(StreamError),
}

/// Incremental decoder for a stream of `M` frames.
#[derive(Debug)]
pub struct FrameDecoder<M> {
    buffer: BytesMut,
    /// Stream bytes taken into frames returned so far, length prefixes
    /// included: where the next frame starts.
    consumed: u64,
    limit: MaxFrameLen,
    state: DecoderState,
    message: std::marker::PhantomData<fn() -> M>,
}

impl<M: Envelope> FrameDecoder<M> {
    #[must_use]
    pub fn new(limit: MaxFrameLen) -> Self {
        Self {
            buffer: BytesMut::new(),
            consumed: 0,
            limit,
            state: DecoderState::Reading,
            message: std::marker::PhantomData,
        }
    }

    /// Appends bytes read from the transport.
    pub fn push(&mut self, bytes: &[u8]) {
        match self.state {
            DecoderState::Reading => self.buffer.extend_from_slice(bytes),
            // Nothing after a fatal error can be framed; don't grow the buffer.
            DecoderState::Failed(_) => {}
        }
    }

    /// Bytes received but not yet part of a complete frame.
    #[must_use]
    pub fn pending_bytes(&self) -> usize {
        self.buffer.len()
    }

    /// The stream offset where the next frame starts: every byte before it
    /// belongs to a frame already returned.
    #[must_use]
    pub const fn consumed(&self) -> u64 {
        self.consumed
    }

    /// Returns the next complete frame, `Ok(None)` if more bytes are needed,
    /// or a [`StreamError`] once the stream is unrecoverable.
    ///
    /// # Errors
    /// [`StreamError::FrameTooLarge`] when a length prefix exceeds the limit.
    /// The decoder then stays failed and returns the same error on every call.
    pub fn next_frame(&mut self) -> Result<Option<Received<M>>, StreamError> {
        match &self.state {
            DecoderState::Failed(error) => return Err(error.clone()),
            DecoderState::Reading => {}
        }
        let Some(prefix) = self.buffer.get(..LENGTH_PREFIX) else {
            return Ok(None);
        };
        let len = u32::from_le_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]);
        if len > self.limit.bytes() {
            let error = StreamError::FrameTooLarge {
                len,
                limit: self.limit.bytes(),
            };
            self.state = DecoderState::Failed(error.clone());
            self.buffer = BytesMut::new();
            return Err(error);
        }
        let total = LENGTH_PREFIX + len as usize;
        if self.buffer.len() < total {
            self.buffer.reserve(total - self.buffer.len());
            return Ok(None);
        }
        self.buffer.advance(LENGTH_PREFIX);
        self.consumed += total as u64;
        let raw = RawFrame(self.buffer.split_to(len as usize).freeze());
        Ok(Some(match M::decode(raw.0.clone()) {
            Ok(message) if message.is_recognized() => Received::Recognized { message, raw },
            Ok(_) => Received::Unrecognized { raw },
            Err(error) => Received::Malformed { raw, error },
        }))
    }
}

/// Encodes messages into frames.
#[derive(Debug, Clone, Copy)]
pub struct FrameEncoder {
    limit: MaxFrameLen,
}

impl FrameEncoder {
    #[must_use]
    pub const fn new(limit: MaxFrameLen) -> Self {
        Self { limit }
    }

    /// # Errors
    /// [`EncodeError::TooLarge`] if the encoded message exceeds the frame limit,
    /// which the peer's decoder would treat as fatal.
    pub fn encode<M: Message>(&self, message: &M) -> Result<Bytes, EncodeError> {
        let mut out = BytesMut::new();
        self.encode_into(message, &mut out)?;
        Ok(out.freeze())
    }

    /// Appends the message's frame to `out`, which a writer reuses, so
    /// framing allocates only when `out` must grow.
    ///
    /// # Errors
    /// As [`Self::encode`]; `out` is then unchanged.
    pub fn encode_into<M: Message>(
        &self,
        message: &M,
        out: &mut BytesMut,
    ) -> Result<(), EncodeError> {
        let len = message.encoded_len();
        let limit = self.limit.bytes();
        let Some(prefix) = u32::try_from(len).ok().filter(|n| *n <= limit) else {
            return Err(EncodeError::TooLarge { len, limit });
        };
        out.reserve(LENGTH_PREFIX + len);
        out.put_u32_le(prefix);
        message.encode_raw(out);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::agent::v1::{AgentRequest, agent_frame, agent_request};
    use crate::pb::common::v1::{Input, TypeText, input};
    use proptest::prelude::*;

    fn type_text(id: u64, text: &str) -> AgentFrame {
        AgentFrame {
            kind: Some(agent_frame::Kind::Request(AgentRequest {
                id,
                operation: Some(agent_request::Operation::Input(Input {
                    action: Some(input::Action::TypeText(TypeText {
                        text: text.to_owned(),
                    })),
                })),
            })),
        }
    }

    fn decode_all(decoder: &mut FrameDecoder<AgentFrame>) -> Vec<Received<AgentFrame>> {
        let mut out = Vec::new();
        while let Some(frame) = decoder.next_frame().unwrap() {
            out.push(frame);
        }
        out
    }

    fn messages(frames: Vec<Received<AgentFrame>>) -> Vec<AgentFrame> {
        frames
            .into_iter()
            .map(|f| match f {
                Received::Recognized { message, .. } => message,
                other @ (Received::Unrecognized { .. } | Received::Malformed { .. }) => {
                    panic!("unexpected {other:?}")
                }
            })
            .collect()
    }

    #[test]
    fn round_trips_one_frame() {
        let encoder = FrameEncoder::new(MaxFrameLen::DEFAULT);
        let frame = type_text(7, "héllo 👋");
        let mut decoder = FrameDecoder::new(MaxFrameLen::DEFAULT);
        decoder.push(&encoder.encode(&frame).unwrap());
        assert_eq!(messages(decode_all(&mut decoder)), vec![frame]);
        assert_eq!(decoder.pending_bytes(), 0);
    }

    #[test]
    fn waits_for_incomplete_prefix_and_payload() {
        let bytes = FrameEncoder::new(MaxFrameLen::DEFAULT)
            .encode(&type_text(1, "abc"))
            .unwrap();
        let mut decoder = FrameDecoder::<AgentFrame>::new(MaxFrameLen::DEFAULT);
        decoder.push(&bytes[..2]);
        assert_eq!(decoder.next_frame().unwrap(), None);
        decoder.push(&bytes[2..bytes.len() - 1]);
        assert_eq!(decoder.next_frame().unwrap(), None);
        decoder.push(&bytes[bytes.len() - 1..]);
        assert!(matches!(
            decoder.next_frame().unwrap(),
            Some(Received::Recognized { .. })
        ));
    }

    #[test]
    fn oversized_frame_is_fatal_and_stays_fatal() {
        let mut decoder = FrameDecoder::<AgentFrame>::new(MaxFrameLen::new(16));
        decoder.push(&17u32.to_le_bytes());
        let expected = StreamError::FrameTooLarge { len: 17, limit: 16 };
        assert_eq!(decoder.next_frame(), Err(expected.clone()));
        decoder.push(
            &FrameEncoder::new(MaxFrameLen::DEFAULT)
                .encode(&type_text(1, "x"))
                .unwrap(),
        );
        assert_eq!(decoder.next_frame(), Err(expected));
        assert_eq!(decoder.pending_bytes(), 0);
    }

    #[test]
    fn encoder_refuses_what_the_peer_would_reject() {
        let encoder = FrameEncoder::new(MaxFrameLen::new(8));
        assert!(matches!(
            encoder.encode(&type_text(1, "longer than eight bytes")),
            Err(EncodeError::TooLarge { .. })
        ));
    }

    #[test]
    fn unknown_variant_from_newer_peer_is_kept_not_dropped() {
        // Field 99 of AgentFrame: a oneof variant this build doesn't know.
        // Tag = (99 << 3) | wire type 2 (length-delimited), as a varint.
        let mut payload = BytesMut::new();
        prost::encoding::encode_varint((99 << 3) | 2, &mut payload);
        prost::encoding::encode_varint(3, &mut payload);
        payload.extend_from_slice(b"new");
        let mut bytes = BytesMut::new();
        bytes.put_u32_le(u32::try_from(payload.len()).unwrap());
        bytes.extend_from_slice(&payload);

        let mut decoder = FrameDecoder::<AgentFrame>::new(MaxFrameLen::DEFAULT);
        decoder.push(&bytes);
        match decoder.next_frame().unwrap() {
            Some(Received::Unrecognized { raw }) => {
                assert_eq!(raw.payload().as_ref(), payload.as_ref())
            }
            other => panic!("expected Unrecognized, got {other:?}"),
        }
    }

    #[test]
    fn malformed_payload_does_not_poison_the_stream() {
        let encoder = FrameEncoder::new(MaxFrameLen::DEFAULT);
        let mut bytes = BytesMut::new();
        bytes.put_u32_le(2);
        bytes.extend_from_slice(&[0xff, 0xff]); // truncated varint tag
        bytes.extend_from_slice(&encoder.encode(&type_text(2, "after")).unwrap());

        let mut decoder = FrameDecoder::<AgentFrame>::new(MaxFrameLen::DEFAULT);
        decoder.push(&bytes);
        let frames = decode_all(&mut decoder);
        assert!(matches!(frames[0], Received::Malformed { .. }));
        assert_eq!(messages(frames[1..].to_vec()), vec![type_text(2, "after")]);
    }

    proptest! {
        /// Design principle §8: identical output however the input is split.
        #[test]
        fn output_is_independent_of_chunking(
            texts in prop::collection::vec(".{0,64}", 1..12),
            cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..24),
        ) {
            let encoder = FrameEncoder::new(MaxFrameLen::DEFAULT);
            let frames: Vec<_> = texts.iter().enumerate().map(|(i, t)| type_text(i as u64, t)).collect();
            let stream: Vec<u8> = frames.iter().flat_map(|f| encoder.encode(f).unwrap().to_vec()).collect();

            let mut positions: Vec<usize> = cuts.iter().map(|c| c.index(stream.len() + 1)).collect();
            positions.push(0);
            positions.push(stream.len());
            positions.sort_unstable();

            let mut decoder = FrameDecoder::new(MaxFrameLen::DEFAULT);
            let mut decoded = Vec::new();
            for window in positions.windows(2) {
                decoder.push(&stream[window[0]..window[1]]);
                decoded.extend(decode_all(&mut decoder));
            }
            prop_assert_eq!(messages(decoded), frames);
            prop_assert_eq!(decoder.pending_bytes(), 0);
        }
    }
}
