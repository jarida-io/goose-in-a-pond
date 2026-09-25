//! Turns per-peer usage into batched Lightning payments; only a periodic server job calls it.

use std::sync::Arc;

use crate::mesh::domain::millisats::Millisats;
use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::ports::invoice_requester::InvoiceRequester;
use crate::mesh::ports::payment_rail::PaymentRail;
use crate::mesh::ports::peer_directory::PeerDirectory;
use crate::mesh::ports::usage_tally::UsageTally;

/// What happened when settlement was attempted for one peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettlementOutcome {
    /// Nothing owed — `pending_borrowed` was zero, no invoice was requested.
    NothingPending {
        peer: PeerId,
    },
    /// Owed but under 1 sat: a 0-sat invoice would clear the debt for free, so it stays pending.
    BelowSettlementMinimum {
        peer: PeerId,
        amount: Millisats,
    },
    Settled {
        peer: PeerId,
        amount: Millisats,
        preimage: String,
    },
    Failed {
        peer: PeerId,
        error: String,
    },
}

pub struct SettlementService {
    peer_directory: Arc<dyn PeerDirectory>,
    usage_tally: Arc<dyn UsageTally>,
    payment_rail: Arc<dyn PaymentRail>,
    invoice_requester: Arc<dyn InvoiceRequester>,
}

impl SettlementService {
    pub fn new(
        peer_directory: Arc<dyn PeerDirectory>,
        usage_tally: Arc<dyn UsageTally>,
        payment_rail: Arc<dyn PaymentRail>,
        invoice_requester: Arc<dyn InvoiceRequester>,
    ) -> Self {
        Self {
            peer_directory,
            usage_tally,
            payment_rail,
            invoice_requester,
        }
    }

    /// One pass over trusted peers with pending usage: invoice, pay, mark settled. A rate of `0`
    /// means settlement is unconfigured (a no-op); `Err` means the pass could not start at all.
    pub async fn run_once(
        &self,
        millisats_per_token: u64,
    ) -> Result<Vec<SettlementOutcome>, String> {
        if millisats_per_token == 0 {
            return Ok(vec![]);
        }

        let peers = self
            .peer_directory
            .list_trusted_peers(None)
            .await
            .map_err(|err| format!("failed to list trusted peers: {err}"))?;

        let mut outcomes = Vec::with_capacity(peers.len());
        for peer in peers {
            outcomes.push(self.settle_one(peer, millisats_per_token).await);
        }
        Ok(outcomes)
    }

