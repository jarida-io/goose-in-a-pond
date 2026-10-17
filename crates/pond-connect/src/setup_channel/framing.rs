//! A message on the wire is a 4-byte big-endian length and then its bytes, split across ATT
//! writes or notifications of at most `ATT_MTU - 3` bytes (`pond-setup-channel.md`, "Framing").
//! Each message starts its own chunk, and no chunk carries bytes of two messages.

/// The largest message either side sends or accepts.
pub const MAX_MESSAGE_LEN: usize = 16 * 1024;
pub const LENGTH_PREFIX_LEN: usize = 4;
/// An ATT write or notification spends three bytes of the MTU on its own header.
pub const ATT_HEADER_LEN: u16 = 3;
/// The ATT_MTU every link starts at, before any exchange.
pub const DEFAULT_ATT_MTU: u16 = 23;
/// The largest ATT_MTU the Core spec allows.
pub const MAX_ATT_MTU: u16 = 517;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("a message of {len} bytes is over the {MAX_MESSAGE_LEN}-byte limit")]
    TooLong { len: usize },
    #[error("a write carried bytes past the end of its message")]
    Overrun,
}

/// The length prefix and the message, ready to split.
pub fn encode(message: &[u8]) -> Result<Vec<u8>, FrameError> {
    if message.len() > MAX_MESSAGE_LEN {
        return Err(FrameError::TooLong { len: message.len() });
    }
    let mut frame = Vec::with_capacity(LENGTH_PREFIX_LEN + message.len());
    // In range: the cap above is far below u32::MAX.
    frame.extend_from_slice(&(message.len() as u32).to_be_bytes());
    frame.extend_from_slice(message);
    Ok(frame)
}

/// Bytes per write or notification at this ATT_MTU, clamped to what the spec allows.
pub fn chunk_len(att_mtu: u16) -> usize {
    usize::from(att_mtu.clamp(DEFAULT_ATT_MTU, MAX_ATT_MTU) - ATT_HEADER_LEN)
}

/// `frame` as the writes or notifications that carry it.
pub fn chunks(frame: &[u8], att_mtu: u16) -> std::slice::Chunks<'_, u8> {
    frame.chunks(chunk_len(att_mtu))
}

/// One message as its own chunks. Sending each message's chunks alone is what keeps a writer
/// from ever coalescing two messages into one write or notification.
pub fn split(message: &[u8], att_mtu: u16) -> Result<Vec<Vec<u8>>, FrameError> {
    Ok(chunks(&encode(message)?, att_mtu)
        .map(<[u8]>::to_vec)
        .collect())
}

/// Rebuilds messages from chunks split anywhere within a message. A chunk belongs to one message,
/// so bytes past its end are a framing error that ends the session.
#[derive(Debug, Default)]
pub struct Reassembler {
    prefix: [u8; LENGTH_PREFIX_LEN],
    prefix_len: usize,
    expected: Option<usize>,
    body: Vec<u8>,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one write; returns the message it completes, if it completes one.
    pub fn push(&mut self, mut write: &[u8]) -> Result<Option<Vec<u8>>, FrameError> {
        while self.expected.is_none() && !write.is_empty() {
            self.prefix[self.prefix_len] = write[0];
            self.prefix_len += 1;
            write = &write[1..];
            if self.prefix_len == LENGTH_PREFIX_LEN {
                let len = u32::from_be_bytes(self.prefix) as usize;
                if len > MAX_MESSAGE_LEN {
                    return Err(FrameError::TooLong { len });
                }
                self.expected = Some(len);
                self.body = Vec::with_capacity(len);
            }
        }
        let Some(expected) = self.expected else {
            return Ok(None);
        };
        let wanted = expected - self.body.len();
        if write.len() > wanted {
            return Err(FrameError::Overrun);
        }
        self.body.extend_from_slice(write);
        if self.body.len() < expected {
            return Ok(None);
        }
        let message = std::mem::take(&mut self.body);
        *self = Self::default();
        Ok(Some(message))
    }

    /// No message is part-way through.
    pub fn is_idle(&self) -> bool {
        self.prefix_len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// splitmix64: the same chunkings on every run, so a failure can be replayed.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }
    }

    fn message(rng: &mut Rng, len: usize) -> Vec<u8> {
        (0..len).map(|_| rng.next() as u8).collect()
    }

    #[test]
    fn a_frame_is_a_big_endian_length_then_the_bytes() {
        assert_eq!(encode(b"{}").unwrap(), [0, 0, 0, 2, b'{', b'}']);
        assert_eq!(encode(&[]).unwrap(), [0, 0, 0, 0]);
        assert!(encode(&vec![0; MAX_MESSAGE_LEN]).is_ok());
        assert_eq!(
            encode(&vec![0; MAX_MESSAGE_LEN + 1]),
            Err(FrameError::TooLong {
                len: MAX_MESSAGE_LEN + 1
            })
        );
    }

