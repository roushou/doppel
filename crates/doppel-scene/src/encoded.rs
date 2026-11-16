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
