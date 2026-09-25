//! MemoryRepository port — driven port for semantic memory persistence.

use crate::user_data::domain::memory::{
    MemoryEdge, MemoryEvent, MemoryEventKind, MemoryFragment, MemoryGraph, MemoryLifecycle,
    MemorySegment,
};
use crate::user_data::domain::profile::ProfileScope;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: memory fragment persistence and similarity search.
#[async_trait]
pub trait MemoryRepository: Send + Sync {
    /// Persist a new memory fragment.
    async fn add(&self, fragment: MemoryFragment) -> Result<()>;

    /// Return the `limit` most recent fragments in scope, newest first.
    async fn search_recent(
        &self,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>>;

    /// Top-`limit` fragments by cosine similarity, or `search_recent` when none are embedded.
    async fn search_similar(
        &self,
        query_embedding: &[f32],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>>;

    /// Delete a memory fragment by ID.
    async fn delete(&self, id: &str) -> Result<()>;

    /// Fragments owned by exactly this member; excludes the shared `IS NULL` rows, which
    /// survive the member and must not be reported as deleted.
    async fn count_for_profile(&self, _profile_id: &str) -> Result<u64> {
        Ok(0)
    }

    /// Up to `limit` active fragments with no embedding, for the startup embedding backfill.
    async fn search_unembedded(&self, _limit: usize) -> Result<Vec<MemoryFragment>> {
        Ok(vec![])
    }

    /// Up to `limit` active fragments whose vector isn't `expected_dims` wide (another model's).
    /// Width stands in for a model id, which isn't persisted: GGUF is 768, fastembed 384.
    async fn search_stale_dimension(
        &self,
        _expected_dims: usize,
        _limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        Ok(vec![])
    }

    /// Attach (or replace) the embedding vector of an existing fragment.
    async fn update_embedding(&self, _id: &str, _embedding: &[f32]) -> Result<()> {
        Ok(())
    }

    /// Active memories matching any keyword, by importance (highest first) then recency.
    async fn search_by_content(
        &self,
        keywords: &[String],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        let _ = keywords;
        self.search_recent(scope, limit).await
    }

    // ── Segment-aware methods (default no-op impls) ──────────────────────

    /// Increment access_count and update last_accessed_at for a memory.
    async fn record_access(&self, _id: &str) -> Result<()> {
        Ok(())
    }

    /// Replace a memory's text in place, keeping its identity (id, created_at, edges).
    /// Clears the embedding, which would still score for the old words; the sweep re-embeds it.
    async fn update_content(&self, _id: &str, _content: &str) -> Result<()> {
        anyhow::bail!("this store cannot edit a memory's text")
    }

    /// Update the lifecycle status of a memory.
    async fn update_lifecycle(&self, _id: &str, _lifecycle: MemoryLifecycle) -> Result<()> {
        Ok(())
    }

    /// Search active memories by segment.
    async fn search_by_segment(
        &self,
        _segment: MemorySegment,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        self.search_recent(scope, limit).await
    }

    /// Active memories with decay fields, for the cleanup service's archive/prune scoring.
    async fn search_scoreable(&self, scope: &ProfileScope) -> Result<Vec<MemoryFragment>> {
        self.search_recent(scope, 1000).await
    }

    /// Batch update lifecycle for multiple memories at once.
    async fn batch_update_lifecycle(&self, _updates: &[(String, MemoryLifecycle)]) -> Result<()> {
        Ok(())
    }

    /// Mark a memory as superseded by another (for consolidation).
    async fn mark_superseded(&self, _id: &str, _superseded_by: &str) -> Result<()> {
        Ok(())
    }

    // ── Graph edge methods (default no-op impls) ────────────────────────

    /// Persist a directed edge between two memories.
    async fn add_edge(&self, _edge: MemoryEdge) -> Result<()> {
        Ok(())
    }

    /// Return all edges originating from `memory_id`.
    async fn get_edges_from(&self, _memory_id: &str) -> Result<Vec<MemoryEdge>> {
        Ok(vec![])
    }

    /// Return all edges pointing to `memory_id`.
    async fn get_edges_to(&self, _memory_id: &str) -> Result<Vec<MemoryEdge>> {
        Ok(vec![])
    }

    /// The subgraph (nodes + edges) within `max_depth` hops of `root_ids`.
    async fn get_subgraph(&self, _root_ids: &[String], _max_depth: u32) -> Result<MemoryGraph> {
        Ok(MemoryGraph {
            nodes: vec![],
            edges: vec![],
        })
    }

    // ── Audit log methods (default no-op impls) ─────────────────────────

    /// Record a memory lifecycle event for audit purposes.
    async fn log_event(
        &self,
        _kind: MemoryEventKind,
        _memory_id: &str,
        _session_id: Option<&str>,
        _data: Option<&str>,
    ) -> Result<()> {
        Ok(())
    }

    /// Retrieve memory audit events, optionally filtered by memory ID.
    async fn get_events(
        &self,
        _memory_id: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<MemoryEvent>> {
        Ok(vec![])
    }

    /// Update the segment and importance of a memory (for recategorization).
    async fn update_segment(
        &self,
        _id: &str,
        _segment: MemorySegment,
        _importance: f32,
    ) -> Result<()> {
        Ok(())
    }

    // ── Consolidation run audit (default no-op) ────────────────────────────

    /// Record a completed consolidation run with full details.
    async fn log_consolidation_run(
        &self,
        _mode: &str,
        _memory_count: usize,
        _accepted: usize,
        _rejected: usize,
        _duration_ms: u64,
        _details: Option<&str>,
    ) -> Result<i64> {
        Ok(0)
    }
}
