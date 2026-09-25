use async_trait::async_trait;
use thiserror::Error;

use crate::mesh::domain::millisats::Millisats;
use crate::mesh::domain::peer_id::PeerId;

#[derive(Error, Debug)]
pub enum InvoiceRequesterError {
    #[error("mesh transport error: {0}")]
    Transport(String),

    #[error("peer {0} reported an error: {1}")]
    PeerError(PeerId, String),

    #[error("no invoice response from peer {0} before timeout")]
    Timeout(PeerId),
}

/// Asks a trusted peer for the invoice `PaymentRail::batch_settle` pays. Implemented by
/// `MeshInferenceService`, the transport's sole `recv()` consumer; a second would compete.
#[async_trait]
pub trait InvoiceRequester: Send + Sync {
    async fn request_invoice(
        &self,
        peer: PeerId,
        amount: Millisats,
    ) -> Result<String, InvoiceRequesterError>;
}
