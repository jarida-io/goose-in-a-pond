//! The embedding slot keeps every corpus's vectors current for the chosen model, in counter-phase.
//! Work is found by the index's staleness join, so there is no queue and a restart resumes.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::context::chunking::{self, Chunk};
use crate::context::ports::ContextRepository;
use crate::context::vector_index::{Corpus, VectorEntry, VectorIndex};
use crate::models::ports::embedding::EmbeddingProvider as _;
use crate::models::services::active_embedder::{ActiveEmbedder, EmbedLease};
use crate::user_data::ports::memory_repository::MemoryRepository;
use crate::user_data::ports::session_storage::SessionStorage;

/// How long one batch may take, so a returning turn waits at most this; a target from the design,
/// not yet measured against the slot on the Orin.
pub const BATCH_BUDGET: Duration = Duration::from_millis(250);

/// Rows read per staleness query; a choice, small so a row rewritten before its embed is rare.
const ROWS_PER_READ: usize = 32;

/// A rough English figure, not a measurement: the sizer learns time per estimated token, so the
/// scale cancels out.
const BYTES_PER_TOKEN: usize = 4;

/// Weight of the newest batch in the speed estimate; a choice.
const SPEED_SMOOTHING: f64 = 0.3;

/// The order corpora are walked in: memory is what recall reads first.
const ORDER: [Corpus; 3] = [Corpus::Memory, Corpus::Summary, Corpus::Context];

pub fn estimate_tokens(text: &str) -> usize {
    (text.len() / BYTES_PER_TOKEN).max(1)
}

/// Packs batches to a time budget from the speed it has seen. Starts at one item: until a batch
/// is timed, nothing says how long a longer text takes on this device.
#[derive(Debug, Clone)]
pub struct BatchSizer {
    budget: Duration,
    /// Smoothed milliseconds per estimated token; `None` until a batch has been timed.
    ms_per_token: Option<f64>,
}

impl BatchSizer {
    pub fn new(budget: Duration) -> Self {
        Self {
            budget,
            ms_per_token: None,
        }
    }

    /// How many of the next items, given each one's token estimate in order, fit the budget:
    /// at least one, at most `cap`.
    pub fn take(&self, tokens: &[usize], cap: usize) -> usize {
        let cap = cap.max(1).min(tokens.len());
        if cap == 0 {
            return 0;
        }
        let Some(rate) = self.ms_per_token else {
            return 1;
        };
        let budget_ms = self.budget.as_secs_f64() * 1000.0;
        let mut spent = 0.0;
        let mut n = 0;
        for &t in &tokens[..cap] {
            let cost = t.max(1) as f64 * rate;
            if n > 0 && spent + cost > budget_ms {
                break;
            }
            spent += cost;
            n += 1;
        }
        n
    }

    pub fn record(&mut self, tokens: usize, took: Duration) {
        if tokens == 0 {
            return;
        }
        let observed = took.as_secs_f64() * 1000.0 / tokens as f64;
        self.ms_per_token = Some(match self.ms_per_token {
            None => observed,
            Some(prev) => prev + SPEED_SMOOTHING * (observed - prev),
        });
    }
}

/// Asked between batches; the runner answers from the lane and the household's activity.
#[async_trait]
pub trait SlotControl: Send + Sync {
    /// False once a turn, a compaction, a due long-task step or a returning person needs the device.
    async fn may_continue(&self) -> bool;
}

/// The stores a pass reads and writes.
pub struct SlotStores<'a> {
    pub index: &'a dyn VectorIndex,
    pub memories: &'a dyn MemoryRepository,
    pub sessions: &'a dyn SessionStorage,
    /// `None` where no context store is wired; that corpus is then left alone.
    pub context: Option<&'a dyn ContextRepository>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassEnd {
    /// Nothing is left that this model can embed.
    Drained,
    /// Something the household waits on needed the device.
    Yielded,
    /// The household chose another model during the pass.
    ModelChanged,
    /// No model is loaded.
    NotReady,
    /// The index could not be read.
    StoreFailed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PassReport {
    pub end: PassEnd,
    pub memories: usize,
    pub summaries: usize,
    pub context_items: usize,
    /// Rows that could not be embedded or stored; each is tried again after a switch or a bell.
    pub failed: usize,
    pub batches: usize,
    pub orphans_pruned: u64,
}

impl PassReport {
    fn new() -> Self {
        Self {
            end: PassEnd::Drained,
            memories: 0,
            summaries: 0,
            context_items: 0,
            failed: 0,
            batches: 0,
            orphans_pruned: 0,
        }
    }

    pub fn embedded(&self) -> usize {
        self.memories + self.summaries + self.context_items
    }

    fn ended(mut self, end: PassEnd) -> Self {
        self.end = end;
        self
    }
}

