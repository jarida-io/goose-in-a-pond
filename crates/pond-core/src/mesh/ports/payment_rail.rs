use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::millisats::Millisats;
use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::domain::settlement::SettlementRecord;

#[derive(Error, Debug)]
pub enum PaymentRailError {
    #[error("Invalid invoice: {0}")]
    InvalidInvoice(String),

    #[error("Settlement failed: {0}")]
    SettlementFailed(String),
}

/// Lightning settlement. Only a background job calls `batch_settle`, never per token.
#[async_trait]
pub trait PaymentRail: Send + Sync {
    async fn issue_invoice(&self, amount: Millisats) -> Result<String, PaymentRailError>;

    async fn verify_preimage(
        &self,
        invoice: &str,
        preimage: &str,
    ) -> Result<bool, PaymentRailError>;

    /// Pay `peer`'s own `invoice`, fetched by the caller (this port has no transport). `amount` is
    /// separate so an adapter can cross-check it against the BOLT11 amount before paying.
    async fn batch_settle(
        &self,
        peer: PeerId,
        amount: Millisats,
        invoice: &str,
    ) -> Result<SettlementRecord, PaymentRailError>;
}
