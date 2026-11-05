//! The embedding slot keeps every indexed item's vector current for the chosen model, in
//! counter-phase. Work is found by the index's staleness join, so there is no queue and a
//! restart resumes.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::context::chunking::{self, Chunk};
use crate::context::vector_index::{Stale, VectorEntry, VectorIndex, INDEXED};
use crate::items::Kind;
use crate::models::ports::embedding::EmbeddingProvider as _;
use crate::models::services::active_embedder::{ActiveEmbedder, EmbedLease};

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

/// The stores a pass reads and writes: the index alone, which reads each item's text from its
/// header in the same query that finds it stale.
pub struct SlotStores<'a> {
    pub index: &'a dyn VectorIndex,
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

/// One item to embed: its passages are the whole text, or a long item's chunks.
struct Work {
    kind: Kind,
    item_id: String,
    /// The header's revision, read with its text.
    rev: String,
    text: String,
    /// More than one only for a long ingested item.
    spans: Vec<Chunk>,
    tokens: usize,
}

impl Work {
    fn new(stale: Stale) -> Self {
        let Stale {
            item_id,
            kind,
            text,
            rev,
        } = stale;
        let spans = if kind.chunked() {
            chunking::chunk(
                &text,
                chunking::DEFAULT_CHUNK_BYTES,
                chunking::DEFAULT_OVERLAP_BYTES,
            )
        } else if text.trim().is_empty() {
            Vec::new()
        } else {
            vec![Chunk {
                start: 0,
                len: text.len(),
            }]
        };
        let tokens = estimate_tokens(&text);
        Self {
            kind,
            item_id,
            rev,
            text,
            spans,
            tokens,
        }
    }

    fn passages(&self) -> impl Iterator<Item = &str> {
        self.spans.iter().filter_map(|s| s.slice(&self.text))
    }
}

/// The slot's memory between passes; the runner owns one for the life of the process.
pub struct EmbeddingSlot {
    sizer: BatchSizer,
    /// Rows this model could not embed or store, skipped until a switch or [`Self::retry_failed`],
    /// so one bad row neither spins a pass nor warns every poll.
    passed_over: HashSet<String>,
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
    /// Items stored this pass; one the staleness join returns again did not become current.
    written: HashSet<String>,
}

impl Pass<'_, '_> {
    /// `None` once the backlog is drained, else why the pass ended early.
    async fn walk(&mut self) -> Option<PassEnd> {
        loop {
            // Room for items already passed over, which the join keeps returning.
            let limit = ROWS_PER_READ + self.slot.passed_over.len();
            let stale = match self
                .stores
                .index
                .needs_embedding(&INDEXED, self.model_id, limit)
                .await
            {
                Ok(stale) => stale,
                Err(e) => {
                    tracing::warn!("embedding slot: could not read the backlog: {e:#}");
                    return Some(PassEnd::StoreFailed);
                }
            };
            let read = stale.len();
            let before = self.slot.passed_over.len();
            let mut seen = HashSet::with_capacity(read);
            let mut fresh = Vec::with_capacity(read);
            for stale in stale {
                if !seen.insert(stale.item_id.clone())
                    || self.slot.passed_over.contains(&stale.item_id)
                {
                    continue;
                }
                let work = Work::new(stale);
                if self.written.contains(&work.item_id) {
                    tracing::warn!(
                        kind = %work.kind,
                        item_id = %work.item_id,
                        "embedding slot: a stored vector did not make the item current"
                    );
                    self.pass_over(work.item_id);
                    continue;
                }
                if work.spans.is_empty() {
                    // Blank text has nothing to embed; it stays out of semantic search.
                    self.slot.passed_over.insert(work.item_id);
                    continue;
                }
                fresh.push(work);
            }
            if fresh.is_empty() {
                // A full read of items passed over grew the set, so the next read reaches past
                // them; anything else means nothing new is left.
                if read < limit || self.slot.passed_over.len() == before {
                    return None;
                }
                continue;
            }
            if let Some(end) = self.embed_all(fresh).await {
                return Some(end);
            }
        }
    }