/// One row to embed: its passages are the whole text, or a long context item's chunks.
struct Work {
    corpus: Corpus,
    row_id: String,
    /// A summary's revision, read with its text.
    rev: Option<String>,
    text: String,
    /// More than one only for a long context item.
    spans: Vec<Chunk>,
    tokens: usize,
}

impl Work {
    fn new(corpus: Corpus, row_id: String, text: String, rev: Option<String>) -> Self {
        let spans = match corpus {
            Corpus::Context => chunking::chunk(
                &text,
                chunking::DEFAULT_CHUNK_BYTES,
                chunking::DEFAULT_OVERLAP_BYTES,
            ),
            _ if text.trim().is_empty() => Vec::new(),
            _ => vec![Chunk {
                start: 0,
                len: text.len(),
            }],
        };
        let tokens = estimate_tokens(&text);
        Self {
            corpus,
            row_id,
            rev,
            text,
            spans,
            tokens,
        }
    }

    fn passages(&self) -> impl Iterator<Item = &str> {
        self.spans.iter().filter_map(|s| s.slice(&self.text))
    }

    fn key(&self) -> (Corpus, String) {
        (self.corpus, self.row_id.clone())
    }
}

/// The slot's memory between passes; the runner owns one for the life of the process.
pub struct EmbeddingSlot {
    sizer: BatchSizer,
    /// Rows this model could not embed or store, skipped until a switch or [`Self::retry_failed`],
    /// so one bad row neither spins a pass nor warns every poll.
    passed_over: HashSet<(Corpus, String)>,
    /// Orphans are pruned and health logged once the backlog drains after any write or switch.
    tidy_owed: bool,
    generation: Option<u64>,
}

impl Default for EmbeddingSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingSlot {
    pub fn new() -> Self {
        Self {
            sizer: BatchSizer::new(BATCH_BUDGET),
            passed_over: HashSet::new(),
            tidy_owed: true,
            generation: None,
        }
    }

    /// Lets rows that failed be tried again, e.g. when a person asks for a reindex.
    pub fn retry_failed(&mut self) {
        self.passed_over.clear();
        self.tidy_owed = true;
    }

    /// Embeds what the current model lacks, memory first, until the backlog is empty or
    /// `control` says stop. Every vector is made and stored under one lease, so it carries the
    /// model that made it.
    pub async fn run_pass(
        &mut self,
        stores: &SlotStores<'_>,
        embedder: &ActiveEmbedder,
        control: &dyn SlotControl,
    ) -> PassReport {
        let report = PassReport::new();
        let (model_id, generation) = {
            let Some(lease) = embedder.lease().await else {
                return report.ended(PassEnd::NotReady);
            };
            // Read under the lease: a switch needs the write lock, so it cannot move meanwhile.
            (lease.model_id(), embedder.generation())
        };
        if self.generation != Some(generation) {
            if self.generation.is_some() {
                // Another model's speed says nothing about this one's.
                self.sizer = BatchSizer::new(BATCH_BUDGET);
            }
            self.generation = Some(generation);
            self.passed_over.clear();
            self.tidy_owed = true;
        }

        let mut pass = Pass {
            slot: self,
            stores,
            embedder,
            control,
            model_id: &model_id,
            generation,
            report,
            written: HashSet::new(),
        };
        if let Some(end) = pass.walk().await {
            return pass.report.ended(end);
        }
        let mut report = pass.report;
        if report.embedded() > 0 {
            tracing::info!(
                model_id = %model_id,
                memories = report.memories,
                summaries = report.summaries,
                context_items = report.context_items,
                failed = report.failed,
                batches = report.batches,
                "embedding slot: backlog embedded"
            );
        }
        if self.tidy_owed {
            self.tidy_owed = false;
            report.orphans_pruned = tidy(stores.index, &model_id).await;
        }
        report.ended(PassEnd::Drained)
    }
}

struct Pass<'p, 's> {
    slot: &'p mut EmbeddingSlot,
    stores: &'p SlotStores<'s>,
    embedder: &'p ActiveEmbedder,
    control: &'p dyn SlotControl,
    model_id: &'p str,
    generation: u64,
    report: PassReport,
    /// Rows stored this pass; one the staleness join returns again did not become current.
    written: HashSet<(Corpus, String)>,
}

