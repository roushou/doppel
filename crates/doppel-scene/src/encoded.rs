//! H.264 frames as a screen's source hands them over.

/// One H.264 frame, in AVCC: each NAL unit prefixed by its length (4 bytes,
/// big endian). It needs every frame since the last key frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    /// Increases by one from frame to frame. A gap means frames were lost:
    /// the screen then waits for the next key frame.
    pub sequence: u64,
    pub kind: FrameKind,
    pub data: Vec<u8>,
}

/// Whether a frame starts afresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameKind {
    /// Decodable alone, with the stream's parameter sets (SPS, then PPS,
    /// without length prefixes).
    Key { parameter_sets: Vec<Vec<u8>> },
    /// Decodable after the frames before it.
    Delta,
}

/// What a browser's decoder (WebCodecs) needs to decode a stream: its codec
/// string and its `avcC` record, both made from a key frame's SPS and PPS.
/// The frames themselves are AVCC already, as the record says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecoderConfig {
    /// `avc1.PPCCLL`: profile, constraint flags and level, from the SPS.
    pub codec: String,
    /// An `AVCDecoderConfigurationRecord` for one SPS and one PPS, with
    /// 4-byte NAL lengths.
    pub description: Vec<u8>,
}

impl DecoderConfig {
    /// The configuration for a key frame's parameter sets (SPS, then PPS);
    /// `None` without both, or with one too long for the record.
    #[must_use]
    pub fn of(parameter_sets: &[Vec<u8>]) -> Option<Self> {
        let [sps, pps, ..] = parameter_sets else {
            return None;
        };
        let profile = |at: usize| sps.get(at).copied().unwrap_or(0);
        let sps_len = u16::try_from(sps.len()).ok()?;
        let pps_len = u16::try_from(pps.len()).ok()?;
        let mut description = Vec::with_capacity(11 + sps.len() + pps.len());
        description.extend_from_slice(&[1, profile(1), profile(2), profile(3), 0xff, 0xe1]);
        description.extend_from_slice(&sps_len.to_be_bytes());
        description.extend_from_slice(sps);
        description.push(1);
        description.extend_from_slice(&pps_len.to_be_bytes());
        description.extend_from_slice(pps);
        Some(Self {
            codec: format!(
                "avc1.{:02x}{:02x}{:02x}",
                profile(1),
                profile(2),
                profile(3)
            ),
            description,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_names_the_profile_and_holds_both_sets() {
        let sps = vec![0x67, 0x64, 0x00, 0x1f, 0xac];
        let pps = vec![0x68, 0xee];
        let config = DecoderConfig::of(&[sps.clone(), pps.clone()]).unwrap();
        assert_eq!(config.codec, "avc1.64001f");
        assert_eq!(
            config.description,
            [
                vec![1, 0x64, 0x00, 0x1f, 0xff, 0xe1, 0, 5],
                sps,
                vec![1, 0, 2],
                pps
            ]
            .concat()
        );
        assert_eq!(DecoderConfig::of(&[vec![0x67]]), None);
    }
}