    fn pass_over(&mut self, item_id: String) {
        self.slot.passed_over.insert(item_id);
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
                        self.written.insert(work.item_id.clone());
                        self.slot.tidy_owed = true;
                        match work.kind {
                            Kind::Conversation => self.report.summaries += 1,
                            kind if kind.chunked() => self.report.context_items += 1,
                            _ => self.report.memories += 1,
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            kind = %work.kind,
                            item_id = %work.item_id,
                            "embedding slot: left without a vector until the model changes or a \
                             reindex: {e:#}"
                        );
                        self.pass_over(work.item_id.clone());
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
        let index = self.stores.index;
        let first = vectors
            .first()
            .ok_or_else(|| anyhow!("the embedder returned no vector"))?;
        if work.spans.len() == 1 && !work.kind.chunked() {
            return index
                .upsert(&VectorEntry::whole(
                    work.item_id.clone(),
                    self.model_id.to_string(),
                    first.clone(),
                    Some(work.rev.clone()),
                ))
                .await;
        }
        // Old passages first: the text may now split into fewer.
        index.remove(&work.item_id).await?;
        for (ix, (span, vector)) in work.spans.iter().zip(vectors).enumerate() {
            let whole = work.spans.len() == 1;
            index
                .upsert(&VectorEntry {
                    item_id: work.item_id.clone(),
                    chunk_ix: ix as i64,
                    chunk_span: (!whole).then_some((span.start as i64, span.len as i64)),
                    model_id: self.model_id.to_string(),
                    vector,
                    source_rev: Some(work.rev.clone()),
                })
                .await?;
        }
        Ok(())
    }
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
    use crate::user_data::domain::profile::ProfileScope;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const MODEL: &str = "test-model";
    const DIMS: usize = 3;

    struct Item {
        kind: Kind,
        text: String,
        rev: String,
        /// The model the index holds a current vector of it under.
        model: Option<String>,
    }

    /// An index over items, each current for at most one model, like the staleness join.
    #[derive(Default)]
    struct SpyIndex {
        items: Mutex<BTreeMap<String, Item>>,
        upserts: Mutex<Vec<VectorEntry>>,
        tidied: AtomicUsize,
        /// When false, a write lands but leaves the item stale, as a racing rewrite would.
        sticks: bool,
    }

    impl SpyIndex {
        fn holding(items: &[(Kind, &str, &str, Option<&str>)]) -> Arc<Self> {
            Arc::new(Self::with(items, true))
        }

        fn with(items: &[(Kind, &str, &str, Option<&str>)], sticks: bool) -> Self {
            let index = Self {
                sticks,
                ..Self::default()
            };
            {
                let mut map = index.items.lock().unwrap();
                for (kind, id, text, model) in items {
                    map.insert(
                        id.to_string(),
                        Item {
                            kind: *kind,
                            text: text.to_string(),
                            rev: format!("rev-{id}"),
                            model: model.map(str::to_string),
                        },
                    );
                }
            }
            index
        }

        fn model_of(&self, id: &str) -> Option<String> {
            self.items.lock().unwrap()[id].model.clone()
        }

        fn written(&self) -> Vec<String> {
            let mut ids: Vec<String> = self
                .upserts
                .lock()
                .unwrap()
                .iter()
                .map(|e| e.item_id.clone())
                .collect();
            ids.sort();
            ids.dedup();
            ids
        }
    }

    #[async_trait]
    impl VectorIndex for SpyIndex {
        async fn upsert(&self, e: &VectorEntry) -> Result<()> {
            if self.sticks {
                if let Some(item) = self.items.lock().unwrap().get_mut(&e.item_id) {
                    item.model = Some(e.model_id.clone());
                }
            }
            self.upserts.lock().unwrap().push(e.clone());
            Ok(())
        }
        async fn remove(&self, id: &str) -> Result<()> {
            if let Some(item) = self.items.lock().unwrap().get_mut(id) {
                item.model = None;
            }
            Ok(())
        }
        async fn get(&self, _id: &str) -> Result<Option<VectorEntry>> {
            Ok(None)
        }
        async fn search(
            &self,
            _q: &[f32],
            _m: &str,
            _s: &ProfileScope,
            _k: &[Kind],
            _l: usize,
        ) -> Result<Vec<VectorHit>> {
            Ok(vec![])
        }
        async fn search_resolved(
            &self,
            _q: &[f32],
            _m: &str,
            _s: &ProfileScope,
            _k: &[Kind],
            _l: usize,
        ) -> Result<Vec<ResolvedHit>> {
            Ok(vec![])
        }
        async fn needs_embedding(&self, kinds: &[Kind], m: &str, l: usize) -> Result<Vec<Stale>> {
            let items = self.items.lock().unwrap();
            let mut stale: Vec<Stale> = items
                .iter()
                .filter(|(_, i)| kinds.contains(&i.kind) && i.model.as_deref() != Some(m))
                .map(|(id, i)| Stale {
                    item_id: id.clone(),
                    kind: i.kind,
                    text: i.text.clone(),
                    rev: i.rev.clone(),
                })
                .collect();
            stale.sort_by_key(|s| kinds.iter().position(|k| *k == s.kind));
            stale.truncate(l);
            Ok(stale)
        }
        async fn prune_orphans(&self) -> Result<u64> {
            self.tidied.fetch_add(1, Ordering::SeqCst);
            Ok(0)
        }
        async fn health(&self, _m: &str) -> Result<IndexHealth> {
            Ok(IndexHealth::default())
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

    fn stores(index: &SpyIndex) -> SlotStores<'_> {
        SlotStores { index }
    }

