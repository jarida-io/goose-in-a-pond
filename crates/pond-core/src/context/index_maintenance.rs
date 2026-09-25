//! One restartable sweep repairs every way the personal-context index drifts from its stores.
//! Orphans come from cross-file deletes WAL can't make atomic; batches cancel so a turn wins.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::context::summary_indexing::{run_summary_indexing, SUMMARY_BATCH_PAUSE_MS};
use crate::context::vector_index::{Corpus, VectorIndex};
use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::ports::session_storage::SessionStorage;

/// What one maintenance pass did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaintenanceReport {
    /// Existing vectors taught to the index without re-embedding.
    pub adopted: u64,
    /// Summaries embedded.
    pub summaries_indexed: usize,
    /// Context items embedded that arrived without a vector.
    pub context_indexed: usize,
    /// Index rows whose source row is gone.
    pub orphans_pruned: u64,
    /// Rows still lacking a usable vector when the pass finished.
    pub still_missing: u64,
    /// Vectors from another model, still awaiting re-embedding.
    pub mismatched: u64,
}

/// How much backlog one pass may work through.
/// Scheduled passes take one bite so turns don't queue; requested ones finish, or Reindex lies.
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
    /// This job's own exemption; relaxing the lane-wide activity flag would qualify every job.
    pub exempt_from_activity_gate: bool,
    /// How much of the backlog this tick may work through.
    pub budget: IndexBudget,
}

/// Decide whether a sweep tick may run, and how much it may do.
/// The first pass after boot is exempt and exhaustive, since connector mail isn't activity.
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

/// Items per embed batch; exhaustive passes stay batched so cancellation lands promptly.
const CONTEXT_BATCH: usize = 64;

/// Bounds an exhaustive pass, since an item that always fails to embed never drains.
const MAX_BATCHES: usize = 500;

pub async fn run_index_maintenance(
    index: &Arc<dyn VectorIndex>,
    storage: &dyn SessionStorage,
    embedder: &dyn EmbeddingProvider,
    cancel: &CancellationToken,
    budget: IndexBudget,
) -> MaintenanceReport {
    let mut report = MaintenanceReport::default();
    let model_id = embedder.model_id();
    let dims = embedder.dimensions();

    // 1. Adoption first: copying existing vectors in pure SQL is free and shrinks later steps.
    for corpus in [Corpus::Memory, Corpus::Context] {
        if cancel.is_cancelled() {
            return report;
        }
        match index.backfill_from_source(corpus, &model_id, dims).await {
            Ok(n) => report.adopted += n,
            Err(e) => tracing::warn!(corpus = corpus.as_str(), "index adoption failed: {e:#}"),
        }
    }

    // 2. Summaries, the only corpus with no stored vector of its own.
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

    // 2b. Context items that arrived without a vector; adoption can't reach them.
    let mut batches = 0usize;
    while !cancel.is_cancelled() && batches < MAX_BATCHES {
        batches += 1;
        // Re-ask, don't page: embedded rows drop out, so offsets over the shrinking set skip rows.
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
            let spans = crate::context::chunking::chunk(
                &text,
                crate::context::chunking::DEFAULT_CHUNK_BYTES,
                crate::context::chunking::DEFAULT_OVERLAP_BYTES,
            );
            // Clear old chunks first: a re-chunked item may have fewer passages.
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
                            // The adapter stamps this; a value set here could re-stale forever.
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
                // Per item, not per chunk, so it compares against the item count.
                report.context_indexed += 1;
            }
        }
        if budget == IndexBudget::OneBatch || worked < CONTEXT_BATCH {
            break;
        }
    }

    // 3. Orphans, after the writes: pruning first deletes rows step 1 would re-create.
    if !cancel.is_cancelled() {
        match index.prune_orphans().await {
            Ok(n) => report.orphans_pruned = n,
            Err(e) => tracing::warn!("orphan prune failed: {e:#}"),
        }
    }

    // 4. Report what is still wrong. WARN on a model change: old vectors still score plausibly.
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
        || report.orphans_pruned > 0
    {
        tracing::info!(
            adopted = report.adopted,
            summaries = report.summaries_indexed,
            context = report.context_indexed,
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
        /// Rows still wanting a vector; drained per call like the real index, so re-asking pages.
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
            // Totals are the per-corpus sums, as a real index reports them.
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
                        // Zero qualifying rows in a non-empty table: the structurally-dead shape.
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

    use crate::user_data::mocks::mock_session::InMemorySessionStorage;

    #[tokio::test]
    async fn a_pass_adopts_before_it_prunes_and_reports_health_last() {
        let spy = Arc::new(SpyIndex::default());
        let index: Arc<dyn VectorIndex> = spy.clone();
        let report = run_index_maintenance(
            &index,
            &InMemorySessionStorage::new(),
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
            &StubEmbedder,
            &CancellationToken::new(),
            IndexBudget::OneBatch,
        )
        .await;
        assert_eq!(report.context_indexed, CONTEXT_BATCH);
    }

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
        // A used pond passes the lane's own gate, so the sweep must not claim an exemption.
        let tick = plan_sweep(false, true);
        assert!(!tick.exempt_from_activity_gate);
        assert_eq!(
            tick.budget,
            IndexBudget::OneBatch,
            "background work must not queue a household's turn behind the mailbox"
        );
    }

    #[test]
    fn a_requested_pass_is_always_exhaustive() {
        for indexed in [false, true] {
            let tick = plan_sweep(true, indexed);
            assert!(tick.exempt_from_activity_gate);
            assert_eq!(tick.budget, IndexBudget::UntilDone);
        }
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
            &StubEmbedder,
            &cancel,
            IndexBudget::OneBatch,
        )
        .await;
        assert_eq!(report, MaintenanceReport::default());
    }
}
