//! Stable mesh peer id: 32 bytes, the length of an ed25519 public key / libp2p peer key.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId([u8; 32]);

impl PeerId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for PeerId {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl std::fmt::Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Error, Debug)]
pub enum PeerIdParseError {
    #[error("invalid peer id hex: {0}")]
    InvalidHex(String),
}

/// Inverse of the hex `Display`, for storage that persists a `PeerId` as text.
impl std::str::FromStr for PeerId {
    type Err = PeerIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 64 {
            return Err(PeerIdParseError::InvalidHex(s.to_string()));
        }
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|_| PeerIdParseError::InvalidHex(s.to_string()))?;
        }
        Ok(Self(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_lowercase_hex() {
        let peer = PeerId::from([0xabu8; 32]);
        assert_eq!(peer.to_string(), "ab".repeat(32));
    }

    #[test]
    fn distinct_bytes_are_not_equal() {
        assert_ne!(PeerId::from([0u8; 32]), PeerId::from([1u8; 32]));
    }

    #[test]
    fn from_str_roundtrips_display() {
        let peer = PeerId::from([0x5cu8; 32]);
        let parsed: PeerId = peer.to_string().parse().unwrap();
        assert_eq!(peer, parsed);
    }

    #[test]
    fn from_str_rejects_wrong_length() {
        assert!("abcd".parse::<PeerId>().is_err());
    }

    #[test]
    fn from_str_rejects_non_hex() {
        let not_hex = "z".repeat(64);
        assert!(not_hex.parse::<PeerId>().is_err());
    }
}
