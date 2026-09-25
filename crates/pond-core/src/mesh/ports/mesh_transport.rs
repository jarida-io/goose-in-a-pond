use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::peer_id::PeerId;

#[derive(Error, Debug)]
pub enum MeshTransportError {
    #[error("Peer unreachable: {0}")]
    PeerUnreachable(PeerId),

    #[error("Malformed address: {0}")]
    MalformedAddress(String),

    #[error("Transport error: {0}")]
    Transport(String),
}

/// Connectivity to other Ponds; `address` is an opaque dial hint so no libp2p type leaks in.
#[async_trait]
pub trait MeshTransport: Send + Sync {
    async fn connect(&self, peer: PeerId, address: String) -> Result<(), MeshTransportError>;

    async fn send(&self, peer: PeerId, frame: Vec<u8>) -> Result<(), MeshTransportError>;

    async fn connected_peers(&self) -> Result<Vec<PeerId>, MeshTransportError>;

    /// Next inbound frame from any connected peer; waits until one arrives.
    async fn recv(&self) -> Result<(PeerId, Vec<u8>), MeshTransportError>;

    /// This Pond's own identity. Pure lookup, no I/O — sync.
    fn local_peer_id(&self) -> PeerId;

    /// Addresses this node is confirmed listening on, as opaque dial hints `connect()` accepts.
    async fn listen_addresses(&self) -> Result<Vec<String>, MeshTransportError>;
}