    async fn settle_one(&self, peer: PeerId, millisats_per_token: u64) -> SettlementOutcome {
        // Only the borrowed side — tokens_lent is peer's own job to collect.
        let pending = match self.usage_tally.pending_borrowed(peer).await {
            Ok(t) => t,
            Err(err) => {
                return SettlementOutcome::Failed {
                    peer,
                    error: format!("failed to read pending tally: {err}"),
                }
            }
        };
        if pending.value() == 0 {
            return SettlementOutcome::NothingPending { peer };
        }

        let Some(amount_value) = pending.value().checked_mul(millisats_per_token) else {
            return SettlementOutcome::Failed {
                peer,
                error: format!("amount overflow: {pending} at {millisats_per_token} msat/token"),
            };
        };
        let amount = Millisats::new(amount_value);

        // Invoices are whole-sat: a sub-sat amount would pay nothing yet clear the whole debt.
        const MSATS_PER_SAT: u64 = 1000;
        if amount.value() < MSATS_PER_SAT {
            return SettlementOutcome::BelowSettlementMinimum { peer, amount };
        }

        let invoice = match self.invoice_requester.request_invoice(peer, amount).await {
            Ok(invoice) => invoice,
            Err(err) => {
                return SettlementOutcome::Failed {
                    peer,
                    error: format!("failed to get invoice: {err}"),
                }
            }
        };

        match self.payment_rail.batch_settle(peer, amount, &invoice).await {
            Ok(record) => {
                // Money moved, so not `Failed`; the usage just gets billed again next pass.
                if let Err(err) = self.usage_tally.mark_settled(peer, pending).await {
                    tracing::warn!(
                        "settlement: paid {peer} but failed to mark {pending} settled: {err}"
                    );
                }
                SettlementOutcome::Settled {
                    peer,
                    amount,
                    preimage: record.preimage,
                }
            }
            Err(err) => SettlementOutcome::Failed {
                peer,
                error: format!("payment failed: {err}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::domain::token_count::TokenCount;
    use crate::mesh::domain::trust_scope::TrustScope;
    use crate::mesh::mocks::mock_invoice_requester::MockInvoiceRequester;
    use crate::mesh::mocks::mock_payment_rail::MockPaymentRail;
    use crate::mesh::mocks::mock_peer_directory::MockPeerDirectory;
    use crate::mesh::mocks::mock_usage_tally::MockUsageTally;

    async fn make_service() -> (
        SettlementService,
        Arc<MockPeerDirectory>,
        Arc<MockUsageTally>,
        Arc<MockPaymentRail>,
        Arc<MockInvoiceRequester>,
    ) {
        let peer_directory = Arc::new(MockPeerDirectory::new());
        let usage_tally = Arc::new(MockUsageTally::new());
        let payment_rail = Arc::new(MockPaymentRail::new());
        let invoice_requester = Arc::new(MockInvoiceRequester::new());
        let service = SettlementService::new(
            peer_directory.clone(),
            usage_tally.clone(),
            payment_rail.clone(),
            invoice_requester.clone(),
        );
        (
            service,
            peer_directory,
            usage_tally,
            payment_rail,
            invoice_requester,
        )
    }

    #[tokio::test]
    async fn lending_to_a_peer_never_triggers_a_payment() {
        let (service, peer_directory, usage_tally, _rail, _invoices) = make_service().await;
        let peer = PeerId::from([9u8; 32]);
        peer_directory
            .add_trusted_peer(peer, TrustScope::Circle)
            .await
            .unwrap();
        // They owe us. No invoice is configured, so a wrongful payment would fail loudly.
        usage_tally
            .record_lent(peer, TokenCount::new(10_000))
            .await
            .unwrap();

        let outcomes = service.run_once(5).await.unwrap();
        assert_eq!(
            outcomes,
            vec![SettlementOutcome::NothingPending { peer }],
            "tokens_lent must never be read by settlement — only tokens_borrowed"
        );

        // The receivable is untouched — still owed to us, still visible.
        assert_eq!(
            usage_tally.pending_lent(peer).await.unwrap(),
            TokenCount::new(10_000)
        );
    }

    #[tokio::test]
    async fn sub_sat_debt_is_carried_forward_instead_of_settled_for_free() {
        let (service, peer_directory, usage_tally, _rail, invoices) = make_service().await;
        let peer = PeerId::from([10u8; 32]);
        peer_directory
            .add_trusted_peer(peer, TrustScope::Circle)
            .await
            .unwrap();
        usage_tally
            .record_borrowed(peer, TokenCount::new(100))
            .await
            .unwrap();
        // Any invoice request would surface as `Failed`, not `BelowSettlementMinimum`.
        invoices.set_invoice(peer, "must-not-be-used").await;

        let outcomes = service.run_once(5).await.unwrap(); // 100 * 5 msat = 500 msat, < 1 sat
        assert_eq!(
            outcomes,
            vec![SettlementOutcome::BelowSettlementMinimum {
                peer,
                amount: Millisats::new(500),
            }]
        );

        // The debt survives — nothing was paid, so nothing should be cleared.
        assert_eq!(
            usage_tally.pending_borrowed(peer).await.unwrap(),
            TokenCount::new(100)
        );
    }

    #[tokio::test]
    async fn debt_settles_normally_once_it_reaches_one_sat() {
        let (service, peer_directory, usage_tally, rail, invoices) = make_service().await;
        let peer = PeerId::from([11u8; 32]);
        peer_directory
            .add_trusted_peer(peer, TrustScope::Circle)
            .await
            .unwrap();
        // 200 tokens * 5 msat/token = 1000 msat = exactly 1 sat.
        usage_tally
            .record_borrowed(peer, TokenCount::new(200))
            .await
            .unwrap();
        let invoice = rail.issue_invoice(Millisats::new(1000)).await.unwrap();
        invoices.set_invoice(peer, invoice).await;

        let outcomes = service.run_once(5).await.unwrap();
        assert!(matches!(
            &outcomes[0],
            SettlementOutcome::Settled { amount, .. } if amount.value() == 1000
        ));
        assert_eq!(
            usage_tally.pending_borrowed(peer).await.unwrap(),
            TokenCount::new(0)
        );
    }

    #[tokio::test]
    async fn zero_rate_is_a_no_op_even_with_pending_usage() {
        let (service, peer_directory, usage_tally, _rail, _invoices) = make_service().await;
        let peer = PeerId::from([1u8; 32]);
        peer_directory
            .add_trusted_peer(peer, TrustScope::Circle)
            .await
            .unwrap();
        usage_tally
            .record_borrowed(peer, TokenCount::new(500))
            .await
            .unwrap();

        let outcomes = service.run_once(0).await.unwrap();
        assert!(outcomes.is_empty(), "rate 0 must not settle anything");
    }

    #[tokio::test]
    async fn no_trusted_peers_settles_nothing() {
        let (service, _pd, _ut, _rail, _inv) = make_service().await;
        let outcomes = service.run_once(10).await.unwrap();
        assert!(outcomes.is_empty());
    }

    #[tokio::test]
    async fn peer_with_no_pending_usage_is_reported_but_not_paid() {
        let (service, peer_directory, _usage_tally, _rail, _invoices) = make_service().await;
        let peer = PeerId::from([2u8; 32]);
        peer_directory
            .add_trusted_peer(peer, TrustScope::Circle)
            .await
            .unwrap();

        let outcomes = service.run_once(10).await.unwrap();
        assert_eq!(outcomes, vec![SettlementOutcome::NothingPending { peer }]);
    }

    #[tokio::test]
    async fn pending_usage_is_paid_and_marked_settled() {
        let (service, peer_directory, usage_tally, rail, invoices) = make_service().await;
        let peer = PeerId::from([3u8; 32]);
        peer_directory
            .add_trusted_peer(peer, TrustScope::Circle)
            .await
            .unwrap();
        // Must clear the 1-sat settlement floor to test this path.
        usage_tally
            .record_borrowed(peer, TokenCount::new(1000))
            .await
            .unwrap();
        // The mock rail only pays invoices it issued; minting one stands in for the peer's wallet.
        let invoice = rail.issue_invoice(Millisats::new(5000)).await.unwrap();
        invoices.set_invoice(peer, invoice).await;

        let outcomes = service.run_once(5).await.unwrap(); // 1000 tokens * 5 msat/token = 5000 msat
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            SettlementOutcome::Settled {
                peer: p,
                amount,
                preimage,
            } => {
                assert_eq!(*p, peer);
                assert_eq!(amount.value(), 5000);
                assert!(!preimage.is_empty());
            }
            other => panic!("expected Settled, got {other:?}"),
        }

        // Usage tally cleared — a second pass has nothing left to settle.
        let remaining = usage_tally.pending_borrowed(peer).await.unwrap();
        assert_eq!(remaining.value(), 0);
    }

    #[tokio::test]
    async fn peer_that_never_answers_the_invoice_request_fails_that_peer_only() {
        let (service, peer_directory, usage_tally, _rail, _invoices) = make_service().await;
        let peer = PeerId::from([4u8; 32]);
        peer_directory
            .add_trusted_peer(peer, TrustScope::Circle)
            .await
            .unwrap();
        // Must clear the 1-sat floor to reach the invoice request.
        usage_tally
            .record_borrowed(peer, TokenCount::new(2000))
            .await
            .unwrap();
        // Deliberately no invoice set — MockInvoiceRequester times out.

        let outcomes = service.run_once(1).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(&outcomes[0], SettlementOutcome::Failed { peer: p, .. } if *p == peer));

        // A failed invoice request must not clear the usage — it's still owed.
        let remaining = usage_tally.pending_borrowed(peer).await.unwrap();
        assert_eq!(remaining.value(), 2000);
    }

    #[tokio::test]
    async fn amount_overflow_fails_loudly_rather_than_wrapping() {
        let (service, peer_directory, usage_tally, _rail, invoices) = make_service().await;
        let peer = PeerId::from([5u8; 32]);
        peer_directory
            .add_trusted_peer(peer, TrustScope::Circle)
            .await
            .unwrap();
        usage_tally
            .record_borrowed(peer, TokenCount::new(u64::MAX))
            .await
            .unwrap();
        invoices.set_invoice(peer, "mock-invoice").await;

        let outcomes = service.run_once(2).await.unwrap();
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(&outcomes[0], SettlementOutcome::Failed { .. }));
    }

    #[tokio::test]
    async fn two_peers_are_settled_independently() {
        let (service, peer_directory, usage_tally, rail, invoices) = make_service().await;
        let a = PeerId::from([6u8; 32]);
        let b = PeerId::from([7u8; 32]);
        peer_directory
            .add_trusted_peer(a, TrustScope::Circle)
            .await
            .unwrap();
        peer_directory
            .add_trusted_peer(b, TrustScope::Circle)
            .await
            .unwrap();
        // Must clear the 1-sat floor to actually settle.
        usage_tally
            .record_borrowed(a, TokenCount::new(2000))
            .await
            .unwrap();
        usage_tally
            .record_borrowed(b, TokenCount::new(4000))
            .await
            .unwrap();
        let invoice_a = rail.issue_invoice(Millisats::new(2000)).await.unwrap();
        let invoice_b = rail.issue_invoice(Millisats::new(4000)).await.unwrap();
        invoices.set_invoice(a, invoice_a).await;
        invoices.set_invoice(b, invoice_b).await;

        let outcomes = service.run_once(1).await.unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes
            .iter()
            .all(|o| matches!(o, SettlementOutcome::Settled { .. })));
    }
}