impl Pass<'_, '_> {
    /// `None` once every corpus is drained, else why the pass ended early.
    async fn walk(&mut self) -> Option<PassEnd> {
        for corpus in ORDER {
            if corpus == Corpus::Context && self.stores.context.is_none() {
                continue;
            }
            loop {
                let passed_over = self
                    .slot
                    .passed_over
                    .iter()
                    .filter(|(c, _)| *c == corpus)
                    .count();
                // Room for rows already passed over, which the join keeps returning.
                let limit = ROWS_PER_READ + passed_over;
                let rows = match self.read(corpus, limit).await {
                    Ok(rows) => rows,
                    Err(e) => {
                        tracing::warn!(
                            corpus = corpus.as_str(),
                            "embedding slot: could not read the backlog: {e:#}"
                        );
                        return Some(PassEnd::StoreFailed);
                    }
                };
                let read = rows.len();
                let before = self.slot.passed_over.len();
                let mut fresh = Vec::with_capacity(read);
                for work in rows {
                    let key = work.key();
                    if self.slot.passed_over.contains(&key) {
                        continue;
                    }
                    if self.written.contains(&key) {
                        tracing::warn!(
                            corpus = corpus.as_str(),
                            row_id = %work.row_id,
                            "embedding slot: a stored vector did not make the row current"
                        );
                        self.pass_over(key);
                        continue;
                    }
                    if work.spans.is_empty() {
                        // Blank text has nothing to embed; it stays out of semantic search.
                        self.slot.passed_over.insert(key);
                        continue;
                    }
                    fresh.push(work);
                }
                if fresh.is_empty() {
                    // A full read of rows passed over grew the set, so the next read reaches past
                    // them; anything else means nothing new is left.
                    if read < limit || self.slot.passed_over.len() == before {
                        break;
                    }
                    continue;
                }
                if let Some(end) = self.embed_all(fresh).await {
                    return Some(end);
                }
            }
        }
        None
    }

    async fn read(&self, corpus: Corpus, limit: usize) -> Result<Vec<Work>> {
        let index = self.stores.index;
        match corpus {
            Corpus::Summary => {
                let ids = index.needs_embedding(corpus, self.model_id, limit).await?;
                let mut out = Vec::with_capacity(ids.len());
                for id in dedupe(ids) {
                    // Read together, or a racing rewrite stamps a newer revision than the text.
                    let (summary, rev) = self
                        .stores
                        .sessions
                        .get_rolling_summary_with_revision(&id)
                        .await
                        .map_err(|e| anyhow!("{e}"))?;
                    out.push(Work::new(corpus, id, summary.unwrap_or_default(), rev));
                }
                Ok(out)
            }
            _ => {
                let rows = index
                    .needs_embedding_with_text(corpus, self.model_id, limit)
                    .await?;
                let mut seen = HashSet::with_capacity(rows.len());
                Ok(rows
                    .into_iter()
                    .filter(|(id, _)| seen.insert(id.clone()))
                    .map(|(id, text)| Work::new(corpus, id, text, None))
                    .collect())
            }
        }
    }

    fn pass_over(&mut self, key: (Corpus, String)) {
        self.slot.passed_over.insert(key);
        self.report.failed += 1;
    }

    /// Works through `queue` in sized batches, checking `control` after each.
    async fn embed_all(&mut self, queue: Vec<Work>) -> Option<PassEnd> {
        let mut at = 0;
        while at < queue.len() {
            if self.embedder.generation() != self.generation {
                return Some(PassEnd::ModelChanged);
            }
            let Some(lease) = self.embedder.lease().await else {
                return Some(PassEnd::NotReady);
            };
            if self.embedder.generation() != self.generation {
                return Some(PassEnd::ModelChanged);
            }

            let rest = &queue[at..];
            let cap_texts = lease.max_batch().max(1);
            let mut fit = 0;
            let mut texts = 0;
            for work in rest {
                if fit > 0 && texts + work.spans.len() > cap_texts {
                    break;
                }
                texts += work.spans.len();
                fit += 1;
            }
            let tokens: Vec<usize> = rest[..fit].iter().map(|w| w.tokens).collect();
            let n = self.slot.sizer.take(&tokens, fit);
            let batch = &rest[..n];

            let started = Instant::now();
            let vectors = embed_works(&lease, batch).await;
            for (work, vectors) in batch.iter().zip(vectors) {
                let stored = match vectors {
                    Ok(vectors) => self.store(work, vectors).await,
                    Err(e) => Err(e),
                };
                match stored {
                    Ok(()) => {
                        self.written.insert(work.key());
                        self.slot.tidy_owed = true;
                        match work.corpus {
                            Corpus::Memory => self.report.memories += 1,
                            Corpus::Summary => self.report.summaries += 1,
                            Corpus::Context => self.report.context_items += 1,
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            corpus = work.corpus.as_str(),
                            row_id = %work.row_id,
                            "embedding slot: left without a vector until the model changes or a \
                             reindex: {e:#}"
                        );
                        self.pass_over(work.key());
                    }
                }
            }
            self.slot
                .sizer
                .record(tokens[..n].iter().sum(), started.elapsed());
            drop(lease);
            self.report.batches += 1;
            at += n;

            if !self.control.may_continue().await {
                return Some(PassEnd::Yielded);
            }
        }
        None
    }

