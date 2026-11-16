//! Which encoded frames can be decoded: from a key frame on, while none is
//! missing. Pure: the reader asks it about each frame before decoding.

use crate::encoded::{EncodedFrame, FrameKind};

/// Where a stream stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resync {
    /// Nothing decodable until a key frame: at the start, after a gap, and
    /// after a frame failed to decode.
    AwaitingKey,
    /// Decoding; `sequence` was the last frame decoded.
    Following { sequence: u64 },
}

/// What to do with a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step<'f> {
    /// A key frame: decode it, with a decoder for these parameter sets.
    Restart(&'f [Vec<u8>]),
    /// The next delta: decode it.
    Decode,
    /// Not decodable: drop it.
    Skip,
}

impl Resync {
    pub const START: Self = Self::AwaitingKey;

    #[must_use]
    pub fn next(self, frame: &EncodedFrame) -> (Self, Step<'_>) {
        let following = Self::Following {
            sequence: frame.sequence,
        };
        match (&frame.kind, self) {
            (FrameKind::Key { parameter_sets }, Self::AwaitingKey | Self::Following { .. }) => {
                (following, Step::Restart(parameter_sets))
            }
            (FrameKind::Delta, Self::Following { sequence })
                if sequence.checked_add(1) == Some(frame.sequence) =>
            {
                (following, Step::Decode)
            }
            (FrameKind::Delta, Self::AwaitingKey | Self::Following { .. }) => {
                (Self::AwaitingKey, Step::Skip)
            }
        }
    }

    /// The frame just given failed to decode: later ones can't be trusted.
    #[must_use]
    pub const fn failed(self) -> Self {
        Self::AwaitingKey
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETS: &[Vec<u8>] = &[];

    fn key(sequence: u64) -> EncodedFrame {
        EncodedFrame {
            sequence,
            kind: FrameKind::Key {
                parameter_sets: SETS.to_vec(),
            },
            data: Vec::new(),
        }
    }

    fn delta(sequence: u64) -> EncodedFrame {
        EncodedFrame {
            sequence,
            kind: FrameKind::Delta,
            data: Vec::new(),
        }
    }

    fn steps(frames: &[EncodedFrame]) -> Vec<Step<'_>> {
        let mut state = Resync::START;
        frames
            .iter()
            .map(|frame| {
                let (next, step) = state.next(frame);
                state = next;
                step
            })
            .collect()
    }

    #[test]
    fn decoding_starts_at_a_key_frame() {
        assert_eq!(
            steps(&[delta(3), delta(4), key(5), delta(6), delta(7)]),
            [
                Step::Skip,
                Step::Skip,
                Step::Restart(SETS),
                Step::Decode,
                Step::Decode
            ]
        );
    }

    #[test]
    fn a_gap_waits_for_the_next_key_frame() {
        assert_eq!(
            steps(&[key(1), delta(2), delta(4), delta(5), key(9), delta(10)]),
            [
                Step::Restart(SETS),
                Step::Decode,
                Step::Skip,
                Step::Skip,
                Step::Restart(SETS),
                Step::Decode
            ]
        );
    }

    #[test]
    fn after_a_failure_only_a_key_frame_decodes() {
        let (state, _) = Resync::START.next(&key(1));
        let state = state.failed();
        assert_eq!(state.next(&delta(2)), (Resync::AwaitingKey, Step::Skip));
        assert_eq!(
            state.next(&key(3)),
            (Resync::Following { sequence: 3 }, Step::Restart(SETS))
        );
    }

    #[test]
    fn a_restart_carries_the_key_frame_s_parameter_sets() {
        let frame = EncodedFrame {
            sequence: 1,
            kind: FrameKind::Key {
                parameter_sets: vec![vec![0x67, 1], vec![0x68, 2]],
            },
            data: Vec::new(),
        };
        assert_eq!(
            Resync::START.next(&frame).1,
            Step::Restart(&[vec![0x67, 1], vec![0x68, 2]])
        );
    }

    #[test]
    fn the_last_sequence_has_no_successor() {
        let state = Resync::Following { sequence: u64::MAX };
        assert_eq!(state.next(&delta(0)), (Resync::AwaitingKey, Step::Skip));
    }
}
