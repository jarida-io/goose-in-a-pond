//! SQLite-backed implementation of the CreditLedger port.

use async_trait::async_trait;
use pond_core::mesh::domain::millisats::Millisats;
use pond_core::mesh::domain::peer_id::PeerId;
use pond_core::mesh::ports::credit_ledger::{CreditLedger, CreditLedgerError};
use sqlx::{Pool, Sqlite};

pub struct SqliteCreditLedger {
    pool: Pool<Sqlite>,
}

impl SqliteCreditLedger {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CreditLedger for SqliteCreditLedger {
    async fn balance(&self, peer: PeerId) -> Result<Millisats, CreditLedgerError> {
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT balance_millisats FROM mesh_credit_balances WHERE peer_id = ?")
                .bind(peer.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| CreditLedgerError::General(e.to_string()))?;
        Ok(Millisats::new(row.map(|(b,)| b as u64).unwrap_or(0)))
    }

    async fn credit(&self, peer: PeerId, amount: Millisats) -> Result<(), CreditLedgerError> {
        sqlx::query(
            "INSERT INTO mesh_credit_balances (peer_id, balance_millisats, updated_at) \
             VALUES (?, ?, datetime('now')) \
             ON CONFLICT(peer_id) DO UPDATE SET \
                balance_millisats = balance_millisats + excluded.balance_millisats, \
                updated_at = datetime('now')",
        )
        .bind(peer.to_string())
        .bind(amount.value() as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| CreditLedgerError::General(e.to_string()))?;
        Ok(())
    }

    async fn debit(&self, peer: PeerId, amount: Millisats) -> Result<(), CreditLedgerError> {
        // The `>= ?` guard lives in this one UPDATE so concurrent debits cannot overdraw.
        let result = sqlx::query(
            "UPDATE mesh_credit_balances \
             SET balance_millisats = balance_millisats - ?, updated_at = datetime('now') \
             WHERE peer_id = ? AND balance_millisats >= ?",
        )
        .bind(amount.value() as i64)
        .bind(peer.to_string())
        .bind(amount.value() as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| CreditLedgerError::General(e.to_string()))?;

        if result.rows_affected() == 0 {
            let have = self.balance(peer).await?;
            return Err(CreditLedgerError::InsufficientBalance {
                peer,
                have,
                need: amount,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::sync::Arc;
    use tempfile::tempdir;

    async fn make_ledger() -> (SqliteCreditLedger, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        (SqliteCreditLedger::new(db.system), tmp)
    }

    #[tokio::test]
    async fn trait_object_conformance() {
        let (ledger, _tmp) = make_ledger().await;
        let _: Arc<dyn CreditLedger> = Arc::new(ledger);
    }

    #[tokio::test]
    async fn unknown_peer_has_zero_balance() {
        let (ledger, _tmp) = make_ledger().await;
        let peer = PeerId::from([1u8; 32]);
        assert_eq!(ledger.balance(peer).await.unwrap(), Millisats::new(0));
    }

    #[tokio::test]
    async fn credit_then_debit_nets_to_zero() {
        let (ledger, _tmp) = make_ledger().await;
        let peer = PeerId::from([2u8; 32]);
        ledger.credit(peer, Millisats::new(1000)).await.unwrap();
        ledger.debit(peer, Millisats::new(1000)).await.unwrap();
        assert_eq!(ledger.balance(peer).await.unwrap(), Millisats::new(0));
    }

    #[tokio::test]
    async fn debit_past_balance_is_typed_error_not_panic() {
        let (ledger, _tmp) = make_ledger().await;
        let peer = PeerId::from([3u8; 32]);
        ledger.credit(peer, Millisats::new(100)).await.unwrap();

        let result = ledger.debit(peer, Millisats::new(101)).await;
        assert!(matches!(
            result,
            Err(CreditLedgerError::InsufficientBalance { .. })
        ));
        assert_eq!(ledger.balance(peer).await.unwrap(), Millisats::new(100));
    }

    #[tokio::test]
    async fn debit_from_zero_balance_is_typed_error() {
        let (ledger, _tmp) = make_ledger().await;
        let peer = PeerId::from([4u8; 32]);
        let result = ledger.debit(peer, Millisats::new(1)).await;
        assert!(matches!(
            result,
            Err(CreditLedgerError::InsufficientBalance { .. })
        ));
    }

    #[tokio::test]
    async fn balances_are_isolated_per_peer() {
        let (ledger, _tmp) = make_ledger().await;
        let a = PeerId::from([5u8; 32]);
        let b = PeerId::from([6u8; 32]);
        ledger.credit(a, Millisats::new(500)).await.unwrap();
        assert_eq!(ledger.balance(b).await.unwrap(), Millisats::new(0));
        assert_eq!(ledger.balance(a).await.unwrap(), Millisats::new(500));
    }

    #[tokio::test]
    async fn concurrent_debits_never_overdraw() {
        let (ledger, _tmp) = make_ledger().await;
        let ledger = Arc::new(ledger);
        let peer = PeerId::from([7u8; 32]);
        let starting_balance = 30u64;
        let attempts = 50u64;
        ledger
            .credit(peer, Millisats::new(starting_balance))
            .await
            .unwrap();

        let mut handles = Vec::new();
        for _ in 0..attempts {
            let ledger = ledger.clone();
            handles.push(tokio::spawn(async move {
                ledger.debit(peer, Millisats::new(1)).await
            }));
        }

        let mut succeeded = 0u64;
        let mut failed = 0u64;
        for handle in handles {
            match handle.await.unwrap() {
                Ok(()) => succeeded += 1,
                Err(CreditLedgerError::InsufficientBalance { .. }) => failed += 1,
                Err(other) => panic!("unexpected error: {other}"),
            }
        }

        assert_eq!(succeeded, starting_balance);
        assert_eq!(failed, attempts - starting_balance);
        assert_eq!(ledger.balance(peer).await.unwrap(), Millisats::new(0));
    }
}
