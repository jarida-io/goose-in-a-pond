//! ChaCha20-Poly1305 under the CPace run's per-direction keys (`pond-setup-channel.md`,
//! "Sealing"). The nonce is four zero bytes and a big-endian counter, one counter per direction
//! from 0; there is no associated data. Any failure ends the session.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use pond_core::security::ports::handshake::{ChannelKeys, CHANNEL_KEY_LEN};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SealError {
    /// The tag failed: tampered, replayed, reordered, or sealed under another key.
    #[error("a sealed message did not open")]
    Rejected,
    /// The message is not a sealed envelope at all.
    #[error("a message that must be sealed was not")]
    NotSealed,
    /// A counter would repeat; unreachable in a session's lifetime, refused anyway.
    #[error("this direction's counter is spent")]
    Exhausted,
}

/// The counter's nonce: four zero bytes, then the counter in big-endian.
pub fn nonce(counter: u64) -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    nonce[4..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

/// One direction's sending half.
pub struct Sealer {
    cipher: ChaCha20Poly1305,
    next: u64,
}

impl Sealer {
    pub fn new(key: &[u8; CHANNEL_KEY_LEN]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            next: 0,
        }
    }

    /// The counter the next message is sealed under.
    pub fn next_counter(&self) -> u64 {
        self.next
    }

    /// Ciphertext and tag.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, SealError> {
        let after = self.next.checked_add(1).ok_or(SealError::Exhausted)?;
        let sealed = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce(self.next)), plaintext)
            .map_err(|_| SealError::Exhausted)?;
        self.next = after;
        Ok(sealed)
    }
}

/// One direction's receiving half. It expects exactly the next counter, so a replayed or
/// reordered message fails its tag.
pub struct Opener {
    cipher: ChaCha20Poly1305,
    next: u64,
}

impl Opener {
    pub fn new(key: &[u8; CHANNEL_KEY_LEN]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            next: 0,
        }
    }

    pub fn next_counter(&self) -> u64 {
        self.next
    }

    /// The plaintext, wiped when dropped: it can hold tokens or a Wi-Fi passphrase.
    pub fn open(&mut self, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, SealError> {
        let after = self.next.checked_add(1).ok_or(SealError::Exhausted)?;
        let plaintext = self
            .cipher
            .decrypt(Nonce::from_slice(&nonce(self.next)), sealed)
            .map_err(|_| SealError::Rejected)?;
        self.next = after;
        Ok(Zeroizing::new(plaintext))
    }
}

/// `{"sealed": "<standard base64>"}`, and nothing else.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<'a> {
    #[serde(borrow)]
    sealed: std::borrow::Cow<'a, str>,
}

/// One end of a sealed session: what it sends under one key and what it receives under the other.
pub struct ChannelCipher {
    sealer: Sealer,
    opener: Opener,
}

impl ChannelCipher {
    /// The pond seals with `k_p2c` and opens with `k_c2p`.
    pub fn pond(keys: &ChannelKeys) -> Self {
        Self {
            sealer: Sealer::new(keys.pond_to_phone()),
            opener: Opener::new(keys.phone_to_pond()),
        }
    }

    /// The phone's end, for tests and tools that speak to a pond.
    pub fn phone(keys: &ChannelKeys) -> Self {
        Self {
            sealer: Sealer::new(keys.phone_to_pond()),
            opener: Opener::new(keys.pond_to_phone()),
        }
    }

    pub fn sealer(&self) -> &Sealer {
        &self.sealer
    }

    pub fn opener(&self) -> &Opener {
        &self.opener
    }

    /// The bare base64, as `pake_verify`'s `sealed` field carries it.
    pub fn seal_to_base64(&mut self, plaintext: &[u8]) -> Result<String, SealError> {
        Ok(B64.encode(self.sealer.seal(plaintext)?))
    }

    pub fn open_base64(&mut self, sealed: &str) -> Result<Zeroizing<Vec<u8>>, SealError> {
        let bytes = B64
            .decode(sealed.as_bytes())
            .map_err(|_| SealError::NotSealed)?;
        self.opener.open(&bytes)
    }

    /// A whole envelope message, ready to frame.
    pub fn seal_envelope(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, SealError> {
        let sealed = self.seal_to_base64(plaintext)?;
        serde_json::to_vec(&Envelope {
            sealed: sealed.into(),
        })
        .map_err(|_| SealError::Exhausted)
    }

    pub fn open_envelope(&mut self, message: &[u8]) -> Result<Zeroizing<Vec<u8>>, SealError> {
        let envelope = parse_envelope(message).ok_or(SealError::NotSealed)?;
        self.open_base64(&envelope.sealed)
    }
}

/// An object only: serde would also read the struct from a one-element array.
fn parse_envelope(message: &[u8]) -> Option<Envelope<'_>> {
    let first = message.iter().find(|byte| !byte.is_ascii_whitespace())?;
    if *first != b'{' {
        return None;
    }
    serde_json::from_slice(message).ok()
}

