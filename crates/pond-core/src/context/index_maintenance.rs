//! Keeping the personal-context index honest (phase D). One sweep repairs every way derived data
//! drifts: never-indexed rows, vectors from a different `embedding_provider`, rewritten summaries,
//! and orphans left by a cross-file delete that WAL cannot make atomic. A `LEFT JOIN` against the
//! live stores makes it restartable; batches pause and cancel so a member's turn beats the embed.
//!
//! It also carries the only RECURRING repair of an unembedded memory row (step 2c). That belongs
//! here and nowhere else: the startup backfill runs once per process, adoption only copies vectors
//! that already exist, and three ordinary paths keep minting rows with none.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::context::summary_indexing::{run_summary_indexing, SUMMARY_BATCH_PAUSE_MS};
use crate::context::vector_index::{Corpus, VectorIndex};
use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::ports::memory_repository::MemoryRepository;
use crate::user_data::ports::session_storage::SessionStorage;
use crate::user_data::services::memory_relevance::{
    run_memory_embedding_sweep, BACKFILL_BATCH_PAUSE_MS, BACKFILL_BATCH_SIZE,
};

/// What one maintenance pass did. Reported so "the index is quietly incomplete"
/// is a number somebody can read rather than something inferred from bad answers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaintenanceReport {
    /// Existing vectors taught to the index without re-embedding.
    pub adopted: u64,
    /// Summaries embedded.
    pub summaries_indexed: usize,
    /// Context items embedded that arrived without a vector.
    pub context_indexed: usize,
    /// Memory fragments embedded that arrived without a vector.
    pub memories_indexed: usize,
    /// Index rows whose source row is gone.
    pub orphans_pruned: u64,
    /// Rows still lacking a usable vector when the pass finished.
    pub still_missing: u64,
    /// Vectors from another model, still awaiting re-embedding.
    pub mismatched: u64,
}

/// How much of the backlog one pass is allowed to work through. A pass never fails: every step is
/// best-effort and logged, because aborting the process is worse than leaving work for next time.
/// A scheduled pass takes one bite, so a household's next turn never queues behind the whole
/// mailbox; a pass somebody ASKED for runs to completion, or the reindex button reports a lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexBudget {
    /// One batch, then stop.
    OneBatch,
    /// Keep going until nothing is missing, or until cancelled.
    UntilDone,
}

/// What one tick of the sweep is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SweepTick {
    /// Whether this tick may run on a pond that has served no turn since boot.
    ///
    /// This job's own exemption, ORed by the lane with the household-wide activity flag, and
    /// deliberately not that flag: relaxing it would qualify every other job on the lane.
    pub exempt_from_activity_gate: bool,
    /// How much of the backlog this tick may work through.
    pub budget: IndexBudget,
}

/// Decide whether a sweep tick may run, and how much it may do.
///
/// The lane gates background jobs on activity since boot, which deadlocks the index: mail arrives
/// from a connector, not a conversation. So the first pass after boot is exempt and exhaustive.
pub fn plan_sweep(asked: bool, indexed_since_boot: bool) -> SweepTick {
    let first_post_boot = !indexed_since_boot;
    SweepTick {
        exempt_from_activity_gate: asked || first_post_boot,
        budget: if asked || first_post_boot {
            IndexBudget::UntilDone
        } else {
            IndexBudget::OneBatch
        },
    }
}

/// How many items one batch embeds. Also the step size of an exhaustive pass,
/// which stays batched so cancellation is honoured promptly.
const CONTEXT_BATCH: usize = 64;

/// A bound on an exhaustive pass, so a corpus that never drains -- an item that
/// fails to embed every time and stays "missing" -- cannot spin forever.
const MAX_BATCHES: usize = 500;

