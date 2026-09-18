//! Driven port: storage for personal context (PAI-8 P1). Methods avoid default bodies, because a
//! decorator that forgets a defaulted method compiles, answers `Ok(0)`, and the row never reaches
//! SQLite. Every read takes a scope, `get` included: PAI-8 invariant 2 is that a `Guest` session
//! sees no context items at all, and a read without a scope could not honour it.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::context::domain::{ContextItem, ContextSource, SourceKind};
use crate::context::retention::ContextRetention;
use crate::user_data::domain::profile::ProfileScope;

#[async_trait]
pub trait ContextRepository: Send + Sync {
    /// Create or update a source.
    ///
    /// `kind` and `profile_id` are identity and must not change on conflict; migration 0044
    /// refuses such an update, since a source whose owner moved misattributes every stored item.
    async fn upsert_source(&self, source: &ContextSource) -> Result<()>;

    /// One source, if this scope may see it.
    async fn get_source(&self, id: &str, scope: &ProfileScope) -> Result<Option<ContextSource>>;

    /// Every source this scope may see.
    async fn list_sources(&self, scope: &ProfileScope) -> Result<Vec<ContextSource>>;

    /// Disconnect a source and delete its items. Returns how many items went with it.
    ///
    /// PAI-8 invariant 6 requires saying how many, so the count is the return value and not a log
    /// line: a caller that cannot report the number cannot satisfy the invariant.
    async fn disconnect_source(&self, id: &str, scope: &ProfileScope) -> Result<u64>;

    /// Store an item, idempotently on `(source_id, external_id)`.
    ///
    /// A cursor slipping backwards is normal, so a re-sync must update the row rather than fill
    /// the corpus with duplicates of the same messages.
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

    /// Semantic search. Returns each item with its cosine similarity, so the
    /// caller can rank on the same blend memory uses rather than on similarity
    /// alone.
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

    /// How many items of one source kind fall inside a time window.
    ///
    /// A COUNT, deliberately, not a `LIMIT`-bounded fetch. The suggestion
    /// engine quotes this number to the household ("three events between now
    /// and midnight"), and a count derived from a capped read would be wrong in
    /// exactly the case that matters -- a busy day -- while looking right on a
    /// quiet one.
    ///
    /// `recent_items` cannot stand in for it either: it is `ORDER BY
    /// occurred_at DESC`, and the CalDAV adapter stores DTSTART in
    /// `occurred_at` over a window reaching ninety days forward, so the
    /// furthest-future event sorts first and today's is unreachable behind any
    /// limit.
    ///
    /// No default body, per this module's header: a defaulted read answers
    /// `Ok(0)` through any decorator that forgets it, and a zero here is
    /// indistinguishable from an empty day -- the engine would go quiet and
    /// nothing would say why.
    async fn count_in_window(
        &self,
        scope: &ProfileScope,
        kind: SourceKind,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<u64>;

    /// What each source has contributed, and how much of it is searchable.
    ///
    /// One query for all sources, not one per source. Defaults to empty so an adapter without it
    /// reports "nothing known" rather than failing the whole sources list.
    async fn item_stats_by_source(&self) -> Result<Vec<SourceItemStats>> {
        Ok(Vec::new())
    }

    /// Delete everything past its retention window. Returns how many rows went.
    ///
    /// Takes the whole [`ContextRetention`] because the window depends on both source kind and
    /// sensitivity; flattening those to one day count keeps sensitive items for the baseline.
    async fn purge_expired(&self, retention: &ContextRetention, now: DateTime<Utc>) -> Result<u64>;
}

// ── Asking an account for news, now ─────────────────────────────────────────

/// What one sync pass did, in the shape a person can be told.
///
/// Counts rather than a bare success flag: "checked, nothing new" and "checked, found eleven
/// things" are both successes, and whoever pressed the button needs to know which happened.
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
    ///
    /// The totals above answer "did anything happen"; this answers "where from", which is what
    /// somebody with two calendars and a mailbox needs to know.
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
///
/// A port because the sync composes a repository, a pipeline, a secret store and one adapter per
/// kind — wiring that belongs to the binary, not to a domain that has never heard of CalDAV.
#[async_trait]
pub trait AccountSync: Send + Sync {
    async fn sync_now(&self) -> Result<AccountSyncSummary>;
}

// ── What each source has actually produced ──────────────────────────────────

/// One source's contribution to the corpus, and how much of it is searchable.
///
/// Two numbers, not one: a source can be perfectly connected and still be half-invisible while
/// the index catches up, and that gap is what explains a search that comes up short.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceItemStats {
    pub source_id: String,
    /// Everything stored from this source.
    pub items: u64,
    /// Of those, the ones still waiting for a vector.
    pub awaiting_index: u64,
}
