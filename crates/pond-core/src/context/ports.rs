//! Driven port: storage for personal context. Reads take a scope so a `Guest` sees no items.
//! Avoid default bodies: a decorator that forgets one still compiles and drops the call.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::context::domain::{ContextItem, ContextSource};
use crate::context::retention::ContextRetention;
use crate::user_data::domain::profile::ProfileScope;

#[async_trait]
pub trait ContextRepository: Send + Sync {
    /// Create or update a source; `kind` and `profile_id` never change (migration 0044 refuses).
    async fn upsert_source(&self, source: &ContextSource) -> Result<()>;

    /// One source, if this scope may see it.
    async fn get_source(&self, id: &str, scope: &ProfileScope) -> Result<Option<ContextSource>>;

    /// Every source this scope may see.
    async fn list_sources(&self, scope: &ProfileScope) -> Result<Vec<ContextSource>>;

    /// Disconnect a source and delete its items, returning the count so callers can report it.
    async fn disconnect_source(&self, id: &str, scope: &ProfileScope) -> Result<u64>;

    /// Store an item, idempotently on `(source_id, external_id)`: cursors slip backwards.
    async fn save_item(&self, item: &ContextItem) -> Result<()>;

    /// The newest items this scope may see.
    async fn recent_items(&self, scope: &ProfileScope, limit: usize) -> Result<Vec<ContextItem>>;

    /// Keyword fallback, for when no embedding provider is wired.
    async fn search_items(
        &self,
        keywords: &[String],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<ContextItem>>;

    /// Semantic search, with each item's cosine similarity for the caller's own ranking blend.
    async fn search_similar(
        &self,
        query_embedding: &[f32],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<(ContextItem, f32)>>;

    /// Items with no vector yet, for the background embedding backfill.
    async fn search_unembedded(&self, limit: usize) -> Result<Vec<ContextItem>>;

    /// Attach a vector to a stored item.
    async fn update_embedding(&self, id: &str, embedding: &[f32]) -> Result<()>;

    /// How many items a member owns. Used to say what deleting them removes.
    async fn count_for_profile(&self, profile_id: &str) -> Result<u64>;

    /// Per-source counts in one query. Defaulted: empty beats failing the whole sources list.
    async fn item_stats_by_source(&self) -> Result<Vec<SourceItemStats>> {
        Ok(Vec::new())
    }

    /// Delete everything past its retention window, which depends on kind and sensitivity.
    async fn purge_expired(&self, retention: &ContextRetention, now: DateTime<Utc>) -> Result<u64>;
}

// ── Asking an account for news, now ─────────────────────────────────────────

/// What one sync pass did, as counts a person can be told.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountSyncSummary {
    /// Accounts considered.
    pub sources: usize,
    /// Accounts whose upstream said nothing had changed.
    pub unchanged: usize,
    /// Items stored.
    pub ingested: usize,
    /// Accounts whose credentials were refused.
    pub needs_reauth: usize,
    /// Accounts that failed for some other reason.
    pub failed: usize,
    /// Accounts skipped because the pond is offline.
    pub paused: usize,
    /// What each account did, named.
    #[serde(default)]
    pub per_source: Vec<SourceSyncOutcome>,
}

/// One account's result from a sync pass.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceSyncOutcome {
    pub source_id: String,
    /// `google`, `fastmail`, `icloud`, … — as stored.
    pub provider: String,
    /// `calendar` or `mail`.
    pub kind: String,
    /// `ingested` | `unchanged` | `needs_reauth` | `failed` | `paused`
    pub outcome: String,
    /// Items stored from this account in this pass.
    pub ingested: usize,
}

/// Pull every connected account now, rather than waiting for the timer.
#[async_trait]
pub trait AccountSync: Send + Sync {
    async fn sync_now(&self) -> Result<AccountSyncSummary>;
}

// ── What each source has actually produced ──────────────────────────────────

/// One source's contribution to the corpus, and how much of it is searchable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceItemStats {
    pub source_id: String,
    /// Everything stored from this source.
    pub items: u64,
    /// Of those, the ones still waiting for a vector.
    pub awaiting_index: u64,
}