pub async fn run_index_maintenance(
    index: &Arc<dyn VectorIndex>,
    storage: &dyn SessionStorage,
    memories: &dyn MemoryRepository,
    embedder: &dyn EmbeddingProvider,
    cancel: &CancellationToken,
    budget: IndexBudget,
) -> MaintenanceReport {
    let mut report = MaintenanceReport::default();
    let model_id = embedder.model_id();
    let dims = embedder.dimensions();

    // 1. Adoption first, because it is free. Any vector that already exists in a
    //    source table is copied in pure SQL, with no inference at all -- so the
    //    expensive steps below have less to do.
    for corpus in [Corpus::Memory, Corpus::Context] {
        if cancel.is_cancelled() {
            return report;
        }
        match index.backfill_from_source(corpus, &model_id, dims).await {
            Ok(n) => report.adopted += n,
            Err(e) => tracing::warn!(corpus = corpus.as_str(), "index adoption failed: {e:#}"),
        }
    }

    // 2. Summaries: the only corpus with no vector of its own, so the only step
    //    that spends the device's scarce resource.
    if !cancel.is_cancelled() {
        report.summaries_indexed = run_summary_indexing(
            storage,
            embedder,
            index,
            cancel,
            crate::context::summary_indexing::SUMMARY_BATCH_SIZE,
            SUMMARY_BATCH_PAUSE_MS,
        )
        .await;
    }

    // 2b. Context items that arrived WITHOUT a vector. Adoption only copies vectors that already
    //     exist, so without this step the corpus stays silently unsearchable and the assistant
    //     answers "no recorded activity" -- a wrong answer rather than an absent one.
    let mut batches = 0usize;
    while !cancel.is_cancelled() && batches < MAX_BATCHES {
        batches += 1;
        // `needs_embedding_with_text` is re-asked each time rather than paged:
        // a row just embedded stops being returned, so the next call is the
        // next batch. Paging by offset over a shrinking set would skip rows.
        let batch = match index
            .needs_embedding_with_text(Corpus::Context, &model_id, CONTEXT_BATCH)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("[context-index] fetch failed: {e}");
                break;
            }
        };
        if batch.is_empty() {
            break;
        }
        let worked = batch.len();
        for (row_id, text) in batch {
            if cancel.is_cancelled() {
                break;
            }
            if text.trim().is_empty() {
                continue;
            }
            // Chunked because a mail body's single vector would describe the signature block as
            // much as the point. A short item is one chunk, so nothing changes for a sensor event.
            let spans = crate::context::chunking::chunk(
                &text,
                crate::context::chunking::DEFAULT_CHUNK_BYTES,
                crate::context::chunking::DEFAULT_OVERLAP_BYTES,
            );
            // Stale chunks first: a re-chunked item has a different
            // number of passages, and leaving the old ones would keep
            // scoring spans that no longer describe anything.
            if let Err(e) = index.remove(Corpus::Context, &row_id).await {
                tracing::warn!("[context-index] could not clear old chunks: {e}");
            }
            let mut stored = 0usize;
            for (ix, span) in spans.iter().enumerate() {
                let Some(passage) = span.slice(&text) else {
                    continue;
                };
                match embedder.embed(passage).await {
                    Ok(vector) => {
                        let entry = crate::context::vector_index::VectorEntry {
                            corpus: Corpus::Context,
                            row_id: row_id.clone(),
                            chunk_ix: ix as i64,
                            chunk_span: Some((span.start as i64, span.len as i64)),
                            model_id: model_id.clone(),
                            vector,
                            // Left None: the adapter's own write-through
                            // stamps the ingest time, and a rev invented here
                            // could disagree with it and re-stale forever.
                            source_rev: None,
                        };
                        if let Err(e) = index.upsert(&entry).await {
                            tracing::warn!("[context-index] store failed: {e}");
                        } else {
                            stored += 1;
                        }
                    }
                    Err(e) => tracing::warn!("[context-index] embed failed: {e}"),
                }
            }
            if stored > 0 {
                // Counted per ITEM, not per chunk: the report answers
                // "how much of the corpus is reachable", and a reader
                // comparing it against the item count would otherwise
                // see more indexed than exist.
                report.context_indexed += 1;
            }
        }
        if budget == IndexBudget::OneBatch || worked < CONTEXT_BATCH {
            break;
        }
    }

    // 2c. Memory fragments that have no vector. The store's OWN column, not the
    //     index's: `search_similar` reads `memories.embedding` directly, so a row
    //     with a NULL there is unreachable semantically no matter how healthy the
    //     index is, and step 1's adoption cannot help because there is nothing to
    //     copy. The only other filler is `run_backfill`, spawned once at boot --
    //     so before this step, every row consolidation minted, every row
    //     `update_content` rewrote, and every row whose embed failed stayed
    //     invisible until the next restart.
    if !cancel.is_cancelled() {
        report.memories_indexed = run_memory_embedding_sweep(
            memories,
            embedder,
            BACKFILL_BATCH_SIZE,
            BACKFILL_BATCH_PAUSE_MS,
            cancel,
            match budget {
                IndexBudget::OneBatch => 1,
                IndexBudget::UntilDone => MAX_BATCHES,
            },
        )
        .await;
    }

    // 3. Orphans. Deliberately AFTER the writes: pruning first would delete rows
    //    that step 1 is about to legitimately re-create, doing the same work
    //    twice on every pass.
    if !cancel.is_cancelled() {
        match index.prune_orphans().await {
            Ok(n) => report.orphans_pruned = n,
            Err(e) => tracing::warn!("orphan prune failed: {e:#}"),
        }
    }

    // 4. Report what is still wrong. A model change must be LOUD: every stored
    //    vector from the old model is meaningless while still scoring plausibly,
    //    and re-embedding a household can take hours on a Jetson. "Retrieval
    //    quietly got worse" is undiagnosable, so it gets a WARN with the count.
    match index.health(&model_id).await {
        Ok(h) => {
            report.still_missing = h.missing;
            report.mismatched = h.mismatched;
            if h.mismatched > 0 {
                tracing::warn!(
                    model_id = %model_id,
                    mismatched = h.mismatched,
                    "vectors from a different embedding model are present and unusable; \
                     retrieval excludes them until they are re-embedded"
                );
            }
            if h.missing > 0 {
                tracing::info!(missing = h.missing, "rows still awaiting a vector");
            }
        }
        Err(e) => tracing::warn!("index health read failed: {e:#}"),
    }

    if report.adopted > 0
        || report.summaries_indexed > 0
        || report.context_indexed > 0
        || report.memories_indexed > 0
        || report.orphans_pruned > 0
    {
        tracing::info!(
            adopted = report.adopted,
            summaries = report.summaries_indexed,
            context = report.context_indexed,
            memories = report.memories_indexed,
            orphans_pruned = report.orphans_pruned,
            "personal-context index maintenance pass complete"
        );
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::vector_index::{
        CorpusHealth, IndexHealth, ResolvedHit, VectorEntry, VectorHit,
    };
    use crate::user_data::domain::profile::ProfileScope;
    use anyhow::Result;
    use async_trait::async_trait;
    use std::sync::Mutex;

    #[derive(Default)]
    struct SpyIndex {
        calls: Mutex<Vec<String>>,
        /// Rows still wanting a vector. Drained by each `needs_embedding_with_text`
        /// so the spy behaves like the real index: an embedded row stops being
        /// returned, which is what makes re-asking a valid way to page.
        backlog: Mutex<usize>,
    }

    #[async_trait]
    impl VectorIndex for SpyIndex {
        async fn upsert(&self, _e: &VectorEntry) -> Result<()> {
            Ok(())
        }
        async fn remove(&self, _c: Corpus, _r: &str) -> Result<()> {
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
        async fn needs_embedding(&self, _c: Corpus, _m: &str, _l: usize) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn needs_embedding_with_text(
            &self,
            _c: Corpus,
            _m: &str,
            l: usize,
        ) -> Result<Vec<(String, String)>> {
            let mut left = self.backlog.lock().unwrap();
            let take = (*left).min(l);
            *left -= take;
            Ok((0..take)
                .map(|i| (format!("row-{i}"), "the invoice is attached".to_string()))
                .collect())
        }
        async fn backfill_from_source(&self, c: Corpus, _m: &str, _d: usize) -> Result<u64> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("adopt:{}", c.as_str()));
            Ok(2)
        }
        async fn prune_orphans(&self) -> Result<u64> {
            self.calls.lock().unwrap().push("prune".into());
            Ok(1)
        }
        async fn health(&self, _m: &str) -> Result<IndexHealth> {
            self.calls.lock().unwrap().push("health".into());
            // Totals are the per-corpus sums, as a real index reports them, with Summary as the
            // wholly-dead corpus. A stub whose numbers did not add up would let a caller that
            // quietly stopped reading one of them still pass.
            Ok(IndexHealth {
                matching: 5,
                mismatched: 3,
                missing: 1,
                per_corpus: vec![
                    CorpusHealth {
                        corpus: Corpus::Memory,
                        rows: 7,
                        source_rows: 9,
                        indexed_rows: 4,
                        missing_rows: 1,
                        mismatched: 2,
                    },
                    CorpusHealth {
                        corpus: Corpus::Context,
                        rows: 2,
                        source_rows: 2,
                        indexed_rows: 1,
                        missing_rows: 0,
                        mismatched: 1,
                    },
                    CorpusHealth {
                        corpus: Corpus::Summary,
                        // Zero qualifying against a non-empty table: the
                        // structurally-dead shape this whole surface exists to
                        // make legible.
                        rows: 0,
                        source_rows: 11,
                        indexed_rows: 0,
                        missing_rows: 0,
                        mismatched: 0,
                    },
                ],
            })
        }
    }

    struct StubEmbedder;
    #[async_trait]
    impl EmbeddingProvider for StubEmbedder {
        async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            Ok(vec![1.0])
        }
        fn dimensions(&self) -> usize {
            1
        }
        fn model_id(&self) -> String {
            "stub".into()
        }
    }

    use crate::user_data::mocks::mock_memory::MockMemoryRepository;
    use crate::user_data::mocks::mock_session::InMemorySessionStorage;

    /// Adoption must come BEFORE the prune, or the prune deletes rows adoption
    /// is about to re-create and every pass does the same work twice.
    #[tokio::test]
    async fn a_pass_adopts_before_it_prunes_and_reports_health_last() {
        let spy = Arc::new(SpyIndex::default());
        let index: Arc<dyn VectorIndex> = spy.clone();
        let report = run_index_maintenance(
            &index,
            &InMemorySessionStorage::new(),
            &MockMemoryRepository::new(),
            &StubEmbedder,
            &CancellationToken::new(),
            IndexBudget::OneBatch,
        )
        .await;

        let calls = spy.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec!["adopt:memory", "adopt:context", "prune", "health"],
            "maintenance ran its steps in the wrong order"
        );
        assert_eq!(report.adopted, 4);
        assert_eq!(report.orphans_pruned, 1);
        assert_eq!(
            report.mismatched, 3,
            "a model change must be reported, not hidden"
        );
        assert_eq!(report.still_missing, 1);
    }

    /// Measured on a real pond: 986 items wanting a vector, a batch of 64, and
    /// scheduled passes that refuse until somebody chats. Pressing Reindex
    /// cleared 480 vectors and put 64 back.
    #[tokio::test]
    async fn a_requested_pass_drains_a_backlog_bigger_than_one_batch() {
        let spy = Arc::new(SpyIndex {
            backlog: Mutex::new(CONTEXT_BATCH * 3 + 7),
            ..Default::default()
        });
        let index: Arc<dyn VectorIndex> = spy.clone();
        let report = run_index_maintenance(
            &index,
            &InMemorySessionStorage::new(),
            &MockMemoryRepository::new(),
            &StubEmbedder,
            &CancellationToken::new(),
            IndexBudget::UntilDone,
        )
        .await;
        assert_eq!(
            report.context_indexed,
            CONTEXT_BATCH * 3 + 7,
            "an asked-for pass must finish the corpus, not take one bite"
        );
        assert_eq!(*spy.backlog.lock().unwrap(), 0);
    }

    /// The background pass stays small, or a household's next turn queues
    /// behind the whole mailbox.
    #[tokio::test]
    async fn a_scheduled_pass_takes_one_bite() {
        let spy = Arc::new(SpyIndex {
            backlog: Mutex::new(CONTEXT_BATCH * 3),
            ..Default::default()
        });
        let index: Arc<dyn VectorIndex> = spy.clone();
        let report = run_index_maintenance(
            &index,
            &InMemorySessionStorage::new(),
            &MockMemoryRepository::new(),
            &StubEmbedder,
            &CancellationToken::new(),
            IndexBudget::OneBatch,
        )
        .await;
        assert_eq!(report.context_indexed, CONTEXT_BATCH);
    }

    /// The deadlock this exemption exists for: mail arrives from a connector,
    /// not a conversation, so a pond that is synced and browsed but never
    /// chatted with never satisfies the lane's activity gate.
    #[test]
    fn an_idle_pond_indexes_itself_once_after_boot() {
        let tick = plan_sweep(false, false);
        assert!(
            tick.exempt_from_activity_gate,
            "a pond nobody has chatted with would never index at all"
        );
        assert_eq!(
            tick.budget,
            IndexBudget::UntilDone,
            "an exemption that indexed one batch would leave the pond unsearchable anyway"
        );
    }

    /// Once, not every tick. After the first pass the gate applies again.
    #[test]
    fn a_later_scheduled_pass_is_gated_again_and_takes_one_bite() {
        let tick = plan_sweep(false, true);
        assert!(
            !tick.exempt_from_activity_gate,
            "the exemption is per boot, not per tick"
        );
        assert_eq!(tick.budget, IndexBudget::OneBatch);
    }

    #[test]
    fn a_busy_pond_still_takes_one_bite_per_scheduled_pass() {
        // A used pond satisfies the lane's own gate; the sweep needs no
        // exemption and must not claim one.
        let tick = plan_sweep(false, true);
        assert!(!tick.exempt_from_activity_gate);
        assert_eq!(
            tick.budget,
            IndexBudget::OneBatch,
            "background work must not queue a household's turn behind the mailbox"
        );
    }

    /// Somebody watching an empty panel gets the whole corpus, always.
    #[test]
    fn a_requested_pass_is_always_exhaustive() {
        for indexed in [false, true] {
            let tick = plan_sweep(true, indexed);
            assert!(tick.exempt_from_activity_gate);
            assert_eq!(tick.budget, IndexBudget::UntilDone);
        }
    }

    /// A memory row with no vector, which NOTHING else repairs after boot.
    ///
    /// The startup backfill is a one-shot spawn; adoption only copies vectors
    /// that already exist. So a fragment consolidation minted at 02:00 was
    /// unreachable by semantic search until somebody restarted the pond.
    #[tokio::test]
    async fn a_pass_embeds_a_memory_row_the_startup_backfill_has_already_missed() {
        use crate::user_data::domain::memory::{MemoryFragment, MemorySegment};

        let memories = MockMemoryRepository::new();
        memories
            .add(MemoryFragment::from_extraction(
                "minted-after-boot".into(),
                None,
                "Jerry keeps the sourdough starter in the pantry.".into(),
                MemorySegment::Preference,
                0.7,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(memories.search_unembedded(8).await.unwrap().len(), 1);

        let spy = Arc::new(SpyIndex::default());
        let index: Arc<dyn VectorIndex> = spy.clone();
        let report = run_index_maintenance(
            &index,
            &InMemorySessionStorage::new(),
            &memories,
            &StubEmbedder,
            &CancellationToken::new(),
            IndexBudget::OneBatch,
        )
        .await;

        assert_eq!(report.memories_indexed, 1);
        assert!(
            memories.search_unembedded(8).await.unwrap().is_empty(),
            "the row is still unembedded, so semantic search still cannot see it"
        );
    }

    /// The memory sweep obeys the same one-bite rule as the context step, or a
    /// household with a large store hands the lane slot to the embedder.
    #[tokio::test]
    async fn a_scheduled_pass_takes_one_bite_of_the_memory_backlog() {
        use crate::user_data::domain::memory::{MemoryFragment, MemorySegment};

        let memories = MockMemoryRepository::new();
        let backlog = BACKFILL_BATCH_SIZE + 5;
        for i in 0..backlog {
            memories
                .add(MemoryFragment::from_extraction(
                    format!("row-{i}"),
                    None,
                    format!("Jerry waters the greenhouse bed number {i}."),
                    MemorySegment::Knowledge,
                    0.5,
                    None,
                ))
                .await
                .unwrap();
        }

        let spy = Arc::new(SpyIndex::default());
        let index: Arc<dyn VectorIndex> = spy.clone();
        let report = run_index_maintenance(
            &index,
            &InMemorySessionStorage::new(),
            &memories,
            &StubEmbedder,
            &CancellationToken::new(),
            IndexBudget::OneBatch,
        )
        .await;

        assert_eq!(report.memories_indexed, BACKFILL_BATCH_SIZE);
        assert_eq!(
            memories.search_unembedded(backlog).await.unwrap().len(),
            backlog - BACKFILL_BATCH_SIZE,
            "the rest must be left for the next tick, not swept in one go"
        );
    }

    /// A member's turn must be able to take the CPU back mid-pass.
    #[tokio::test]
    async fn a_cancelled_pass_does_no_work() {
        let spy = Arc::new(SpyIndex::default());
        let index: Arc<dyn VectorIndex> = spy.clone();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let report = run_index_maintenance(
            &index,
            &InMemorySessionStorage::new(),
            &MockMemoryRepository::new(),
            &StubEmbedder,
            &cancel,
            IndexBudget::OneBatch,
        )
        .await;
        assert_eq!(report, MaintenanceReport::default());
    }
}