    fn memories(n: usize) -> Vec<(Kind, String, String)> {
        (0..n)
            .map(|i| (Kind::Fact, format!("m{i}"), format!("Jerry waters bed {i}")))
            .collect()
    }

    fn holding(items: &[(Kind, String, String)]) -> Arc<SpyIndex> {
        let items: Vec<(Kind, &str, &str, Option<&str>)> = items
            .iter()
            .map(|(k, id, text)| (*k, id.as_str(), text.as_str(), None))
            .collect();
        SpyIndex::holding(&items)
    }

    #[tokio::test]
    async fn items_of_another_model_are_redone_and_current_ones_left_alone() {
        let index = SpyIndex::holding(&[
            (Kind::Fact, "never", "Jerry keeps bees", None),
            (Kind::Trait, "old", "Jerry plays chess", Some("old-model")),
            (Kind::Fact, "current", "Jerry cycles", Some(MODEL)),
        ]);
        let embedder = embedder(None, 1);

        let report = EmbeddingSlot::new()
            .run_pass(&stores(&index), &embedder, &Control::always())
            .await;

        assert_eq!(report.end, PassEnd::Drained);
        assert_eq!(report.memories, 2);
        assert_eq!(
            index.written(),
            vec!["never", "old"],
            "the current item is left alone"
        );
        assert_eq!(index.model_of("old").as_deref(), Some(MODEL));
        assert!(index
            .upserts
            .lock()
            .unwrap()
            .iter()
            .all(|e| e.source_rev == Some(format!("rev-{}", e.item_id))));
        assert_eq!(index.tidied.load(Ordering::SeqCst), 1, "drained, so tidied");
    }

    #[tokio::test]
    async fn the_pass_stops_after_one_batch_when_told_to() {
        let index = holding(&memories(5));
        let embedder = embedder(None, 1);

        let report = EmbeddingSlot::new()
            .run_pass(&stores(&index), &embedder, &Control::allowing(0))
            .await;

        assert_eq!(report.end, PassEnd::Yielded);
        assert_eq!(report.batches, 1);
        assert_eq!(index.written().len(), 1, "the sizer starts at one item");
        assert_eq!(index.tidied.load(Ordering::SeqCst), 0, "not drained");
    }

    #[tokio::test]
    async fn a_switch_between_batches_ends_the_pass() {
        let index = holding(&memories(3));
        let embedder = embedder(None, 1);
        let control = Control {
            switch: Some(embedder.clone()),
            ..Control::always()
        };

        let report = EmbeddingSlot::new()
            .run_pass(&stores(&index), &embedder, &control)
            .await;

        assert_eq!(report.end, PassEnd::ModelChanged);
        assert_eq!(index.written().len(), 1);
    }

