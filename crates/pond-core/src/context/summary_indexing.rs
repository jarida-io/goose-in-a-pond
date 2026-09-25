//! Indexes conversation summaries as a restartable sweep: their writers are already LLM calls.
//! A rewritten summary is detected by `source_rev` vs `rolling_summary_updated_at`.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::context::vector_index::{Corpus, VectorEntry, VectorIndex};
use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::ports::session_storage::SessionStorage;

/// Summaries embedded per batch before a pause, which yields the Jetson's CPU to inference.
pub const SUMMARY_BATCH_SIZE: usize = 16;

/// Pause between batches, in milliseconds.
pub const SUMMARY_BATCH_PAUSE_MS: u64 = 250;

/// Embed and index every summary [`VectorIndex::needs_embedding`] reports; returns how many.
pub async fn run_summary_indexing(
    storage: &dyn SessionStorage,
    embedder: &dyn EmbeddingProvider,
    index: &Arc<dyn VectorIndex>,
    cancel: &CancellationToken,
    batch_size: usize,
    pause_ms: u64,
) -> usize {
    let model_id = embedder.model_id();
    let mut indexed = 0usize;

    loop {
        if cancel.is_cancelled() {
            break;
        }
        let batch = match index
            .needs_embedding(Corpus::Summary, &model_id, batch_size)
            .await
        {
            Ok(rows) if rows.is_empty() => break,
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("[summary-index] fetch failed: {e}");
                break;
            }
        };

        let mut progressed = false;
        for session_id in &batch {
            if cancel.is_cancelled() {
                break;
            }
            // Read together, or a racing rewrite stamps a newer revision than the text.
            let (summary, rev) = match storage.get_rolling_summary_with_revision(session_id).await {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!(session_id = %session_id, "[summary-index] read failed: {e}");
                    continue;
                }
            };
            let Some(text) = summary.filter(|s| !s.trim().is_empty()) else {
                // Blanked since the query ran; the next sweep won't return it.
                continue;
            };
            match embedder.embed(&text).await {
                Ok(vector) => {
                    // Whole: a summary is already a compression; chunking it gains nothing.
                    let entry = VectorEntry::whole(
                        Corpus::Summary,
                        session_id.clone(),
                        model_id.clone(),
                        vector,
                        rev,
                    );
                    match index.upsert(&entry).await {
                        Ok(()) => {
                            indexed += 1;
                            progressed = true;
                        }
                        Err(e) => {
                            tracing::warn!(session_id = %session_id, "[summary-index] store failed: {e}")
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(session_id = %session_id, "[summary-index] embed failed: {e}")
                }
            }
        }

        // Every row in the batch failed and would come back forever; stop rather than spin.
        if !progressed {
            tracing::warn!("[summary-index] no progress in a batch — stopping");
            break;
        }
        if pause_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(pause_ms)).await;
        }
    }

    if indexed > 0 {
        tracing::info!("[summary-index] indexed {indexed} conversation summaries");
    }
    indexed
}
