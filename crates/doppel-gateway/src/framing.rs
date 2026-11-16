//! The daemon's stream framing, `[u32 little-endian length][message]`, to
//! and from one message per WebSocket message. Pure: bytes in, messages
//! out.

/// The largest message either way: the daemon's own limit, so a full-size
/// screenshot fits.
pub(crate) const MAX_MESSAGE: usize = 64 << 20;

const PREFIX: usize = 4;

/// A message longer than [`MAX_MESSAGE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a {0}-byte message is over the {MAX_MESSAGE}-byte limit")]
pub(crate) struct TooLong(pub(crate) usize);

/// Messages split out of a byte stream, however the bytes arrive.
#[derive(Debug, Default)]
pub(crate) struct Splitter {
    pending: Vec<u8>,
}

impl Splitter {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    /// The next whole message, if its bytes have all arrived.
    pub(crate) fn next_message(&mut self) -> Result<Option<Vec<u8>>, TooLong> {
        let Some(prefix) = self.pending.first_chunk::<PREFIX>() else {
            return Ok(None);
        };
        let length =
            usize::try_from(u32::from_le_bytes(*prefix)).map_err(|_| TooLong(usize::MAX))?;
        if length > MAX_MESSAGE {
            return Err(TooLong(length));
        }
        if self.pending.len() < PREFIX + length {
            return Ok(None);
        }
        let message = self.pending[PREFIX..PREFIX + length].to_vec();
        self.pending.drain(..PREFIX + length);
        Ok(Some(message))
    }
}

/// A message as the daemon's stream carries it.
pub(crate) fn framed(message: &[u8]) -> Result<Vec<u8>, TooLong> {
    let length = u32::try_from(message.len())
        .ok()
        .filter(|_| message.len() <= MAX_MESSAGE)
        .ok_or(TooLong(message.len()))?;
    let mut bytes = Vec::with_capacity(PREFIX + message.len());
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(message);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn split_all(splitter: &mut Splitter) -> Vec<Vec<u8>> {
        std::iter::from_fn(|| splitter.next_message().unwrap()).collect()
    }

    proptest! {
        /// However the stream is cut, the same messages come out.
        #[test]
        fn messages_come_out_whole_however_the_bytes_arrive(
            messages in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..300), 0..8),
            cuts in prop::collection::vec(any::<prop::sample::Index>(), 0..12),
        ) {
            let stream: Vec<u8> = messages.iter().flat_map(|m| framed(m).unwrap()).collect();
            let mut at: Vec<usize> = cuts.iter().map(|cut| cut.index(stream.len() + 1)).collect();
            at.push(0);
            at.push(stream.len());
            at.sort_unstable();
            let mut splitter = Splitter::default();
            let mut out = Vec::new();
            for pair in at.windows(2) {
                splitter.push(&stream[pair[0]..pair[1]]);
                out.extend(split_all(&mut splitter));
            }
            prop_assert_eq!(out, messages);
        }
    }

    #[test]
    fn a_message_over_the_limit_is_refused_before_it_arrives() {
        let mut splitter = Splitter::default();
        let length = u32::try_from(MAX_MESSAGE + 1).unwrap();
        splitter.push(&length.to_le_bytes());
        assert_eq!(splitter.next_message(), Err(TooLong(MAX_MESSAGE + 1)));
        assert_eq!(
            framed(&vec![0; MAX_MESSAGE + 1]),
            Err(TooLong(MAX_MESSAGE + 1))
        );
    }
}