    #[tokio::test]
    async fn items_that_fail_end_the_pass_instead_of_spinning() {
        let index = holding(&[
            (Kind::Fact, "a".into(), "poison one".into()),
            (Kind::Fact, "b".into(), "fine".into()),
            (Kind::Fact, "c".into(), "poison two".into()),
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let embedder = Arc::new(ActiveEmbedder::loaded(Arc::new(Provider {
            refuse: Some("poison"),
            max_batch: 1,
            calls: calls.clone(),
        })));
        let mut slot = EmbeddingSlot::new();

        let report = tokio::time::timeout(
            Duration::from_secs(5),
            slot.run_pass(&stores(&index), &embedder, &Control::always()),
        )
        .await
        .expect("a pass over items that always fail must end");
        assert_eq!(report.end, PassEnd::Drained);
        assert_eq!((report.memories, report.failed), (1, 2));
        assert_eq!(calls.load(Ordering::SeqCst), 3, "each item is tried once");

        let again = slot
            .run_pass(&stores(&index), &embedder, &Control::always())
            .await;
        assert_eq!(again.embedded() + again.failed, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 3, "not retried every poll");

        slot.retry_failed();
        let asked = slot
            .run_pass(&stores(&index), &embedder, &Control::always())
            .await;
        assert_eq!(asked.failed, 2, "a reindex tries them again");
    }

    #[tokio::test]
    async fn a_write_that_leaves_the_item_stale_is_not_retried_in_a_loop() {
        let items: Vec<(Kind, String, String)> = memories(2);
        let items: Vec<(Kind, &str, &str, Option<&str>)> = items
            .iter()
            .map(|(k, id, text)| (*k, id.as_str(), text.as_str(), None))
            .collect();
        let index = SpyIndex::with(&items, false);
        let embedder = embedder(None, 1);

        let report = tokio::time::timeout(
            Duration::from_secs(5),
            EmbeddingSlot::new().run_pass(&stores(&index), &embedder, &Control::always()),
        )
        .await
        .expect("must end");
        assert_eq!(report.end, PassEnd::Drained);
        assert_eq!(
            index.upserts.lock().unwrap().len(),
            2,
            "each item written once"
        );
        assert_eq!(report.failed, 2);
    }

    #[tokio::test]
    async fn a_batch_one_item_breaks_is_redone_an_item_at_a_time() {
        let index = holding(&[
            (Kind::Fact, "a".into(), "fine".into()),
            (Kind::Fact, "b".into(), "poison".into()),
            (Kind::Fact, "c".into(), "also fine".into()),
        ]);
        let embedder = embedder(Some("poison"), 8);
        let mut slot = EmbeddingSlot::new();
        // Taught that items are cheap, so the next batch takes all three.
        slot.sizer.record(1, Duration::from_micros(1));

        let report = slot
            .run_pass(&stores(&index), &embedder, &Control::always())
            .await;
        assert_eq!(report.batches, 1);
        assert_eq!((report.memories, report.failed), (2, 1));
    }

    #[tokio::test]
    async fn memory_is_embedded_before_conversations_and_ingested_items() {
        let index = SpyIndex::holding(&[
            (Kind::Mail, "a-mail", "Invoice due", None),
            (Kind::Conversation, "b-chat", "We planned the vents", None),
            (Kind::Fact, "c-fact", "Jerry keeps bees", None),
        ]);
        let embedder = embedder(None, 1);
        let report = EmbeddingSlot::new()
            .run_pass(&stores(&index), &embedder, &Control::always())
            .await;
        let order: Vec<String> = index
            .upserts
            .lock()
            .unwrap()
            .iter()
            .map(|e| e.item_id.clone())
            .collect();
        assert_eq!(order, vec!["c-fact", "b-chat", "a-mail"]);
        assert_eq!(
            (report.memories, report.summaries, report.context_items),
            (1, 1, 1)
        );
    }

    #[tokio::test]
    async fn a_long_ingested_item_is_indexed_by_passage_and_a_short_one_whole() {
        let long = "The invoice for the solar pump is attached. ".repeat(40);
        let index = SpyIndex::holding(&[
            (Kind::Mail, "short", "Rain tomorrow", None),
            (Kind::Mail, "long", &long, Some("old-model")),
        ]);
        let embedder = embedder(None, 64);

        let report = EmbeddingSlot::new()
            .run_pass(&stores(&index), &embedder, &Control::always())
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
            upserts.iter().filter(|e| e.item_id == "long").collect();
        assert_eq!(long_chunks.len(), passages.len());
        assert!(long_chunks
            .iter()
            .all(|e| e.chunk_span.is_some() && e.model_id == MODEL));
        let short: Vec<&VectorEntry> = upserts.iter().filter(|e| e.item_id == "short").collect();
        assert_eq!(short.len(), 1);
        assert_eq!(
            short[0].chunk_span, None,
            "a short item is one whole vector"
        );
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
