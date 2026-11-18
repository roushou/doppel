use doppel_scene::encoded::{EncodedFrame, FrameKind};

use super::ParseError;
use crate::pb::vmm::v1 as vmm;

/// A screen's H.264 frame as the daemon streams it.
impl TryFrom<vmm::EncodedFrame> for EncodedFrame {
    type Error = ParseError;

    fn try_from(frame: vmm::EncodedFrame) -> Result<Self, ParseError> {
        let kind = if frame.key {
            if frame.parameter_sets.is_empty() {
                return Err(ParseError::Missing("encoded_frame.parameter_sets"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_keep_their_kind_and_bytes_and_a_bare_key_frame_is_refused() {
        let key = EncodedFrame::try_from(vmm::EncodedFrame {
            sequence: 7,
            key: true,
            parameter_sets: vec![vec![0x67].into(), vec![0x68].into()],
            data: vec![0, 0, 0, 1, 0x65].into(),
        })
        .unwrap();
        assert_eq!(
            key,
            EncodedFrame {
                sequence: 7,
                kind: FrameKind::Key {
                    parameter_sets: vec![vec![0x67], vec![0x68]],
                },
                data: vec![0, 0, 0, 1, 0x65],
            }
        );
        assert!(
            EncodedFrame::try_from(vmm::EncodedFrame {
                key: true,
                ..vmm::EncodedFrame::default()
            })
            .is_err()
        );
    }
}
