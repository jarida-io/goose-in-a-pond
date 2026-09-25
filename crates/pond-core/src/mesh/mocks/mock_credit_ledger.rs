use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::mesh::domain::millisats::Millisats;
use crate::mesh::domain::peer_id::PeerId;
use crate::mesh::ports::credit_ledger::{CreditLedger, CreditLedgerError};

/// In-memory credit ledger; a peer with no activity has a zero balance, not a missing entry.
pub struct MockCreditLedger {
    balances: Arc<RwLock<HashMap<PeerId, Millisats>>>,
}

impl MockCreditLedger {
    pub fn new() -> Self {
        Self {
            balances: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for MockCreditLedger {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CreditLedger for MockCreditLedger {
    async fn balance(&self, peer: PeerId) -> Result<Millisats, CreditLedgerError> {
        Ok(self
            .balances
            .read()
            .await
            .get(&peer)
            .copied()
            .unwrap_or(Millisats::new(0)))
    }

    async fn credit(&self, peer: PeerId, amount: Millisats) -> Result<(), CreditLedgerError> {
        let mut balances = self.balances.write().await;
        let current = balances.get(&peer).copied().unwrap_or(Millisats::new(0));
        let updated = current
            .checked_add(amount)
            .ok_or_else(|| CreditLedgerError::General("credit overflow".to_string()))?;
        balances.insert(peer, updated);
        Ok(())
    }

    async fn debit(&self, peer: PeerId, amount: Millisats) -> Result<(), CreditLedgerError> {
        let mut balances = self.balances.write().await;
        let current = balances.get(&peer).copied().unwrap_or(Millisats::new(0));
        let updated =
            current
                .checked_sub(amount)
                .ok_or(CreditLedgerError::InsufficientBalance {
                    peer,
                    have: current,
                    need: amount,
                })?;
        balances.insert(peer, updated);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    #[tokio::test]
    async fn trait_object_conformance() {
        let _: StdArc<dyn CreditLedger> = StdArc::new(MockCreditLedger::new());
    }

    #[tokio::test]
    async fn unknown_peer_has_zero_balance() {
        let ledger = MockCreditLedger::new();
        let peer = PeerId::from([1u8; 32]);
        assert_eq!(ledger.balance(peer).await.unwrap(), Millisats::new(0));
    }

    #[tokio::test]
    async fn credit_then_debit_nets_to_zero() {
        let ledger = MockCreditLedger::new();
        let peer = PeerId::from([2u8; 32]);
        ledger.credit(peer, Millisats::new(1000)).await.unwrap();
        ledger.debit(peer, Millisats::new(1000)).await.unwrap();
        assert_eq!(ledger.balance(peer).await.unwrap(), Millisats::new(0));
    }

    #[tokio::test]
    async fn debit_past_balance_is_typed_error_not_panic() {
        let ledger = MockCreditLedger::new();
        let peer = PeerId::from([3u8; 32]);
        ledger.credit(peer, Millisats::new(100)).await.unwrap();

        let result = ledger.debit(peer, Millisats::new(101)).await;
        assert!(matches!(
            result,
            Err(CreditLedgerError::InsufficientBalance { .. })
        ));
        // Balance is unchanged after a failed debit.
        assert_eq!(ledger.balance(peer).await.unwrap(), Millisats::new(100));
    }

    #[tokio::test]
    async fn debit_from_zero_balance_is_typed_error() {
        let ledger = MockCreditLedger::new();
        let peer = PeerId::from([4u8; 32]);
        let result = ledger.debit(peer, Millisats::new(1)).await;
        assert!(matches!(
            result,
            Err(CreditLedgerError::InsufficientBalance { .. })
        ));
    }

    #[tokio::test]
    async fn balances_are_isolated_per_peer() {
        let ledger = MockCreditLedger::new();
        let a = PeerId::from([5u8; 32]);
        let b = PeerId::from([6u8; 32]);
        ledger.credit(a, Millisats::new(500)).await.unwrap();
        assert_eq!(ledger.balance(b).await.unwrap(), Millisats::new(0));
        assert_eq!(ledger.balance(a).await.unwrap(), Millisats::new(500));
    }
}