/// The size of the envelope that seals `plaintext_len` bytes, so an answer can be checked to fit.
pub fn envelope_len(plaintext_len: usize) -> usize {
    const OVERHEAD: usize = r#"{"sealed":""}"#.len();
    OVERHEAD + (plaintext_len + TAG_LEN).div_ceil(3) * 4
}

/// Whether a clear-phase message is shaped like an envelope, so it can be refused as one.
pub fn is_envelope(message: &[u8]) -> bool {
    parse_envelope(message).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> ChannelKeys {
        ChannelKeys::new(&[1u8; 32], &[2u8; 32])
    }

    #[test]
    fn the_nonce_is_four_zero_bytes_then_the_big_endian_counter() {
        assert_eq!(nonce(0), [0u8; 12]);
        assert_eq!(nonce(1), [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(
            nonce(0x0102_0304_0506_0708),
            [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]
        );
    }

    #[test]
    fn each_side_opens_what_the_other_sealed_in_order() {
        let (mut pond, mut phone) = (ChannelCipher::pond(&keys()), ChannelCipher::phone(&keys()));
        for i in 0..5u8 {
            let down = pond.seal_envelope(&[i; 10]).unwrap();
            assert_eq!(phone.open_envelope(&down).unwrap().as_slice(), &[i; 10]);
            let up = phone.seal_envelope(&[i + 100; 3]).unwrap();
            assert_eq!(pond.open_envelope(&up).unwrap().as_slice(), &[i + 100; 3]);
        }
        assert_eq!(pond.sealer().next_counter(), 5);
        assert_eq!(pond.opener().next_counter(), 5);
    }

    #[test]
    fn the_two_directions_use_different_keys() {
        let mut pond = ChannelCipher::pond(&keys());
        let sealed = pond.seal_envelope(b"{}").unwrap();
        assert_eq!(
            pond.open_envelope(&sealed).unwrap_err(),
            SealError::Rejected,
            "a pond cannot open its own message: no reflection"
        );
    }

    #[test]
    fn a_tampered_replayed_or_skipped_message_is_rejected() {
        let (mut pond, mut phone) = (ChannelCipher::pond(&keys()), ChannelCipher::phone(&keys()));
        let first = phone.sealer.seal(b"first").unwrap();
        let second = phone.sealer.seal(b"second").unwrap();

        let mut tampered = first.clone();
        tampered[0] ^= 1;
        assert_eq!(
            pond.opener.open(&tampered).unwrap_err(),
            SealError::Rejected
        );
        assert_eq!(pond.opener.next_counter(), 0, "a failure moves no counter");
        assert_eq!(
            pond.opener.open(&second).unwrap_err(),
            SealError::Rejected,
            "skipping ahead fails"
        );
        assert_eq!(pond.opener.open(&first).unwrap().as_slice(), b"first");
        assert_eq!(
            pond.opener.open(&first).unwrap_err(),
            SealError::Rejected,
            "a replay fails"
        );
        assert_eq!(pond.opener.open(&second).unwrap().as_slice(), b"second");
    }

    #[test]
    fn a_sealed_message_is_the_plaintext_plus_a_sixteen_byte_tag() {
        let mut sealer = Sealer::new(&[9u8; 32]);
        assert_eq!(sealer.seal(b"abc").unwrap().len(), 3 + TAG_LEN);
    }

    #[test]
    fn only_a_bare_envelope_is_an_envelope() {
        assert!(is_envelope(br#"{"sealed":"AAAA"}"#));
        assert!(!is_envelope(br#"{"sealed":"AAAA","id":1}"#));
        assert!(!is_envelope(br#"{"id":1,"type":"hello"}"#));
        assert!(!is_envelope(br#"["AAAA"]"#));
        let mut pond = ChannelCipher::pond(&keys());
        assert_eq!(
            pond.open_envelope(br#"{"sealed":"not base64!"}"#)
                .unwrap_err(),
            SealError::NotSealed
        );
        assert_eq!(
            pond.open_envelope(br#"{"id":1,"type":"status"}"#)
                .unwrap_err(),
            SealError::NotSealed
        );
    }

    #[test]
    fn the_envelope_length_is_exact() {
        let mut pond = ChannelCipher::pond(&keys());
        for len in [0, 1, 2, 3, 100, 12_000] {
            assert_eq!(
                pond.seal_envelope(&vec![b'x'; len]).unwrap().len(),
                envelope_len(len),
                "{len}"
            );
        }
    }

    #[test]
    fn a_spent_counter_refuses_to_seal() {
        let mut sealer = Sealer::new(&[9u8; 32]);
        sealer.next = u64::MAX;
        assert_eq!(sealer.seal(b"x").unwrap_err(), SealError::Exhausted);
    }
}