    async fn store(&self, work: &Work, vectors: Vec<Vec<f32>>) -> Result<()> {
        let first = vectors
            .first()
            .ok_or_else(|| anyhow!("the embedder returned no vector"))?;
        let id = work.row_id.as_str();
        match work.corpus {
            // Writes the row and the index, as every other memory write does.
            Corpus::Memory => self.stores.memories.update_embedding(id, first).await,
            Corpus::Summary => {
                self.stores
                    .index
                    .upsert(&VectorEntry::whole(
                        Corpus::Summary,
                        work.row_id.clone(),
                        self.model_id.to_string(),
                        first.clone(),
                        work.rev.clone(),
                    ))
                    .await
            }
            Corpus::Context => {
                let context = self
                    .stores
                    .context
                    .ok_or_else(|| anyhow!("no context store is wired"))?;
                // Old passages first: the text may now split into fewer.
                self.stores.index.remove(Corpus::Context, id).await?;
                if work.spans.len() == 1 {
                    // The row and the index, so the store's own search reads this model too.
                    return context.update_embedding(id, first).await;
                }
                // Indexed by passage; the row keeps no vector, as ingest leaves a long item.
                for (ix, (span, vector)) in work.spans.iter().zip(vectors).enumerate() {
                    self.stores
                        .index
                        .upsert(&VectorEntry {
                            corpus: Corpus::Context,
                            row_id: work.row_id.clone(),
                            chunk_ix: ix as i64,
                            chunk_span: Some((span.start as i64, span.len as i64)),
                            model_id: self.model_id.to_string(),
                            vector,
                            source_rev: None,
                        })
                        .await?;
                }
                Ok(())
            }
        }
    }
}

