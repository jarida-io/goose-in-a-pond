use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::RwLock;

use crate::mesh::domain::millisats::Millisats;
use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::domain::settlement::SettlementRecord;
use crate::mesh::ports::payment_rail::{PaymentRail, PaymentRailError};

/// In-memory payment rail; any preimage `"preimage-for-<invoice>"` verifies.
pub struct MockPaymentRail {
    next_invoice: AtomicU64,
    issued: Arc<RwLock<HashMap<String, Millisats>>>,
}

impl MockPaymentRail {
    pub fn new() -> Self {
        Self {
            next_invoice: AtomicU64::new(1),
            issued: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for MockPaymentRail {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PaymentRail for MockPaymentRail {
    async fn issue_invoice(&self, amount: Millisats) -> Result<String, PaymentRailError> {
        let id = self.next_invoice.fetch_add(1, Ordering::SeqCst);
        let invoice = format!("mock-invoice-{id}");
        self.issued.write().await.insert(invoice.clone(), amount);
        Ok(invoice)
    }

    async fn verify_preimage(
        &self,
        invoice: &str,
        preimage: &str,
    ) -> Result<bool, PaymentRailError> {
        if !self.issued.read().await.contains_key(invoice) {
            return Err(PaymentRailError::InvalidInvoice(invoice.to_string()));
        }
        Ok(preimage == format!("preimage-for-{invoice}"))
    }

    async fn batch_settle(
        &self,
        peer: PeerId,
        amount: Millisats,
        invoice: &str,
    ) -> Result<SettlementRecord, PaymentRailError> {
        if !self.issued.read().await.contains_key(invoice) {
            return Err(PaymentRailError::InvalidInvoice(invoice.to_string()));
        }
        Ok(SettlementRecord {
            peer_id: peer,
            amount,
            preimage: format!("preimage-for-{invoice}"),
            settled_at: Utc::now(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    #[tokio::test]
    async fn trait_object_conformance() {
        let _: StdArc<dyn PaymentRail> = StdArc::new(MockPaymentRail::new());
    }

    #[tokio::test]
    async fn issue_then_verify_correct_preimage() {
        let rail = MockPaymentRail::new();
        let invoice = rail.issue_invoice(Millisats::new(1000)).await.unwrap();
        let preimage = format!("preimage-for-{invoice}");
        assert!(rail.verify_preimage(&invoice, &preimage).await.unwrap());
    }

    #[tokio::test]
    async fn verify_wrong_preimage_fails() {
        let rail = MockPaymentRail::new();
        let invoice = rail.issue_invoice(Millisats::new(1000)).await.unwrap();
        assert!(!rail.verify_preimage(&invoice, "wrong").await.unwrap());
    }

    #[tokio::test]
    async fn verify_unknown_invoice_errors() {
        let rail = MockPaymentRail::new();
        let result = rail.verify_preimage("nonexistent", "x").await;
        assert!(matches!(result, Err(PaymentRailError::InvalidInvoice(_))));
    }

    #[tokio::test]
    async fn batch_settle_returns_stored_preimage() {
        let rail = MockPaymentRail::new();
        let peer = PeerId::from([9u8; 32]);
        let invoice = rail.issue_invoice(Millisats::new(500)).await.unwrap();
        let record = rail
            .batch_settle(peer, Millisats::new(500), &invoice)
            .await
            .unwrap();
        assert_eq!(record.peer_id, peer);
        assert_eq!(record.amount, Millisats::new(500));
        assert!(!record.preimage.is_empty());
    }

    #[tokio::test]
    async fn batch_settle_of_unknown_invoice_errors() {
        let rail = MockPaymentRail::new();
        let peer = PeerId::from([9u8; 32]);
        let result = rail
            .batch_settle(peer, Millisats::new(500), "nonexistent")
            .await;
        assert!(matches!(result, Err(PaymentRailError::InvalidInvoice(_))));
    }
}
