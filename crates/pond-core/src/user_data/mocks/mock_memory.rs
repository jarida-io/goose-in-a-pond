//! In-memory mock implementations of `EmbeddingProvider` and `MemoryRepository`.

use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::domain::memory::{
    cosine_similarity, MemoryEvent, MemoryEventKind, MemoryFragment, MemoryLifecycle, MemorySegment,
};
use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::ports::memory_repository::MemoryRepository;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Mock embedding provider — always returns a zero vector of `dims` length.
pub struct MockEmbeddingProvider {
    pub dims: usize,
}

impl MockEmbeddingProvider {
    pub fn new() -> Self {
        Self { dims: 384 }
    }
}

impl Default for MockEmbeddingProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// In-memory twin of `sqlite_memory::scope_sql`.
///
/// These two must agree or every mock-backed test is testing a fiction. The
/// rule: an `Owner` sees their own rows plus unattributed shared ones, a
/// `Household` sees everything, and a `Guest` sees nothing.
fn scope_matches(f: &MemoryFragment, scope: &ProfileScope) -> bool {
    match scope {
        ProfileScope::Owner(id) => {
            f.profile_id.as_deref() == Some(id.as_str()) || f.profile_id.is_none()
        }
        ProfileScope::Household => true,
        ProfileScope::Guest => false,
    }
}

#[async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
        Ok(vec![0.0_f32; self.dims])
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}

/// One row of the mock's `consolidation_runs` table:
/// (mode, memory_count, accepted, rejected).
pub type RecordedConsolidationRun = (String, usize, usize, usize);

/// Mock memory repository — stores fragments in-memory.
///
/// `search_similar` mirrors the SQLite adapter: cosine over rows that actually
/// carry an embedding, falling back to `search_recent` when none do.
///
/// The lifecycle / supersede / segment / event writes are recorded rather than
/// dropped so consolidation's correction-safety guards can be asserted on.
pub struct MockMemoryRepository {
    fragments: Arc<RwLock<Vec<MemoryFragment>>>,
    lifecycle_updates: Arc<RwLock<Vec<(String, MemoryLifecycle)>>>,
    superseded: Arc<RwLock<Vec<(String, String)>>>,
    segment_updates: Arc<RwLock<Vec<(String, MemorySegment, f32)>>>,
    /// Recorded in full rather than as a projection, because the batch
    /// extraction engine READS its own audit rows back: the window key it logs
    /// on a finished window is what stops a re-walk paying for stretches it has
    /// already mined. A mock whose `get_events` returned nothing would make that
    /// guard untestable and, worse, make it look tested.
    events: Arc<RwLock<Vec<MemoryEvent>>>,
    consolidation_runs: Arc<RwLock<Vec<RecordedConsolidationRun>>>,
}

