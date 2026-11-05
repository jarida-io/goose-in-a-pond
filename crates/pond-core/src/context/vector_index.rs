//! The vector index, keyed by item (context-space.md section 4.3). It holds no text: WAL can't
//! delete an item and its vector atomically, so orphans hold nothing.

use anyhow::Result;
use async_trait::async_trait;

use crate::items::Kind;
use crate::user_data::domain::profile::ProfileScope;

/// The kinds the index holds, in the order the embedding slot walks them: memory first, since
/// it is what recall reads first. Suggestions, reports and messages are not searched yet.
pub const INDEXED: [Kind; 8] = [
    Kind::Fact,
    Kind::Trait,
    Kind::Entity,
    Kind::Conversation,
    Kind::Mail,
    Kind::Event,
    Kind::Notification,
    Kind::HomeEvent,
];

/// The kinds memory search reads.
pub const MEMORY: [Kind; 3] = [Kind::Fact, Kind::Trait, Kind::Entity];

/// The kinds context search reads: what ingest stores in `context_items`.
pub const CONTEXT: [Kind; 4] = [Kind::Mail, Kind::Event, Kind::Notification, Kind::HomeEvent];

/// One vector on its way into the index.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorEntry {
    pub item_id: String,
    /// Passage index, 0 when whole; part of the `(item_id, chunk_ix)` identity.
    pub chunk_ix: i64,
    /// Byte span of the passage in the item's embedded text, `None` for all of it.
    pub chunk_span: Option<(i64, i64)>,
    /// The embedder that produced `vector`. Never inferred at read time.
    pub model_id: String,
    pub vector: Vec<f32>,
    /// The header's `updated_at` the vector was made from; another one means the text moved.
    pub source_rev: Option<String>,
}

impl VectorEntry {
    /// A vector over an item's whole text; chunked items build entries directly.
    pub fn whole(
        item_id: String,
        model_id: String,
        vector: Vec<f32>,
        source_rev: Option<String>,
    ) -> Self {
        Self {
            item_id,
            chunk_ix: 0,
            chunk_span: None,
            model_id,
            vector,
            source_rev,
        }
    }
}

/// One retrieval result: identity and score, never content.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorHit {
    pub item_id: String,
    pub kind: Kind,
    /// Cosine similarity against the query, in `[-1, 1]`.
    pub score: f32,
}

/// A hit with its text read from the live header, via the scope-enforcing `JOIN`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedHit {
    pub item_id: String,
    pub kind: Kind,
    pub score: f32,
    /// The best-matching passage of the item's embedded text.
    pub text: String,
}

/// An item whose vector is missing, from another model, or older than its header.
#[derive(Debug, Clone, PartialEq)]
pub struct Stale {
    pub item_id: String,
    pub kind: Kind,
    /// What [`crate::items::embedded_text`] makes of the header.
    pub text: String,
    /// The header's `updated_at`, read with the text, for [`VectorEntry::source_rev`].
    pub rev: String,
}

/// How much of one kind is usable for a given model; totals would average a dead one away.
#[derive(Debug, Clone, PartialEq)]
pub struct KindHealth {
    pub kind: Kind,
    /// Items retrieval can return (active, with text): the coverage denominator.
    pub rows: u64,
    /// Every header of the kind; `rows == 0 && source_rows > 0` is the structural failure.
    pub source_rows: u64,
    /// Of `rows`, those with a vector from the model asked about: all retrieval can return.
    pub indexed_rows: u64,
    /// Of `rows`, those with no vector at all. Repaired by embedding.
    pub missing_rows: u64,
    /// Of `rows`, those with another model's vector: excluded from retrieval until re-embedded.
    pub mismatched: u64,
}

/// Index usability for a model; totals sum `per_kind`, orphans excluded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexHealth {
    pub matching: u64,
    pub mismatched: u64,
    pub missing: u64,
    /// One row per kind, in [`INDEXED`] order.
    pub per_kind: Vec<KindHealth>,
}

/// Driven port: the item-keyed vector index; reads filter scope against live headers via `JOIN`.
#[async_trait]
pub trait VectorIndex: Send + Sync {
    /// Insert or replace a vector: a header's text is rewritten in place, so appends go stale.
    async fn upsert(&self, entry: &VectorEntry) -> Result<()>;

    /// Drop an item's vectors; best-effort, with [`Self::prune_orphans`] reconciling.
    async fn remove(&self, item_id: &str) -> Result<()>;

    /// An item's first vector, e.g. to verify a write or hand a caller the vector it scored.
    async fn get(&self, item_id: &str) -> Result<Option<VectorEntry>>;

    /// Top-`limit` hits among `kinds`, from `model_id` only and within what `scope` may see.
    /// Cross-model cosine is meaningless; scope is applied in SQL so hidden items take no slots.
    async fn search(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        kinds: &[Kind],
        limit: usize,
    ) -> Result<Vec<VectorHit>>;

    /// Like [`Self::search`], reading each hit's live text in the same query to avoid races.
    async fn search_resolved(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        kinds: &[Kind],
        limit: usize,
    ) -> Result<Vec<ResolvedHit>>;

    /// Items among `kinds` with no current vector for `model_id`, found by `LEFT JOIN`, in
    /// `kinds` order.
    async fn needs_embedding(
        &self,
        kinds: &[Kind],
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<Stale>>;

    /// Delete vectors whose item is gone. Returns how many.
    async fn prune_orphans(&self) -> Result<u64>;

    /// Health counts, global and per indexed kind; every kind appears, even with no items.
    async fn health(&self, model_id: &str) -> Result<IndexHealth>;
}
