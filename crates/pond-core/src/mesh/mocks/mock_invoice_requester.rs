use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::mesh::domain::millisats::Millisats;
use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::ports::invoice_requester::{InvoiceRequester, InvoiceRequesterError};

/// In-memory invoice requester; an unscripted peer answers `Timeout`, like one that never replies.
pub struct MockInvoiceRequester {
    responses: Arc<RwLock<HashMap<PeerId, Result<String, String>>>>,
}

impl MockInvoiceRequester {
    pub fn new() -> Self {
        Self {
            responses: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn set_invoice(&self, peer: PeerId, invoice: impl Into<String>) {
        self.responses
            .write()
            .await
            .insert(peer, Ok(invoice.into()));
    }

    pub async fn set_error(&self, peer: PeerId, message: impl Into<String>) {
        self.responses
            .write()
            .await
            .insert(peer, Err(message.into()));
    }
}

impl Default for MockInvoiceRequester {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InvoiceRequester for MockInvoiceRequester {
    async fn request_invoice(
        &self,
        peer: PeerId,
        _amount: Millisats,
    ) -> Result<String, InvoiceRequesterError> {
        match self.responses.read().await.get(&peer) {
            Some(Ok(invoice)) => Ok(invoice.clone()),
            Some(Err(message)) => Err(InvoiceRequesterError::PeerError(peer, message.clone())),
            None => Err(InvoiceRequesterError::Timeout(peer)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    #[tokio::test]
    async fn trait_object_conformance() {
        let _: StdArc<dyn InvoiceRequester> = StdArc::new(MockInvoiceRequester::new());
    }

    #[tokio::test]
    async fn unset_peer_times_out() {
        let requester = MockInvoiceRequester::new();
        let peer = PeerId::from([1u8; 32]);
        let result = requester.request_invoice(peer, Millisats::new(1000)).await;
        assert!(matches!(result, Err(InvoiceRequesterError::Timeout(_))));
    }

    #[tokio::test]
    async fn set_invoice_is_returned() {
        let requester = MockInvoiceRequester::new();
        let peer = PeerId::from([2u8; 32]);
        requester.set_invoice(peer, "mock-invoice-1").await;
        let result = requester
            .request_invoice(peer, Millisats::new(1000))
            .await
            .unwrap();
        assert_eq!(result, "mock-invoice-1");
    }

    #[tokio::test]
    async fn set_error_is_returned_as_peer_error() {
        let requester = MockInvoiceRequester::new();
        let peer = PeerId::from([3u8; 32]);
        requester
            .set_error(peer, "no payment rail configured")
            .await;
        let result = requester.request_invoice(peer, Millisats::new(1000)).await;
        assert!(matches!(
            result,
            Err(InvoiceRequesterError::PeerError(_, _))
        ));
    }
}
