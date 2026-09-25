use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::domain::trust_scope::TrustScope;

#[derive(Error, Debug)]
pub enum PeerDirectoryError {
    #[error("Peer not found: {0}")]
    PeerNotFound(PeerId),

    #[error("Directory error: {0}")]
    General(String),
}

/// Which peers this Pond trusts, and at what scope; a peer absent from it is not trusted at all.
#[async_trait]
pub trait PeerDirectory: Send + Sync {
    async fn add_trusted_peer(
        &self,
        peer: PeerId,
        scope: TrustScope,
    ) -> Result<(), PeerDirectoryError>;

    async fn remove_trusted_peer(&self, peer: PeerId) -> Result<(), PeerDirectoryError>;

    async fn list_trusted_peers(
        &self,
        scope: Option<TrustScope>,
    ) -> Result<Vec<PeerId>, PeerDirectoryError>;

    async fn trust_scope_of(&self, peer: PeerId) -> Result<Option<TrustScope>, PeerDirectoryError>;

    /// Last address `peer` was dialed at, for reconnect after restart; not part of the trust model.
    async fn record_peer_address(
        &self,
        peer: PeerId,
        address: String,
    ) -> Result<(), PeerDirectoryError>;

    /// Trusted peers with a recorded address, to seed the transport's reconnect loop at startup.
    async fn known_addresses(&self) -> Result<Vec<(PeerId, String)>, PeerDirectoryError>;
}
