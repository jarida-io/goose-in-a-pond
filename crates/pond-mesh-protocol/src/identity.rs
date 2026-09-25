//! Mesh peer identity and signatures; a [`PeerId`] *is* its ed25519 public key.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use pond_core::mesh::domain::peer_id::PeerId;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum IdentityError {
    #[error("malformed signature")]
    MalformedSignature,
    #[error("malformed peer id")]
    MalformedPeerId,
}

/// This Pond's mesh signing key; its [`PeerId`] is what other peers pin.
pub struct MeshKeypair(SigningKey);

impl MeshKeypair {
    pub fn generate() -> Self {
        Self(SigningKey::generate(&mut rand_core::OsRng))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&bytes))
    }

    pub fn peer_id(&self) -> PeerId {
        PeerId::from(self.0.verifying_key().to_bytes())
    }

    /// Raw 32-byte secret seed, to rebuild the same identity in another library (e.g. libp2p).
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.0.sign(message).to_bytes()
    }
}

/// Whether `peer`'s key signed `message`; malformed input is an error, never a panic.
pub fn verify(peer: PeerId, message: &[u8], signature: &[u8; 64]) -> Result<bool, IdentityError> {
    let verifying_key =
        VerifyingKey::from_bytes(peer.as_bytes()).map_err(|_| IdentityError::MalformedPeerId)?;
    let signature =
        Signature::from_slice(signature).map_err(|_| IdentityError::MalformedSignature)?;
    Ok(verifying_key.verify(message, &signature).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_then_verify_succeeds() {
        let keypair = MeshKeypair::generate();
        let signature = keypair.sign(b"hello mesh");
        assert!(verify(keypair.peer_id(), b"hello mesh", &signature).unwrap());
    }

    #[test]
    fn verify_fails_for_tampered_message() {
        let keypair = MeshKeypair::generate();
        let signature = keypair.sign(b"hello mesh");
        assert!(!verify(keypair.peer_id(), b"goodbye mesh", &signature).unwrap());
    }

    #[test]
    fn verify_fails_for_wrong_signer() {
        let signer = MeshKeypair::generate();
        let impostor = MeshKeypair::generate();
        let signature = signer.sign(b"hello mesh");
        assert!(!verify(impostor.peer_id(), b"hello mesh", &signature).unwrap());
    }

    #[test]
    fn peer_id_is_stable_across_calls() {
        let keypair = MeshKeypair::generate();
        assert_eq!(keypair.peer_id(), keypair.peer_id());
    }

    #[test]
    fn from_bytes_roundtrips_peer_id() {
        let original = MeshKeypair::generate();
        let restored = MeshKeypair::from_bytes(original.0.to_bytes());
        assert_eq!(original.peer_id(), restored.peer_id());
    }
}