    #[test]
    fn chunks_are_the_mtu_less_the_att_header() {
        assert_eq!(chunk_len(23), 20);
        assert_eq!(chunk_len(185), 182);
        assert_eq!(chunk_len(0), 20, "below the minimum reads as the minimum");
        assert_eq!(chunk_len(u16::MAX), 514);
        let frame = encode(&[7u8; 50]).unwrap();
        let sizes: Vec<usize> = chunks(&frame, 23).map(<[u8]>::len).collect();
        assert_eq!(sizes, [20, 20, 14]);
    }

    #[test]
    fn many_random_chunkings_reassemble_exactly() {
        let mut rng = Rng(0x005e_ed0f_9019);
        let mut reassembler = Reassembler::new();
        for round in 0..2_000 {
            let len = match round % 4 {
                0 => rng.below(8),
                1 => rng.below(64),
                2 => rng.below(2_048),
                _ => rng.below(MAX_MESSAGE_LEN + 1),
            };
            let sent = message(&mut rng, len);
            let frame = encode(&sent).unwrap();
            let mut rest = frame.as_slice();
            let mut received = None;
            while !rest.is_empty() {
                let cut = 1 + rng.below(rest.len().min(600));
                let (write, tail) = rest.split_at(cut);
                rest = tail;
                let out = reassembler.push(write).unwrap();
                if rest.is_empty() {
                    received = out;
                } else {
                    assert!(out.is_none(), "round {round}: finished early");
                }
            }
            assert_eq!(received.as_deref(), Some(sent.as_slice()), "round {round}");
            assert!(reassembler.is_idle());
        }
    }

    #[test]
    fn every_mtu_round_trips() {
        let mut rng = Rng(23);
        let sent = message(&mut rng, 3_000);
        let frame = encode(&sent).unwrap();
        for mtu in [23, 24, 27, 185, 247, 512, 517] {
            let mut reassembler = Reassembler::new();
            let mut received = None;
            for chunk in chunks(&frame, mtu) {
                assert!(received.is_none());
                received = reassembler.push(chunk).unwrap();
            }
            assert_eq!(received.unwrap(), sent, "mtu {mtu}");
        }
    }

    #[test]
    fn empty_writes_change_nothing_and_an_empty_message_completes() {
        let mut reassembler = Reassembler::new();
        assert_eq!(reassembler.push(&[]), Ok(None));
        assert_eq!(reassembler.push(&[0, 0]), Ok(None));
        assert_eq!(reassembler.push(&[]), Ok(None));
        assert_eq!(reassembler.push(&[0, 0]), Ok(Some(Vec::new())));
    }

    #[test]
    fn an_oversize_length_is_refused_before_any_body_arrives() {
        let mut reassembler = Reassembler::new();
        let len = (MAX_MESSAGE_LEN as u32 + 1).to_be_bytes();
        assert_eq!(reassembler.push(&len[..3]), Ok(None));
        assert_eq!(
            reassembler.push(&len[3..]),
            Err(FrameError::TooLong {
                len: MAX_MESSAGE_LEN + 1
            })
        );
        assert_eq!(
            Reassembler::new().push(&u32::MAX.to_be_bytes()),
            Err(FrameError::TooLong {
                len: u32::MAX as usize
            })
        );
    }

    #[test]
    fn a_writer_starts_every_message_in_a_fresh_chunk() {
        let messages: [&[u8]; 3] = [&[1u8; 36], b"{}", &[2u8; 57]];
        let mut reassembler = Reassembler::new();
        for message in messages {
            let chunks = split(message, DEFAULT_ATT_MTU).unwrap();
            assert_eq!(
                chunks[0][..LENGTH_PREFIX_LEN],
                (message.len() as u32).to_be_bytes(),
                "a message's first chunk opens with its own length"
            );
            assert_eq!(chunks.concat(), encode(message).unwrap());
            let (last, rest) = chunks.split_last().unwrap();
            for chunk in rest {
                assert_eq!(reassembler.push(chunk), Ok(None));
            }
            assert_eq!(
                reassembler.push(last).unwrap().as_deref(),
                Some(message),
                "a message ends exactly where its last chunk does"
            );
        }
        assert_eq!(
            split(&[0u8; 36], DEFAULT_ATT_MTU)
                .unwrap()
                .last()
                .unwrap()
                .len(),
            20,
            "a 40-byte frame fills its last chunk, and the next message still starts a new one"
        );
    }

    #[test]
    fn a_chunk_carrying_bytes_of_two_messages_is_refused() {
        let mut first = encode(b"{\"id\":1}").unwrap();
        let second = encode(b"{\"id\":2}").unwrap();
        let mut reassembler = Reassembler::new();
        assert_eq!(reassembler.push(&first[..6]), Ok(None));
        first.truncate(9);
        first.extend_from_slice(&second);
        assert_eq!(reassembler.push(&first[6..]), Err(FrameError::Overrun));

        let both = [encode(b"a").unwrap(), encode(b"b").unwrap()].concat();
        assert_eq!(Reassembler::new().push(&both), Err(FrameError::Overrun));
    }
}
