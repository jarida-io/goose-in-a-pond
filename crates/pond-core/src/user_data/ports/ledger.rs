//! Where the Budget specialist's ledger is kept.

use anyhow::Result;
use async_trait::async_trait;
use chrono::NaiveDate;

use crate::user_data::domain::ledger::LedgerEntry;

/// Every read and write is one member's.
#[async_trait]
pub trait LedgerRepository: Send + Sync {
    /// Stores the entries together: all of them or none.
    async fn add(&self, entries: &[LedgerEntry]) -> Result<()>;

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<LedgerEntry>>;

    /// The member's entries from `from` up to, not including, `until`, newest day first.
    async fn list(
        &self,
        profile_id: &str,
        from: NaiveDate,
        until: NaiveDate,
    ) -> Result<Vec<LedgerEntry>>;

    /// Writes every field of a stored entry; `false` when its member has none by that id.
    async fn save(&self, entry: &LedgerEntry) -> Result<bool>;

    /// `false` when the member has no entry by that id.
    async fn delete(&self, profile_id: &str, id: &str) -> Result<bool>;
}
