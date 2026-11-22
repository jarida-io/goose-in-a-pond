//! The personal-context vector index over corpora that keep their own stores.
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
    /// A member's profile, places, confirmed interests, fact lines, don't rules, answers,
    /// anchors, deliverables, saved items, captures and screenshot notes, each row id prefixed
    /// with its table.
    Personal,
    /// `session_messages`: what a member said and what the pond replied.
    Message,
    /// `news_digests`.
    Digest,
    /// `news_stories`.
    Story,
    /// `long_task_artifacts` other than raw data.
    Report,
    /// `notification_history`.
    Notice,
    /// `ideas` not dismissed.
    Idea,
}

impl Corpus {
    pub fn as_str(self) -> &'static str {
        match self {
            Corpus::Memory => "memory",
            Corpus::Context => "context",
            Corpus::Summary => "summary",
            Corpus::Personal => "personal",
            Corpus::Message => "message",
            Corpus::Digest => "digest",
            Corpus::Story => "story",
            Corpus::Report => "report",
            Corpus::Notice => "notice",
            Corpus::Idea => "idea",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|corpus| corpus.as_str() == raw)
    }

    /// Every corpus, so a sweep cannot silently forget one.
    pub const ALL: [Corpus; 10] = [
        Corpus::Memory,
        Corpus::Context,
        Corpus::Summary,
        Corpus::Personal,
        Corpus::Message,
        Corpus::Digest,
        Corpus::Story,
        Corpus::Report,
        Corpus::Notice,
        Corpus::Idea,
    ];

    /// Whether long rows are split into passages; memories and summaries are a few sentences.
    pub fn chunked(self) -> bool {
        !matches!(self, Corpus::Memory | Corpus::Summary)
    }
}

/// What a turn's own retrieval leaves out: corpora it already shows, and its own conversation,
/// whose words are in front of the model already.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Exclusions {
    pub corpora: Vec<Corpus>,
    pub session_id: Option<String>,
}

/// A row the embedding slot owes a vector: its id, text, and the revision the vector is stamped
/// with, so an edit makes it stale.
#[derive(Debug, Clone, PartialEq)]
pub struct StaleRow {
    pub row_id: String,
    pub text: String,
    pub rev: Option<String>,
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
    ) -> Result<Vec<ResolvedHit>> {
        self.search_resolved_excluding(query, model_id, scope, limit, &Exclusions::default())
            .await
    }

    /// [`Self::search_resolved`] without what `exclusions` names, left out in SQL so it takes no
    /// top-K slot.
    async fn search_resolved_excluding(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        limit: usize,
        exclusions: &Exclusions,
    ) -> Result<Vec<ResolvedHit>>;

    /// Rows with no current vector for `model_id`, found by `LEFT JOIN`, not a durable queue.
    async fn needs_embedding(
        &self,
        corpus: Corpus,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<String>>;

    /// Like [`Self::needs_embedding`], with each row's text and revision, for the embedding slot.
    async fn needs_embedding_with_text(
        &self,
        corpus: Corpus,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<StaleRow>>;

    /// Delete index rows whose source row is gone. Returns how many.
    async fn prune_orphans(&self) -> Result<u64>;

    /// Health counts, global and per corpus; every corpus must appear, even with no rows.
    async fn health(&self, model_id: &str) -> Result<IndexHealth>;
}
