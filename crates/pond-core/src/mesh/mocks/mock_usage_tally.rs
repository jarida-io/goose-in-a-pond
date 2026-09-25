use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::domain::token_count::TokenCount;
use crate::mesh::ports::usage_tally::{UsageTally, UsageTallyError};

/// In-memory usage tally; an unrecorded peer has zero pending, not a missing entry.
pub struct MockUsageTally {
    borrowed: Arc<RwLock<HashMap<PeerId, TokenCount>>>,
    lent: Arc<RwLock<HashMap<PeerId, TokenCount>>>,
}

impl MockUsageTally {
    pub fn new() -> Self {
        Self {
            borrowed: Arc::new(RwLock::new(HashMap::new())),
            lent: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for MockUsageTally {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl UsageTally for MockUsageTally {
    async fn record_borrowed(
        &self,
        peer: PeerId,
        tokens: TokenCount,
    ) -> Result<(), UsageTallyError> {
        let mut borrowed = self.borrowed.write().await;
        let current = borrowed.get(&peer).copied().unwrap_or(TokenCount::new(0));
        let updated = current
            .checked_add(tokens)
            .ok_or_else(|| UsageTallyError::General("tally overflow".to_string()))?;
        borrowed.insert(peer, updated);
        Ok(())
    }

    async fn record_lent(&self, peer: PeerId, tokens: TokenCount) -> Result<(), UsageTallyError> {
        let mut lent = self.lent.write().await;
        let current = lent.get(&peer).copied().unwrap_or(TokenCount::new(0));
        let updated = current
            .checked_add(tokens)
            .ok_or_else(|| UsageTallyError::General("tally overflow".to_string()))?;
        lent.insert(peer, updated);
        Ok(())
    }

    async fn pending_borrowed(&self, peer: PeerId) -> Result<TokenCount, UsageTallyError> {
        Ok(self
            .borrowed
            .read()
            .await
            .get(&peer)
            .copied()
            .unwrap_or(TokenCount::new(0)))
    }

    async fn pending_lent(&self, peer: PeerId) -> Result<TokenCount, UsageTallyError> {
        Ok(self
            .lent
            .read()
            .await
            .get(&peer)
            .copied()
            .unwrap_or(TokenCount::new(0)))
    }

    async fn mark_settled(&self, peer: PeerId, up_to: TokenCount) -> Result<(), UsageTallyError> {
        let mut borrowed = self.borrowed.write().await;
        let current = borrowed.get(&peer).copied().unwrap_or(TokenCount::new(0));
        let remaining = current
            .checked_sub(up_to)
            .ok_or_else(|| UsageTallyError::General("settled more than pending".to_string()))?;
        borrowed.insert(peer, remaining);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    #[tokio::test]
    async fn trait_object_conformance() {
        let _: StdArc<dyn UsageTally> = StdArc::new(MockUsageTally::new());
    }

    #[tokio::test]
    async fn record_borrowed_accumulates() {
        let tally = MockUsageTally::new();
        let peer = PeerId::from([1u8; 32]);
        tally
            .record_borrowed(peer, TokenCount::new(100))
            .await
            .unwrap();
        tally
            .record_borrowed(peer, TokenCount::new(50))
            .await
            .unwrap();
        assert_eq!(
            tally.pending_borrowed(peer).await.unwrap(),
            TokenCount::new(150)
        );
    }

    #[tokio::test]
    async fn mark_settled_reduces_borrowed() {
        let tally = MockUsageTally::new();
        let peer = PeerId::from([2u8; 32]);
        tally
            .record_borrowed(peer, TokenCount::new(100))
            .await
            .unwrap();
        tally.mark_settled(peer, TokenCount::new(60)).await.unwrap();
        assert_eq!(
            tally.pending_borrowed(peer).await.unwrap(),
            TokenCount::new(40)
        );
    }

    #[tokio::test]
    async fn mark_settled_more_than_pending_errors() {
        let tally = MockUsageTally::new();
        let peer = PeerId::from([3u8; 32]);
        tally
            .record_borrowed(peer, TokenCount::new(10))
            .await
            .unwrap();
        let result = tally.mark_settled(peer, TokenCount::new(11)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn unknown_peer_has_zero_pending() {
        let tally = MockUsageTally::new();
        let peer = PeerId::from([4u8; 32]);
        assert_eq!(
            tally.pending_borrowed(peer).await.unwrap(),
            TokenCount::new(0)
        );
        assert_eq!(tally.pending_lent(peer).await.unwrap(), TokenCount::new(0));
    }

    #[tokio::test]
    async fn lent_and_borrowed_are_independent() {
        let tally = MockUsageTally::new();
        let peer = PeerId::from([5u8; 32]);
        tally
            .record_borrowed(peer, TokenCount::new(30))
            .await
            .unwrap();
        tally.record_lent(peer, TokenCount::new(70)).await.unwrap();

        assert_eq!(
            tally.pending_borrowed(peer).await.unwrap(),
            TokenCount::new(30)
        );
        assert_eq!(tally.pending_lent(peer).await.unwrap(), TokenCount::new(70));

        tally.mark_settled(peer, TokenCount::new(30)).await.unwrap();
        assert_eq!(
            tally.pending_borrowed(peer).await.unwrap(),
            TokenCount::new(0)
        );
        assert_eq!(tally.pending_lent(peer).await.unwrap(), TokenCount::new(70));
    }
}