fn dedupe(ids: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

/// Each work's vectors, one per passage. A batch that fails is retried a row at a time, so one
/// row the model refuses cannot keep its neighbours from their vectors.
async fn embed_works(lease: &EmbedLease<'_>, works: &[Work]) -> Vec<Result<Vec<Vec<f32>>>> {
    let texts: Vec<String> = works
        .iter()
        .flat_map(|w| w.passages().map(str::to_string))
        .collect();
    match embed_texts(lease, &texts).await {
        Ok(mut vectors) => {
            let mut out = Vec::with_capacity(works.len());
            for work in works {
                let rest = vectors.split_off(work.spans.len().min(vectors.len()));
                out.push(Ok(std::mem::replace(&mut vectors, rest)));
            }
            out
        }
        Err(e) if works.len() == 1 => vec![Err(e)],
        Err(_) => {
            let mut out = Vec::with_capacity(works.len());
            for work in works {
                let texts: Vec<String> = work.passages().map(str::to_string).collect();
                out.push(embed_texts(lease, &texts).await);
            }
            out
        }
    }
}

async fn embed_texts(lease: &EmbedLease<'_>, texts: &[String]) -> Result<Vec<Vec<f32>>> {
    let mut vectors = Vec::with_capacity(texts.len());
    for call in texts.chunks(lease.max_batch().max(1)) {
        vectors.extend(lease.embed_batch(call).await?);
    }
    if vectors.len() != texts.len() {
        return Err(anyhow!(
            "the embedder returned {} vectors for {} texts",
            vectors.len(),
            texts.len()
        ));
    }
    let dims = lease.dims();
    if let Some(v) = vectors.iter().find(|v| v.len() != dims) {
        return Err(anyhow!(
            "a {} wide vector from a model that is {dims} wide",
            v.len()
        ));
    }
    Ok(vectors)
}

/// Prunes rows whose source is gone and logs what the index holds for `model_id`.
async fn tidy(index: &dyn VectorIndex, model_id: &str) -> u64 {
    let pruned = match index.prune_orphans().await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("embedding slot: orphan prune failed: {e:#}");
            0
        }
    };
    match index.health(model_id).await {
        Ok(h) if h.mismatched > 0 || h.missing > 0 => tracing::warn!(
            model_id = %model_id,
            matching = h.matching,
            mismatched = h.mismatched,
            missing = h.missing,
            "embedding slot: some rows have no vector from this model; search leaves them out"
        ),
        Ok(h) => tracing::info!(
            model_id = %model_id,
            matching = h.matching,
            orphans_pruned = pruned,
            "embedding slot: every row has a vector from this model"
        ),
        Err(e) => tracing::warn!("embedding slot: index health read failed: {e:#}"),
    }
    pruned
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::vector_index::{IndexHealth, ResolvedHit, VectorHit};
    use crate::models::domain::embedding_models::EmbedderChoice;
    use crate::models::ports::embedding::EmbeddingProvider;
    use crate::user_data::domain::memory::MemoryFragment;
    use crate::user_data::domain::profile::ProfileScope;
    use crate::user_data::mocks::mock_session::InMemorySessionStorage;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const MODEL: &str = "test-model";
    const DIMS: usize = 3;

    /// Each row's text, and the model the index holds a vector of it under.
    type Rows = BTreeMap<(Corpus, String), (String, Option<String>)>;

    /// An index over rows of text, each current for at most one model, like the staleness join.
    #[derive(Default)]
    struct SpyIndex {
        rows: Mutex<Rows>,
        upserts: Mutex<Vec<VectorEntry>>,
        tidied: AtomicUsize,
    }

    impl SpyIndex {
        fn holding(rows: &[(Corpus, &str, &str, Option<&str>)]) -> Arc<Self> {
            let index = Self::default();
            {
                let mut map = index.rows.lock().unwrap();
                for (corpus, id, text, model) in rows {
                    map.insert(
                        (*corpus, id.to_string()),
                        (text.to_string(), model.map(str::to_string)),
                    );
                }
            }
            Arc::new(index)
        }

        fn model_of(&self, corpus: Corpus, id: &str) -> Option<String> {
            self.rows.lock().unwrap()[&(corpus, id.to_string())]
                .1
                .clone()
        }

        fn stale(&self, corpus: Corpus, model_id: &str, limit: usize) -> Vec<(String, String)> {
            self.rows
                .lock()
                .unwrap()
                .iter()
                .filter(|((c, _), (_, m))| *c == corpus && m.as_deref() != Some(model_id))
                .take(limit)
                .map(|((_, id), (text, _))| (id.clone(), text.clone()))
                .collect()
        }

        fn stamp(&self, corpus: Corpus, id: &str, model_id: &str) {
            if let Some(row) = self.rows.lock().unwrap().get_mut(&(corpus, id.to_string())) {
                row.1 = Some(model_id.to_string());
            }
        }
    }

    #[async_trait]
    impl VectorIndex for SpyIndex {
        async fn upsert(&self, e: &VectorEntry) -> Result<()> {
            self.stamp(e.corpus, &e.row_id, &e.model_id);
            self.upserts.lock().unwrap().push(e.clone());
            Ok(())
        }
        async fn remove(&self, c: Corpus, r: &str) -> Result<()> {
            if let Some(row) = self.rows.lock().unwrap().get_mut(&(c, r.to_string())) {
                row.1 = None;
            }
            Ok(())
        }
        async fn get(&self, _c: Corpus, _r: &str) -> Result<Option<VectorEntry>> {
            Ok(None)
        }
        async fn search(
            &self,
            _q: &[f32],
            _m: &str,
            _s: &ProfileScope,
            _l: usize,
        ) -> Result<Vec<VectorHit>> {
            Ok(vec![])
        }
        async fn search_resolved(
            &self,
            _q: &[f32],
            _m: &str,
            _s: &ProfileScope,
            _l: usize,
        ) -> Result<Vec<ResolvedHit>> {
            Ok(vec![])
        }
        async fn needs_embedding(&self, c: Corpus, m: &str, l: usize) -> Result<Vec<String>> {
            Ok(self.stale(c, m, l).into_iter().map(|(id, _)| id).collect())
        }
        async fn needs_embedding_with_text(
            &self,
            c: Corpus,
            m: &str,
            l: usize,
        ) -> Result<Vec<(String, String)>> {
            Ok(self.stale(c, m, l))
        }
        async fn prune_orphans(&self) -> Result<u64> {
            self.tidied.fetch_add(1, Ordering::SeqCst);
            Ok(0)
        }
        async fn health(&self, _m: &str) -> Result<IndexHealth> {
            Ok(IndexHealth::default())
        }
    }

    /// Memory writes that reach the index the way the SQLite store's do, unless told not to.
    struct IndexedMemories {
        index: Arc<SpyIndex>,
        embedder: Arc<ActiveEmbedder>,
        mirrors: bool,
        updated: Mutex<Vec<String>>,
    }

    impl IndexedMemories {
        fn new(index: &Arc<SpyIndex>, embedder: &Arc<ActiveEmbedder>) -> Self {
            Self {
                index: index.clone(),
                embedder: embedder.clone(),
                mirrors: true,
                updated: Mutex::new(vec![]),
            }
        }
        fn updated(&self) -> Vec<String> {
            self.updated.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl MemoryRepository for IndexedMemories {
        async fn add(&self, _f: MemoryFragment) -> Result<()> {
            Ok(())
        }
        async fn search_recent(&self, _s: &ProfileScope, _l: usize) -> Result<Vec<MemoryFragment>> {
            Ok(vec![])
        }
        async fn search_similar(
            &self,
            _q: &[f32],
            _s: &ProfileScope,
            _l: usize,
        ) -> Result<Vec<MemoryFragment>> {
            Ok(vec![])
        }
        async fn delete(&self, _id: &str) -> Result<()> {
            Ok(())
        }
        async fn update_embedding(&self, id: &str, embedding: &[f32]) -> Result<()> {
            assert_eq!(embedding.len(), DIMS);
            self.updated.lock().unwrap().push(id.to_string());
            if self.mirrors {
                self.index
                    .stamp(Corpus::Memory, id, &self.embedder.model_id());
            }
            Ok(())
        }
    }

    struct Provider {
        refuse: Option<&'static str>,
        max_batch: usize,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl EmbeddingProvider for Provider {
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.refuse {
                Some(word) if text.contains(word) => Err(anyhow!("refused {word}")),
                _ => Ok(vec![0.5; DIMS]),
            }
        }
        fn max_batch(&self) -> usize {
            self.max_batch
        }
        fn dimensions(&self) -> usize {
            DIMS
        }
        fn model_id(&self) -> String {
            MODEL.into()
        }
    }

    fn embedder(refuse: Option<&'static str>, max_batch: usize) -> Arc<ActiveEmbedder> {
        Arc::new(ActiveEmbedder::loaded(Arc::new(Provider {
            refuse,
            max_batch,
            calls: Arc::new(AtomicUsize::new(0)),
        })))
    }

    /// Says yes `allow` times, then no; may switch the model the first time it is asked.
    struct Control {
        asked: AtomicUsize,
        allow: usize,
        switch: Option<Arc<ActiveEmbedder>>,
    }

    impl Control {
        fn always() -> Self {
            Self::allowing(usize::MAX)
        }
        fn allowing(allow: usize) -> Self {
            Self {
                asked: AtomicUsize::new(0),
                allow,
                switch: None,
            }
        }
    }

    #[async_trait]
    impl SlotControl for Control {
        async fn may_continue(&self) -> bool {
            let asked = self.asked.fetch_add(1, Ordering::SeqCst);
            if asked == 0 {
                if let Some(embedder) = &self.switch {
                    embedder.choose(EmbedderChoice::Off).await;
                }
            }
            asked < self.allow
        }
    }

    fn stores<'a>(
        index: &'a SpyIndex,
        memories: &'a dyn MemoryRepository,
        sessions: &'a InMemorySessionStorage,
        context: Option<&'a dyn ContextRepository>,
    ) -> SlotStores<'a> {
        SlotStores {
            index,
            memories,
            sessions,
            context,
        }
    }

    fn memories(n: usize) -> Vec<(Corpus, String, String)> {
        (0..n)
            .map(|i| {
                (
                    Corpus::Memory,
                    format!("m{i}"),
                    format!("Jerry waters bed {i}"),
                )
            })
            .collect()
    }

    fn holding(rows: &[(Corpus, String, String)]) -> Arc<SpyIndex> {
        let rows: Vec<(Corpus, &str, &str, Option<&str>)> = rows
            .iter()
            .map(|(c, id, text)| (*c, id.as_str(), text.as_str(), None))
            .collect();
        SpyIndex::holding(&rows)
    }

    #[tokio::test]
    async fn memory_goes_through_the_repository_and_rows_of_another_model_are_redone() {
        let index = SpyIndex::holding(&[
            (Corpus::Memory, "never", "Jerry keeps bees", None),
            (
                Corpus::Memory,
                "old",
                "Jerry plays chess",
                Some("old-model"),
            ),
            (Corpus::Memory, "current", "Jerry cycles", Some(MODEL)),
        ]);
        let embedder = embedder(None, 1);
        let memories = IndexedMemories::new(&index, &embedder);
        let sessions = InMemorySessionStorage::new();

        let report = EmbeddingSlot::new()
            .run_pass(
                &stores(&index, &memories, &sessions, None),
                &embedder,
                &Control::always(),
            )
            .await;

        assert_eq!(report.end, PassEnd::Drained);
        assert_eq!(report.memories, 2);
        let mut updated = memories.updated();
        updated.sort();
        assert_eq!(
            updated,
            vec!["never", "old"],
            "the current row is left alone"
        );
        assert_eq!(
            index.model_of(Corpus::Memory, "old").as_deref(),
            Some(MODEL)
        );
        assert!(
            index.upserts.lock().unwrap().is_empty(),
            "memory reaches the index only through its repository"
        );
        assert_eq!(index.tidied.load(Ordering::SeqCst), 1, "drained, so tidied");
    }

    #[tokio::test]
    async fn the_pass_stops_after_one_batch_when_told_to() {
        let index = holding(&memories(5));
        let embedder = embedder(None, 1);
        let memories = IndexedMemories::new(&index, &embedder);
        let sessions = InMemorySessionStorage::new();

        let report = EmbeddingSlot::new()
            .run_pass(
                &stores(&index, &memories, &sessions, None),
                &embedder,
                &Control::allowing(0),
            )
            .await;

        assert_eq!(report.end, PassEnd::Yielded);
        assert_eq!(report.batches, 1);
        assert_eq!(memories.updated().len(), 1, "the sizer starts at one item");
        assert_eq!(index.tidied.load(Ordering::SeqCst), 0, "not drained");
    }

    #[tokio::test]
    async fn a_switch_between_batches_ends_the_pass() {
        let index = holding(&memories(3));
        let embedder = embedder(None, 1);
        let memories = IndexedMemories::new(&index, &embedder);
        let sessions = InMemorySessionStorage::new();
        let control = Control {
            switch: Some(embedder.clone()),
            ..Control::always()
        };

        let report = EmbeddingSlot::new()
            .run_pass(
                &stores(&index, &memories, &sessions, None),
                &embedder,
                &control,
            )
            .await;

        assert_eq!(report.end, PassEnd::ModelChanged);
        assert_eq!(memories.updated().len(), 1);
    }

    #[tokio::test]
    async fn rows_that_fail_end_the_pass_instead_of_spinning() {
        let index = holding(&[
            (Corpus::Memory, "a".into(), "poison one".into()),
            (Corpus::Memory, "b".into(), "fine".into()),
            (Corpus::Memory, "c".into(), "poison two".into()),
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let embedder = Arc::new(ActiveEmbedder::loaded(Arc::new(Provider {
            refuse: Some("poison"),
            max_batch: 1,
            calls: calls.clone(),
        })));
        let memories = IndexedMemories::new(&index, &embedder);
        let sessions = InMemorySessionStorage::new();
        let mut slot = EmbeddingSlot::new();

        let report = tokio::time::timeout(
            Duration::from_secs(5),
            slot.run_pass(
                &stores(&index, &memories, &sessions, None),
                &embedder,
                &Control::always(),
            ),
        )
        .await
        .expect("a pass over rows that always fail must end");
        assert_eq!(report.end, PassEnd::Drained);
        assert_eq!((report.memories, report.failed), (1, 2));
        assert_eq!(calls.load(Ordering::SeqCst), 3, "each row is tried once");

        let again = slot
            .run_pass(
                &stores(&index, &memories, &sessions, None),
                &embedder,
                &Control::always(),
            )
            .await;
        assert_eq!(again.embedded() + again.failed, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 3, "not retried every poll");

        slot.retry_failed();
        let asked = slot
            .run_pass(
                &stores(&index, &memories, &sessions, None),
                &embedder,
                &Control::always(),
            )
            .await;
        assert_eq!(asked.failed, 2, "a reindex tries them again");
    }

    #[tokio::test]
    async fn a_write_that_leaves_the_row_stale_is_not_retried_in_a_loop() {
        let index = holding(&memories(2));
        let embedder = embedder(None, 1);
        let memories = IndexedMemories {
            mirrors: false,
            ..IndexedMemories::new(&index, &embedder)
        };
        let sessions = InMemorySessionStorage::new();

        let report = tokio::time::timeout(
            Duration::from_secs(5),
            EmbeddingSlot::new().run_pass(
                &stores(&index, &memories, &sessions, None),
                &embedder,
                &Control::always(),
            ),
        )
        .await
        .expect("must end");
        assert_eq!(report.end, PassEnd::Drained);
        assert_eq!(memories.updated().len(), 2, "each row written once");
        assert_eq!(report.failed, 2);
    }

    #[tokio::test]
    async fn a_batch_one_row_breaks_is_redone_a_row_at_a_time() {
        let index = holding(&[
            (Corpus::Memory, "a".into(), "fine".into()),
            (Corpus::Memory, "b".into(), "poison".into()),
            (Corpus::Memory, "c".into(), "also fine".into()),
        ]);
        let embedder = embedder(Some("poison"), 8);
        let memories = IndexedMemories::new(&index, &embedder);
        let sessions = InMemorySessionStorage::new();
        let mut slot = EmbeddingSlot::new();
        // Taught that rows are cheap, so the next batch takes all three.
        slot.sizer.record(1, Duration::from_micros(1));

        let report = slot
            .run_pass(
                &stores(&index, &memories, &sessions, None),
                &embedder,
                &Control::always(),
            )
            .await;
        assert_eq!(report.batches, 1);
        assert_eq!((report.memories, report.failed), (2, 1));
    }

    #[tokio::test]
    async fn a_summary_is_indexed_with_the_revision_read_beside_its_text() {
        let index = SpyIndex::holding(&[(Corpus::Summary, "s1", "", None)]);
        let embedder = embedder(None, 1);
        let memories = IndexedMemories::new(&index, &embedder);
        let sessions = InMemorySessionStorage::new();
        sessions
            .set_rolling_summary("s1", "We planned the greenhouse vents", "msg-9")
            .await
            .unwrap();

        let report = EmbeddingSlot::new()
            .run_pass(
                &stores(&index, &memories, &sessions, None),
                &embedder,
                &Control::always(),
            )
            .await;

        assert_eq!(report.summaries, 1);
        let upserts = index.upserts.lock().unwrap();
        assert_eq!(upserts.len(), 1);
        assert_eq!(upserts[0].corpus, Corpus::Summary);
        assert_eq!(upserts[0].model_id, MODEL);
        assert!(upserts[0].source_rev.is_some());
    }

    #[tokio::test]
    async fn a_long_context_item_is_indexed_by_passage_and_a_short_one_through_its_store() {
        use crate::context::domain::{ContextItem, ItemKind, ItemParts, SourceKind};
        use crate::context::mocks::mock_context_repository::MockContextRepository;
        use crate::security::domain::redaction::RedactionKind;
        use crate::security::mocks::mock_redactor::MockRedactor;

        let redactor = MockRedactor::replacing("sk-never-present", RedactionKind::ApiKey);
        let item = |id: &str, body: &str| {
            ContextItem::from_parts(
                &redactor,
                ItemParts {
                    id: id.into(),
                    source_id: "mail".into(),
                    external_id: id.into(),
                    profile_id: "jerry".into(),
                    source_kind: SourceKind::Mail,
                    kind: ItemKind::Message,
                    occurred_at: chrono::Utc::now(),
                    ingested_at: chrono::Utc::now(),
                    title: String::new(),
                    body: body.into(),
                    participants: vec![],
                    stored_sensitivity: None,
                    embedding: None,
                },
            )
            .unwrap()
        };

        let long = "The invoice for the solar pump is attached. ".repeat(40);
        let index = SpyIndex::holding(&[
            (Corpus::Context, "short", "Rain tomorrow", None),
            (Corpus::Context, "long", &long, Some("old-model")),
        ]);
        let embedder = embedder(None, 64);
        let memories = IndexedMemories::new(&index, &embedder);
        let sessions = InMemorySessionStorage::new();
        let context = MockContextRepository::new();
        for (id, text) in [("short", "Rain tomorrow"), ("long", long.as_str())] {
            context.save_item(&item(id, text)).await.unwrap();
        }

        let report = EmbeddingSlot::new()
            .run_pass(
                &stores(&index, &memories, &sessions, Some(&context)),
                &embedder,
                &Control::always(),
            )
            .await;

        assert_eq!(report.context_items, 2);
        let passages = chunking::chunk(
            &long,
            chunking::DEFAULT_CHUNK_BYTES,
            chunking::DEFAULT_OVERLAP_BYTES,
        );
        assert!(passages.len() > 1);
        let upserts = index.upserts.lock().unwrap();
        let long_chunks: Vec<&VectorEntry> =
            upserts.iter().filter(|e| e.row_id == "long").collect();
        assert_eq!(long_chunks.len(), passages.len());
        assert!(long_chunks
            .iter()
            .all(|e| e.chunk_span.is_some() && e.model_id == MODEL));
        let stored = |id: &str| {
            context
                .all_items()
                .into_iter()
                .find(|item| item.id() == id)
                .and_then(|item| item.embedding().map(<[f32]>::len))
        };
        assert_eq!(stored("short"), Some(DIMS));
        assert_eq!(stored("long"), None, "a long item has no single vector");
    }

    #[test]
    fn the_sizer_starts_at_one_and_packs_to_its_budget() {
        let mut sizer = BatchSizer::new(Duration::from_millis(250));
        assert_eq!(sizer.take(&[10; 50], 50), 1, "nothing timed yet");
        assert_eq!(sizer.take(&[], 50), 0);

        // 10 ms per token: a 5-token item costs 50 ms, so five fit in 250 ms.
        sizer.record(5, Duration::from_millis(50));
        assert_eq!(sizer.take(&[5; 50], 50), 5);
        assert_eq!(sizer.take(&[5; 50], 3), 3, "the provider's cap wins");
        assert_eq!(sizer.take(&[5; 2], 50), 2, "no more than there is");
        assert_eq!(
            sizer.take(&[1_000, 5], 50),
            1,
            "one item over budget still goes alone"
        );
    }

    #[test]
    fn the_sizer_follows_a_slower_device_without_jumping() {
        let mut sizer = BatchSizer::new(Duration::from_millis(250));
        sizer.record(10, Duration::from_millis(10));
        let fast = sizer.take(&[10; 100], 100);
        sizer.record(10, Duration::from_millis(100));
        let slower = sizer.take(&[10; 100], 100);
        assert!(slower < fast, "{slower} < {fast}");
        assert!(slower > 2, "smoothed, not the last batch alone: {slower}");
    }
}
