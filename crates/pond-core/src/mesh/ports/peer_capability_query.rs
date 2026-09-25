use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::capabilities::PeerCapabilities;
use crate::mesh::domain::peer_id::PeerId;

#[derive(Error, Debug)]
pub enum PeerCapabilityQueryError {
    #[error("mesh transport error: {0}")]
    Transport(String),

    #[error("no response from peer {0} before timeout")]
    Timeout(PeerId),
}

/// Live "what does this peer offer now?". Not in `PeerDirectory`: a cache would look authoritative.
#[async_trait]
pub trait PeerCapabilityQuery: Send + Sync {
    async fn capabilities_of(
        &self,
        peer: PeerId,
    ) -> Result<PeerCapabilities, PeerCapabilityQueryError>;
}
