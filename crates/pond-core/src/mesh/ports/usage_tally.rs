use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::domain::token_count::TokenCount;

#[derive(Error, Debug)]
pub enum UsageTallyError {
    #[error("Tally error: {0}")]
    General(String),
}

/// Borrowed tokens are what we owe a peer and all `SettlementService` pays against; lent is
/// observability only. `mark_settled` only ever reduces `tokens_borrowed`.
#[async_trait]
pub trait UsageTally: Send + Sync {
    async fn record_borrowed(
        &self,
        peer: PeerId,
        tokens: TokenCount,
    ) -> Result<(), UsageTallyError>;

    async fn record_lent(&self, peer: PeerId, tokens: TokenCount) -> Result<(), UsageTallyError>;

    async fn pending_borrowed(&self, peer: PeerId) -> Result<TokenCount, UsageTallyError>;

    async fn pending_lent(&self, peer: PeerId) -> Result<TokenCount, UsageTallyError>;

    async fn mark_settled(&self, peer: PeerId, up_to: TokenCount) -> Result<(), UsageTallyError>;
}
