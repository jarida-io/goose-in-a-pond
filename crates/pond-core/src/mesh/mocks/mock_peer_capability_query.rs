use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::mesh::domain::capabilities::PeerCapabilities;
use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::ports::peer_capability_query::{PeerCapabilityQuery, PeerCapabilityQueryError};

/// In-memory capability query; a peer never `set` answers the all-`false` default, not an error.
pub struct MockPeerCapabilityQuery {
    capabilities: Arc<RwLock<HashMap<PeerId, PeerCapabilities>>>,
}

impl MockPeerCapabilityQuery {
    pub fn new() -> Self {
        Self {
            capabilities: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn set(&self, peer: PeerId, capabilities: PeerCapabilities) {
        self.capabilities.write().await.insert(peer, capabilities);
    }
}

impl Default for MockPeerCapabilityQuery {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PeerCapabilityQuery for MockPeerCapabilityQuery {
    async fn capabilities_of(
        &self,
        peer: PeerId,
    ) -> Result<PeerCapabilities, PeerCapabilityQueryError> {
        Ok(self
            .capabilities
            .read()
            .await
            .get(&peer)
            .copied()
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    #[tokio::test]
    async fn trait_object_conformance() {
        let _: StdArc<dyn PeerCapabilityQuery> = StdArc::new(MockPeerCapabilityQuery::new());
    }

    #[tokio::test]
    async fn unset_peer_defaults_to_no_capabilities() {
        let query = MockPeerCapabilityQuery::new();
        let peer = PeerId::from([1u8; 32]);
        let caps = query.capabilities_of(peer).await.unwrap();
        assert_eq!(caps, PeerCapabilities::default());
    }

    #[tokio::test]
    async fn set_capabilities_are_returned() {
        let query = MockPeerCapabilityQuery::new();
        let peer = PeerId::from([2u8; 32]);
        let caps = PeerCapabilities {
            inference_available: true,
            lightning_available: false,
        };
        query.set(peer, caps).await;
        assert_eq!(query.capabilities_of(peer).await.unwrap(), caps);
    }
}
