//! The personal-context vector index over three corpora that keep their own stores.
//! It holds no text: WAL can't delete a row and its vector atomically, so orphans hold nothing.

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::profile::ProfileScope;

/// Which corpus a vector belongs to, so retrieval can label provenance.
/// Declaration order is the tie-break policy (derived `Ord`); a test pins it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Corpus {
    /// `memory_fragments` — things extracted from conversation.
    Memory,
    /// `context_items`: things ingested from a source, redacted before storage.
    Context,
    /// `sessions.rolling_summary` — a model's compression of a conversation.
    Summary,
}

impl Corpus {
    pub fn as_str(self) -> &'static str {
        match self {
            Corpus::Memory => "memory",
            Corpus::Context => "context",
            Corpus::Summary => "summary",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "memory" => Some(Corpus::Memory),
            "context" => Some(Corpus::Context),
            "summary" => Some(Corpus::Summary),
            _ => None,
        }
    }

    /// Every corpus, so a sweep cannot silently forget one.
    pub const ALL: [Corpus; 3] = [Corpus::Memory, Corpus::Context, Corpus::Summary];
}

/// One vector on its way into the index.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorEntry {
    pub corpus: Corpus,
    /// The source row's primary key in its own database.
    pub row_id: String,
    /// Passage index, 0 when whole; part of the `(corpus, row_id, chunk_ix)` identity.
    pub chunk_ix: i64,
    /// Byte span of the passage in the source text, `None` for all of it; never the words.
    pub chunk_span: Option<(i64, i64)>,
    /// The embedder that produced `vector`. Never inferred at read time.
    pub model_id: String,
    pub vector: Vec<f32>,
    /// Freshness marker from the source row; tells a sweep a rewritten summary's vector is stale.
    pub source_rev: Option<String>,
}

impl VectorEntry {
    /// A vector over a row's whole text; chunked corpora build entries directly.
    pub fn whole(
        corpus: Corpus,
        row_id: String,
        model_id: String,
        vector: Vec<f32>,
        source_rev: Option<String>,
    ) -> Self {
        Self {
            corpus,
            row_id,
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
    pub corpus: Corpus,
    pub row_id: String,
    /// Cosine similarity against the query, in `[-1, 1]`.
    pub score: f32,
}

/// A hit with its text read from the live source row, via the scope-enforcing `JOIN`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedHit {
    pub corpus: Corpus,
    pub row_id: String,
    pub score: f32,
    /// The live text: memory content, a context item's title and body, or the summary.
    pub text: String,
}

/// How much of one corpus is usable for a given model; totals would average a dead one away.
#[derive(Debug, Clone, PartialEq)]
pub struct CorpusHealth {
    pub corpus: Corpus,
    /// Source rows passing the liveness predicate: the coverage denominator, not the table count.
    pub rows: u64,
    /// Every row in the source table; `rows == 0 && source_rows > 0` is the structural failure.
    pub source_rows: u64,
    /// Of `rows`, those with a vector from the model asked about: all retrieval can return.
    pub indexed_rows: u64,
    /// Of `rows`, those with no vector at all. Repaired by embedding.
    pub missing_rows: u64,
    /// Of `rows`, those with another model's vector: excluded from retrieval until re-embedded.
    pub mismatched: u64,
}

/// Index usability for a model; totals sum `per_corpus` over qualifying rows, orphans excluded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IndexHealth {
    /// Vectors produced by the model currently configured.
    pub matching: u64,
    /// Vectors from some other model, unusable until re-embedded.
    pub mismatched: u64,
    /// Rows in the source stores with no vector at all.
    pub missing: u64,
    /// The same counts, one row per corpus, in [`Corpus::ALL`] order.
    pub per_corpus: Vec<CorpusHealth>,
}

/// Driven port: the shared vector index; reads filter scope against live rows via `JOIN`.
#[async_trait]
pub trait VectorIndex: Send + Sync {
    /// Insert or replace a vector: summaries are rewritten in place, so appends would go stale.
    async fn upsert(&self, entry: &VectorEntry) -> Result<()>;

    /// Drop a deleted row's vectors; best-effort, with [`Self::prune_orphans`] reconciling.
    async fn remove(&self, corpus: Corpus, row_id: &str) -> Result<()>;

    /// Read one vector back, e.g. to verify a write without a full search.
    async fn get(&self, corpus: Corpus, row_id: &str) -> Result<Option<VectorEntry>>;

    /// Top-`limit` hits for `query`, restricted to `model_id` and to what `scope` may see.
    /// Cross-model cosine is meaningless; scope is applied in SQL so hidden rows take no slots.
    async fn search(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<VectorHit>>;

    /// Like [`Self::search`], reading each hit's live text in the same query to avoid races.
    async fn search_resolved(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<ResolvedHit>>;

    /// Rows with no current vector for `model_id`, found by `LEFT JOIN`, not a durable queue.
    async fn needs_embedding(
        &self,
        corpus: Corpus,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<String>>;

    /// Like [`Self::needs_embedding`], with each row's text: context items' only embedding path.
    async fn needs_embedding_with_text(
        &self,
        corpus: Corpus,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>>;

    /// Copy source-store vectors in without re-embedding, making the index rebuildable.
    /// `expected_dims` filters: another width came from another model.
    async fn backfill_from_source(
        &self,
        corpus: Corpus,
        model_id: &str,
        expected_dims: usize,
    ) -> Result<u64>;

    /// Delete index rows whose source row is gone. Returns how many.
    async fn prune_orphans(&self) -> Result<u64>;

    /// Health counts, global and per corpus; every corpus must appear, even with no rows.
    async fn health(&self, model_id: &str) -> Result<IndexHealth>;
}
