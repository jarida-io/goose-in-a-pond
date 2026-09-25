//! Bridges our [`PeerId`] (a raw ed25519 public key) and
//! [`pond_mesh_protocol::identity::MeshKeypair`] to libp2p's types: one key, wrapped two ways.

use libp2p::identity::{ed25519, Keypair, PublicKey};
use libp2p::PeerId as Libp2pPeerId;
use thiserror::Error;

use pond_core::mesh::domain::peer_id::PeerId;
use pond_mesh_protocol::identity::MeshKeypair;

#[derive(Error, Debug)]
pub enum IdentityBridgeError {
    #[error("malformed peer id: {0}")]
    MalformedPeerId(String),
}

/// The libp2p keypair for our `Handshake` signing key, so the swarm has the same identity.
pub fn to_libp2p_keypair(keypair: &MeshKeypair) -> Keypair {
    Keypair::ed25519_from_bytes(keypair.secret_bytes())
        .expect("a 32-byte ed25519 secret is always a valid libp2p keypair")
}

/// The libp2p `PeerId` to dial for a domain `PeerId`.
pub fn domain_peer_to_libp2p(peer: PeerId) -> Result<Libp2pPeerId, IdentityBridgeError> {
    let ed_public = ed25519::PublicKey::try_from_bytes(peer.as_bytes())
        .map_err(|err| IdentityBridgeError::MalformedPeerId(err.to_string()))?;
    let public: PublicKey = ed_public.into();
    Ok(public.to_peer_id())
}

/// Our domain `PeerId` from a libp2p public key; a bare libp2p `PeerId` can't yield key bytes.
pub fn public_key_to_domain(public: &PublicKey) -> Option<PeerId> {
    let ed_public = public.clone().try_into_ed25519().ok()?;
    Some(PeerId::from(ed_public.to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_peer_and_public_key_roundtrip() {
        let mesh_keypair = MeshKeypair::generate();
        let libp2p_keypair = to_libp2p_keypair(&mesh_keypair);
        let recovered = public_key_to_domain(&libp2p_keypair.public()).unwrap();
        assert_eq!(recovered, mesh_keypair.peer_id());
    }

    #[test]
    fn domain_peer_to_libp2p_matches_keypair_peer_id() {
        let mesh_keypair = MeshKeypair::generate();
        let libp2p_keypair = to_libp2p_keypair(&mesh_keypair);
        let derived = domain_peer_to_libp2p(mesh_keypair.peer_id()).unwrap();
        assert_eq!(derived, libp2p_keypair.public().to_peer_id());
    }
}