impl MockMemoryRepository {
    pub fn new() -> Self {
        Self {
            fragments: Arc::new(RwLock::new(Vec::new())),
            lifecycle_updates: Arc::new(RwLock::new(Vec::new())),
            superseded: Arc::new(RwLock::new(Vec::new())),
            segment_updates: Arc::new(RwLock::new(Vec::new())),
            events: Arc::new(RwLock::new(Vec::new())),
            consolidation_runs: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Lifecycle transitions recorded, in order: (memory id, new lifecycle).
    pub async fn lifecycle_updates(&self) -> Vec<(String, MemoryLifecycle)> {
        self.lifecycle_updates.read().await.clone()
    }

    /// Supersede links recorded, in order: (superseded id, superseding id).
    pub async fn superseded(&self) -> Vec<(String, String)> {
        self.superseded.read().await.clone()
    }

    /// Recategorizations recorded, in order: (id, new segment, new importance).
    pub async fn segment_updates(&self) -> Vec<(String, MemorySegment, f32)> {
        self.segment_updates.read().await.clone()
    }

    /// Audit events recorded, in order: (kind, memory id, data).
    ///
    /// The projection the existing assertions are written against; the rows
    /// themselves come back through [`MemoryRepository::get_events`].
    pub async fn events(&self) -> Vec<(MemoryEventKind, String, Option<String>)> {
        self.events
            .read()
            .await
            .iter()
            .map(|e| (e.event_kind.clone(), e.memory_id.clone(), e.data.clone()))
            .collect()
    }

    /// Consolidation audit rows recorded, in order.
    pub async fn consolidation_runs(&self) -> Vec<RecordedConsolidationRun> {
        self.consolidation_runs.read().await.clone()
    }
}

impl Default for MockMemoryRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryRepository for MockMemoryRepository {
    async fn add(&self, fragment: MemoryFragment) -> Result<()> {
        self.fragments.write().await.push(fragment);
        Ok(())
    }

    async fn search_recent(
        &self,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        let fragments = self.fragments.read().await;
        let mut results: Vec<MemoryFragment> = fragments
            .iter()
            .filter(|f| scope_matches(f, scope))
            .cloned()
            .collect();
        // newest first, then truncate
        results.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        results.truncate(limit);
        Ok(results)
    }

    async fn search_similar(
        &self,
        query_embedding: &[f32],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        let scored: Vec<(f32, MemoryFragment)> = {
            let fragments = self.fragments.read().await;
            fragments
                .iter()
                .filter(|f| is_active(f))
                .filter(|f| scope_matches(f, scope))
                .filter_map(|f| {
                    let emb = f.embedding.as_ref()?;
                    Some((cosine_similarity(query_embedding, emb), f.clone()))
                })
                .collect()
        };

        // Same contract as the SQLite adapter: with nothing embedded at all,
        // degrade to recency rather than returning an empty result.
        if scored.is_empty() {
            return self.search_recent(scope, limit).await;
        }

        let mut scored = scored;
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, f)| f).collect())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.fragments.write().await.retain(|f| f.id != id);
        Ok(())
    }

    async fn search_unembedded(&self, limit: usize) -> Result<Vec<MemoryFragment>> {
        let fragments = self.fragments.read().await;
        Ok(fragments
            .iter()
            .filter(|f| f.embedding.is_none() && is_active(f))
            .take(limit)
            .cloned()
            .collect())
    }

    async fn update_embedding(&self, id: &str, embedding: &[f32]) -> Result<()> {
        let mut fragments = self.fragments.write().await;
        if let Some(f) = fragments.iter_mut().find(|f| f.id == id) {
            f.embedding = Some(embedding.to_vec());
        }
        Ok(())
    }

    // ── Consolidation-observable writes ────────────────────────────────────
    // Recorded (not no-op'd) so the correction-safety guards are assertable.

    async fn update_lifecycle(&self, id: &str, lifecycle: MemoryLifecycle) -> Result<()> {
        self.lifecycle_updates
            .write()
            .await
            .push((id.to_string(), lifecycle.clone()));
        let mut fragments = self.fragments.write().await;
        if let Some(f) = fragments.iter_mut().find(|f| f.id == id) {
            f.lifecycle = Some(lifecycle);
        }
        Ok(())
    }

    async fn mark_superseded(&self, id: &str, superseded_by: &str) -> Result<()> {
        self.superseded
            .write()
            .await
            .push((id.to_string(), superseded_by.to_string()));
        let mut fragments = self.fragments.write().await;
        if let Some(f) = fragments.iter_mut().find(|f| f.id == id) {
            f.superseded_by = Some(superseded_by.to_string());
            f.lifecycle = Some(MemoryLifecycle::Merged);
        }
        Ok(())
    }

    async fn update_segment(
        &self,
        id: &str,
        segment: MemorySegment,
        importance: f32,
    ) -> Result<()> {
        self.segment_updates
            .write()
            .await
            .push((id.to_string(), segment.clone(), importance));
        let mut fragments = self.fragments.write().await;
        if let Some(f) = fragments.iter_mut().find(|f| f.id == id) {
            f.segment = Some(segment);
            f.importance = Some(importance);
        }
        Ok(())
    }

    async fn log_event(
        &self,
        kind: MemoryEventKind,
        memory_id: &str,
        session_id: Option<&str>,
        data: Option<&str>,
    ) -> Result<()> {
        let mut events = self.events.write().await;
        let id = events.len() as i64 + 1;
        events.push(MemoryEvent {
            id,
            event_kind: kind,
            memory_id: memory_id.to_string(),
            session_id: session_id.map(str::to_string),
            data: data.map(str::to_string),
            created_at: chrono::Utc::now().to_rfc3339(),
        });
        Ok(())
    }

    /// Newest first, like the SQLite adapter, so a caller that takes the first
    /// match takes the most recent one in both.
    async fn get_events(&self, memory_id: Option<&str>, limit: usize) -> Result<Vec<MemoryEvent>> {
        let events = self.events.read().await;
        Ok(events
            .iter()
            .rev()
            .filter(|e| memory_id.is_none_or(|id| e.memory_id == id))
            .take(limit)
            .cloned()
            .collect())
    }

    async fn log_consolidation_run(
        &self,
        mode: &str,
        memory_count: usize,
        accepted: usize,
        rejected: usize,
        _duration_ms: u64,
        _details: Option<&str>,
    ) -> Result<i64> {
        let mut runs = self.consolidation_runs.write().await;
        runs.push((mode.to_string(), memory_count, accepted, rejected));
        Ok(runs.len() as i64)
    }
}

/// Pre-lifecycle rows carry `None`, which the SQLite adapter treats as active.
fn is_active(fragment: &MemoryFragment) -> bool {
    !matches!(
        fragment.lifecycle,
        Some(MemoryLifecycle::Archived) | Some(MemoryLifecycle::Merged)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_embedding_returns_zero_vector() {
        let ep = MockEmbeddingProvider::new();
        let v = ep.embed("hello").await.unwrap();
        assert_eq!(v.len(), 384);
        assert!(v.iter().all(|&x| x == 0.0));
    }

    #[tokio::test]
    async fn mock_memory_add_and_search_recent() {
        let repo = MockMemoryRepository::new();
        let frag = MemoryFragment::from_chat(
            "id1".to_string(),
            Some("profile1".to_string()),
            None,
            "Hello world".to_string(),
        );
        repo.add(frag).await.unwrap();
        let results = repo
            .search_recent(&ProfileScope::Owner("profile1".into()), 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "Hello world");
    }

    #[tokio::test]
    async fn mock_memory_delete() {
        let repo = MockMemoryRepository::new();
        let frag = MemoryFragment::from_chat("del-id".to_string(), None, None, "temp".to_string());
        repo.add(frag).await.unwrap();
        repo.delete("del-id").await.unwrap();
        assert!(repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap()
            .is_empty());
    }
}
