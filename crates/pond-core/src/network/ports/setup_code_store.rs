use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::network::domain::setup_record::SetupRecord;

/// Where a pond's printed setup code and its claim latch live. The store, not the core, draws a
/// new code: pond-core has no RNG.
#[async_trait]
pub trait SetupCodeStore: Send + Sync {
    /// `Ok(None)` only when no code was ever made. An unreadable or corrupt record is an error,
    /// never `None`, or the next `create_if_missing` would void the printed card.
    async fn load(&self) -> Result<Option<SetupRecord>>;

    /// Idempotent: an existing record comes back unchanged, so a re-run never voids a card.
    async fn create_if_missing(&self) -> Result<SetupRecord>;

    /// Idempotent and keeps the first `claimed_at` and `claimed_by`: `by` is written only by the
    /// claim that latches. An error when there is no record.
    async fn mark_claimed(&self, at: DateTime<Utc>, by: Option<&str>) -> Result<SetupRecord>;

    /// Clears `claimed_at` and `claimed_by` and keeps the code: factory reset re-arms the same
    /// printed card.
    async fn rearm(&self) -> Result<SetupRecord>;
}
