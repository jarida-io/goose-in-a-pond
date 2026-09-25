//! SQLite-backed `MemoryRepository`; embeddings are stored as little-endian f32 BLOBs.

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use pond_core::context::vector_index::{Corpus, VectorEntry, VectorIndex};
use pond_core::user_data::domain::memory::{
    cosine_similarity, MemoryEvent, MemoryEventKind, MemoryFragment, MemoryLifecycle,
    MemorySegment, MemoryTier,
};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use serde_json;
use sqlx::{Pool, Sqlite};
use std::sync::Arc;

pub struct SqliteMemoryRepository {
    pool: Pool<Sqlite>,
    /// Optional; without it memories are still stored and the sweep indexes them later.
    index: Option<Arc<dyn VectorIndex>>,
    /// The embedder behind stored vectors; fragments don't carry it but the index needs it.
    model_id: Option<String>,
}

impl SqliteMemoryRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self {
            pool,
            index: None,
            model_id: None,
        }
    }

    /// Mirror every stored vector into the shared index. Here, not in a decorator: fragments
    /// arrive already redacted (secret vectors dropped), a decorator could miss defaulted
    /// port methods, and `pond memories add` runs in its own process. Sole SQL writer.
    pub fn with_vector_index(
        mut self,
        index: Arc<dyn VectorIndex>,
        model_id: Option<String>,
    ) -> Self {
        self.index = Some(index);
        self.model_id = model_id;
        self
    }

    /// Never fails the caller: the index is derived and the sweep repairs it.
    async fn mirror(&self, id: &str, embedding: Option<&[f32]>) {
        let Some(index) = &self.index else { return };
        let outcome = match (embedding, self.model_id.as_deref()) {
            (Some(vector), Some(model_id)) if !vector.is_empty() => {
                index
                    .upsert(&VectorEntry {
                        corpus: Corpus::Memory,
                        row_id: id.to_string(),
                        // Never chunked: a memory is a sentence or two.
                        chunk_ix: 0,
                        chunk_span: None,
                        model_id: model_id.to_string(),
                        vector: vector.to_vec(),
                        // No revision tracked: consolidation supersedes rather than edits.
                        source_rev: None,
                    })
                    .await
            }
            // No model id (e.g. the CLI): leave the entry to the sweep rather than strip it.
            (Some(vector), None) if !vector.is_empty() => return,
            // No vector: remove any stale entry (the redactor drops secret-bearing vectors).
            _ => index.remove(Corpus::Memory, id).await,
        };
        if let Err(e) = outcome {
            tracing::warn!(memory_id = %id, "vector index write failed: {e:#}");
        }
    }
}

// ── Embedding BLOB encoding ───────────────────────────────────────────────────

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

// ── Row helper ────────────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct FragmentRow {
    id: String,
    profile_id: Option<String>,
    session_id: Option<String>,
    content: String,
    embedding: Option<Vec<u8>>,
    source: String,
    tags: String,
    created_at: String,
    // ── Segment-aware columns (nullable for pre-migration rows) ───────
    segment: Option<String>,
    importance: Option<f64>, // SQLite REAL → f64
    tier: Option<String>,
    decay_rate: Option<f64>,
    access_count: i64,
    last_accessed_at: Option<String>,
    lifecycle: Option<String>,
    superseded_by: Option<String>,
    /// For correction memories: what wrong claim this corrects.
    corrects: Option<String>,
}

fn parse_dt(s: &str) -> chrono::DateTime<Utc> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|ndt| ndt.and_utc())
        .unwrap_or_else(|_| Utc::now())
}

fn parse_segment(s: &str) -> Option<MemorySegment> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

fn parse_tier(s: &str) -> Option<MemoryTier> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

fn parse_lifecycle(s: &str) -> Option<MemoryLifecycle> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

fn row_to_fragment(row: FragmentRow) -> MemoryFragment {
    let tags: Vec<String> = serde_json::from_str(&row.tags).unwrap_or_default();
    let embedding = row.embedding.as_deref().map(blob_to_vec);
    MemoryFragment {
        id: row.id,
        profile_id: row.profile_id,
        session_id: row.session_id,
        content: row.content,
        embedding,
        source: row.source,
        tags,
        created_at: parse_dt(&row.created_at),
        segment: row.segment.as_deref().and_then(parse_segment),
        importance: row.importance.map(|v| v as f32),
        tier: row.tier.as_deref().and_then(parse_tier),
        decay_rate: row.decay_rate.map(|v| v as f32),
        access_count: row.access_count as u32,
        last_accessed_at: row.last_accessed_at.as_deref().map(parse_dt),
        lifecycle: row.lifecycle.as_deref().and_then(parse_lifecycle),
        superseded_by: row.superseded_by,
        corrects: row.corrects,
    }
}

/// `AND …` fragment (or empty) for a [`ProfileScope`]; every scoped read goes through it.
///
/// - `Owner(id)`: own rows plus unattributed (shared) ones, so household facts stay visible.
/// - `Household`: no predicate.
/// - `Guest`: matches nothing, so a missed caller short-circuit fails closed.
fn scope_sql(scope: &ProfileScope) -> (&'static str, Option<&str>) {
    match scope {
        ProfileScope::Owner(id) => (
            "AND (profile_id = ? OR profile_id IS NULL)",
            Some(id.as_str()),
        ),
        ProfileScope::Household => ("", None),
        ProfileScope::Guest => ("AND 1 = 0", None),
    }
}

const SELECT_ALL: &str = "\
    id, profile_id, session_id, content, embedding, source, tags, created_at, \
    segment, importance, tier, decay_rate, access_count, last_accessed_at, \
    lifecycle, superseded_by, corrects";

fn segment_to_str(s: &MemorySegment) -> &'static str {
    match s {
        MemorySegment::Identity => "identity",
        MemorySegment::Preference => "preference",
        MemorySegment::Correction => "correction",
        MemorySegment::Relationship => "relationship",
        MemorySegment::Project => "project",
        MemorySegment::Knowledge => "knowledge",
        MemorySegment::Context => "context",
    }
}

fn lifecycle_to_str(l: &MemoryLifecycle) -> &'static str {
    match l {
        MemoryLifecycle::Active => "active",
        MemoryLifecycle::Archived => "archived",
        MemoryLifecycle::Merged => "merged",
    }
}

fn tier_to_str(t: &MemoryTier) -> &'static str {
    match t {
        MemoryTier::Short => "short",
        MemoryTier::Long => "long",
        MemoryTier::Permanent => "permanent",
    }
}

#[async_trait]
impl MemoryRepository for SqliteMemoryRepository {
    async fn add(&self, fragment: MemoryFragment) -> Result<()> {
        let tags_json = serde_json::to_string(&fragment.tags)?;
        let created_str = fragment.created_at.format("%Y-%m-%d %H:%M:%S").to_string();
        let embedding_blob = fragment.embedding.as_deref().map(vec_to_blob);
        let segment_str = fragment.segment.as_ref().map(segment_to_str);
        let tier_str = fragment.tier.as_ref().map(tier_to_str).or(Some("long"));
        let lifecycle_str = Some(
            fragment
                .lifecycle
                .as_ref()
                .map(lifecycle_to_str)
                .unwrap_or("active"),
        );
        let last_accessed_str = fragment
            .last_accessed_at
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string());

        sqlx::query(
            "INSERT INTO memory_fragments \
             (id, profile_id, session_id, content, embedding, source, tags, created_at, \
              segment, importance, tier, decay_rate, access_count, last_accessed_at, \
              lifecycle, superseded_by, corrects) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&fragment.id)
        .bind(&fragment.profile_id)
        .bind(&fragment.session_id)
        .bind(&fragment.content)
        .bind(embedding_blob)
        .bind(&fragment.source)
        .bind(&tags_json)
        .bind(&created_str)
        .bind(segment_str)
        .bind(fragment.importance.map(|v| v as f64))
        .bind(tier_str)
        .bind(fragment.decay_rate.map(|v| v as f64))
        .bind(fragment.access_count as i64)
        .bind(last_accessed_str)
        .bind(lifecycle_str)
        .bind(&fragment.superseded_by)
        .bind(&fragment.corrects)
        .execute(&self.pool)
        .await?;

        // Write-through after the row is durable, mirroring what the row holds (post-redaction).
        self.mirror(&fragment.id, fragment.embedding.as_deref())
            .await;
        Ok(())
    }

    async fn search_recent(
        &self,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        if scope.excludes_everything() {
            return Ok(vec![]);
        }
        let (filter, bind) = scope_sql(scope);
        let query = format!(
            "SELECT {SELECT_ALL} FROM memory_fragments \
             WHERE (lifecycle IS NULL OR lifecycle = 'active') {filter} \
             ORDER BY created_at DESC LIMIT ?"
        );
        let mut q = sqlx::query_as::<_, FragmentRow>(&query);
        if let Some(pid) = bind {
            q = q.bind(pid);
        }
        let rows: Vec<FragmentRow> = q.bind(limit as i64).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(row_to_fragment).collect())
    }

    async fn search_similar(
        &self,
        query_embedding: &[f32],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        if scope.excludes_everything() {
            return Ok(vec![]);
        }
        let (filter, bind) = scope_sql(scope);
        let query = format!(
            "SELECT {SELECT_ALL} FROM memory_fragments \
             WHERE embedding IS NOT NULL \
             AND (lifecycle IS NULL OR lifecycle = 'active') {filter}"
        );

        let rows: Vec<FragmentRow> = match bind {
            Some(pid) => {
                sqlx::query_as(&query)
                    .bind(pid)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => sqlx::query_as(&query).fetch_all(&self.pool).await?,
        };

        if rows.is_empty() {
            return self.search_recent(scope, limit).await;
        }

        let candidates = rows.len();
        let mut scored: Vec<(f32, MemoryFragment)> = rows
            .into_iter()
            .map(row_to_fragment)
            .filter_map(|f| {
                let emb = f.embedding.clone()?;
                // Other width = other model: its 0.0 cosine looks valid and would crowd out
                // results and stop the recency fallback below from firing.
                if emb.len() != query_embedding.len() {
                    return None;
                }
                let score = cosine_similarity(query_embedding, &emb);
                Some((score, f))
            })
            .collect();

        if scored.is_empty() {
            // Nothing comparable (e.g. embedding_provider changed): fall back to keyword recency.
            if candidates > 0 {
                tracing::warn!(
                    incomparable = candidates,
                    query_dims = query_embedding.len(),
                    "every embedded memory was produced by a different embedding model — \
                     falling back to recency. Re-embed the store or restore the previous \
                     embedding_provider."
                );
            }
            return self.search_recent(scope, limit).await;
        }
        if scored.len() < candidates {
            tracing::warn!(
                incomparable = candidates - scored.len(),
                comparable = scored.len(),
                "some embedded memories were produced by a different embedding model \
                 and were excluded from semantic search"
            );
        }

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored.into_iter().map(|(_, f)| f).collect())
    }

    async fn count_for_profile(&self, profile_id: &str) -> Result<u64> {
        // No `OR profile_id IS NULL`: shared rows outlive the member.
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM memory_fragments WHERE profile_id = ?")
                .bind(profile_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(count.max(0) as u64)
    }

    async fn delete(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM memory_fragments WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        // Best effort, not atomic with the delete (separate DB files; WAL makes cross-file
        // transactions non-atomic). `prune_orphans` reconciles; orphans hold no text.
        self.mirror(id, None).await;
        Ok(())
    }

    async fn search_unembedded(&self, limit: usize) -> Result<Vec<MemoryFragment>> {
        // Oldest first, so an interrupted backfill resumes where it stopped.
        let sql = format!(
            "SELECT {SELECT_ALL} FROM memory_fragments \
             WHERE embedding IS NULL AND (lifecycle IS NULL OR lifecycle = 'active') \
             ORDER BY created_at ASC LIMIT ?"
        );
        let rows: Vec<FragmentRow> = sqlx::query_as(&sql)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(row_to_fragment).collect())
    }

    async fn search_stale_dimension(
        &self,
        expected_dims: usize,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        // `length()` of a BLOB counts bytes (of TEXT, chars), so this filters width exactly.
        let expected_bytes = (expected_dims * std::mem::size_of::<f32>()) as i64;
        // Oldest first, as in `search_unembedded`.
        let sql = format!(
            "SELECT {SELECT_ALL} FROM memory_fragments \
             WHERE embedding IS NOT NULL AND length(embedding) != ? \
             AND (lifecycle IS NULL OR lifecycle = 'active') \
             ORDER BY created_at ASC LIMIT ?"
        );
        let rows: Vec<FragmentRow> = sqlx::query_as(&sql)
            .bind(expected_bytes)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(row_to_fragment).collect())
    }

    async fn update_embedding(&self, id: &str, embedding: &[f32]) -> Result<()> {
        sqlx::query("UPDATE memory_fragments SET embedding = ? WHERE id = ?")
            .bind(vec_to_blob(embedding))
            .bind(id)
            .execute(&self.pool)
            .await?;
        // Backfill and dimension repair both land here, so this also populates the index.
        self.mirror(id, Some(embedding)).await;
        Ok(())
    }

    async fn search_by_content(
        &self,
        keywords: &[String],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        if keywords.is_empty() || scope.excludes_everything() {
            return Ok(vec![]);
        }

        let like_clauses: Vec<String> = keywords
            .iter()
            .map(|_| "LOWER(content) LIKE ?".to_string())
            .collect();
        let likes_sql = like_clauses.join(" OR ");

        let (profile_filter, profile_bind) = scope_sql(scope);

        let sql = format!(
            "SELECT {SELECT_ALL} FROM memory_fragments \
             WHERE (lifecycle IS NULL OR lifecycle = 'active') \
             AND ({likes_sql}) \
             {profile_filter} \
             ORDER BY COALESCE(importance, 0.5) DESC, created_at DESC \
             LIMIT ?"
        );

        let mut query = sqlx::query_as::<_, FragmentRow>(&sql);

        for kw in keywords {
            query = query.bind(format!("%{}%", kw.to_lowercase()));
        }

        if let Some(pid) = profile_bind {
            query = query.bind(pid);
        }

        query = query.bind(limit as i64);

        let rows: Vec<FragmentRow> = query.fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(row_to_fragment).collect())
    }

    // ── Segment-aware methods ────────────────────────────────────────────────

    async fn record_access(&self, id: &str) -> Result<()> {
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        sqlx::query(
            "UPDATE memory_fragments \
             SET access_count = access_count + 1, last_accessed_at = ? \
             WHERE id = ?",
        )
        .bind(&now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_content(&self, id: &str, content: &str) -> Result<()> {
        // Drop the stale vector; `search_unembedded` re-embeds the row next sweep.
        sqlx::query("UPDATE memory_fragments SET content = ?, embedding = NULL WHERE id = ?")
            .bind(content)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn update_lifecycle(&self, id: &str, lifecycle: MemoryLifecycle) -> Result<()> {
        sqlx::query("UPDATE memory_fragments SET lifecycle = ? WHERE id = ?")
            .bind(lifecycle_to_str(&lifecycle))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn search_by_segment(
        &self,
        segment: MemorySegment,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<MemoryFragment>> {
        if scope.excludes_everything() {
            return Ok(vec![]);
        }
        let (filter, bind) = scope_sql(scope);
        let query = format!(
            "SELECT {SELECT_ALL} FROM memory_fragments \
             WHERE segment = ? AND (lifecycle IS NULL OR lifecycle = 'active') {filter} \
             ORDER BY created_at DESC LIMIT ?"
        );

        let seg = segment_to_str(&segment);
        let mut q = sqlx::query_as::<_, FragmentRow>(&query).bind(seg);
        if let Some(pid) = bind {
            q = q.bind(pid);
        }
        let rows: Vec<FragmentRow> = q.bind(limit as i64).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(row_to_fragment).collect())
    }

    async fn search_scoreable(&self, scope: &ProfileScope) -> Result<Vec<MemoryFragment>> {
        if scope.excludes_everything() {
            return Ok(vec![]);
        }
        let (filter, bind) = scope_sql(scope);
        let query = format!(
            "SELECT {SELECT_ALL} FROM memory_fragments \
             WHERE (lifecycle IS NULL OR lifecycle = 'active') \
             AND importance IS NOT NULL {filter} \
             ORDER BY created_at DESC LIMIT 1000"
        );

        let rows: Vec<FragmentRow> = match bind {
            Some(pid) => {
                sqlx::query_as(&query)
                    .bind(pid)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => sqlx::query_as(&query).fetch_all(&self.pool).await?,
        };
        Ok(rows.into_iter().map(row_to_fragment).collect())
    }

    async fn batch_update_lifecycle(&self, updates: &[(String, MemoryLifecycle)]) -> Result<()> {
        for (id, lifecycle) in updates {
            self.update_lifecycle(id, lifecycle.clone()).await?;
        }
        Ok(())
    }

    async fn mark_superseded(&self, id: &str, superseded_by: &str) -> Result<()> {
        sqlx::query(
            "UPDATE memory_fragments SET lifecycle = 'merged', superseded_by = ? WHERE id = ?",
        )
        .bind(superseded_by)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ── Audit log ───────────────────────────────────────────────────────────

    async fn log_event(
        &self,
        kind: MemoryEventKind,
        memory_id: &str,
        session_id: Option<&str>,
        data: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO memory_events (event_kind, memory_id, session_id, data) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(kind.to_string())
        .bind(memory_id)
        .bind(session_id)
        .bind(data)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_events(&self, memory_id: Option<&str>, limit: usize) -> Result<Vec<MemoryEvent>> {
        let rows: Vec<EventRow> = match memory_id {
            Some(mid) => {
                sqlx::query_as(
                    "SELECT id, event_kind, memory_id, session_id, data, created_at \
                     FROM memory_events WHERE memory_id = ? \
                     ORDER BY created_at DESC, id DESC LIMIT ?",
                )
                .bind(mid)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as(
                    "SELECT id, event_kind, memory_id, session_id, data, created_at \
                     FROM memory_events ORDER BY created_at DESC, id DESC LIMIT ?",
                )
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().map(row_to_event).collect())
    }

    async fn update_segment(
        &self,
        id: &str,
        segment: pond_core::user_data::domain::memory::MemorySegment,
        importance: f32,
    ) -> anyhow::Result<()> {
        let seg_str = format!("{:?}", segment).to_lowercase();
        sqlx::query("UPDATE memory_fragments SET segment = ?, importance = ? WHERE id = ?")
            .bind(&seg_str)
            .bind(importance as f64)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn log_consolidation_run(
        &self,
        mode: &str,
        memory_count: usize,
        accepted: usize,
        rejected: usize,
        duration_ms: u64,
        details: Option<&str>,
    ) -> anyhow::Result<i64> {
        let row = sqlx::query_scalar::<_, i64>(
            "INSERT INTO consolidation_runs (mode, memory_count, accepted, rejected, duration_ms, details, completed_at)
             VALUES (?, ?, ?, ?, ?, ?, datetime('now'))
             RETURNING id",
        )
        .bind(mode)
        .bind(memory_count as i64)
        .bind(accepted as i64)
        .bind(rejected as i64)
        .bind(duration_ms as i64)
        .bind(details)
        .fetch_one(&self.pool)
        .await?;
        Ok(row)
    }
}

// ── Event row helper ─────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct EventRow {
    id: i64,
    event_kind: String,
    memory_id: String,
    session_id: Option<String>,
    data: Option<String>,
    created_at: String,
}

fn parse_event_kind(s: &str) -> MemoryEventKind {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .unwrap_or(MemoryEventKind::Written)
}

fn row_to_event(row: EventRow) -> MemoryEvent {
    MemoryEvent {
        id: row.id,
        event_kind: parse_event_kind(&row.event_kind),
        memory_id: row.memory_id,
        session_id: row.session_id,
        data: row.data,
        created_at: row.created_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::tempdir;

    async fn make_repo() -> (SqliteMemoryRepository, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        (SqliteMemoryRepository::new(db.system), tmp)
    }

    #[tokio::test]
    async fn add_and_search_recent() {
        let (repo, _tmp) = make_repo().await;
        let frag =
            MemoryFragment::from_chat("f1".to_string(), None, None, "Hello from chat".to_string());
        repo.add(frag).await.unwrap();
        let results = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "Hello from chat");
    }

    // ── ProfileScope semantics against real SQL ──────────────────────────

    async fn repo_with_two_owners_and_a_shared_row() -> (SqliteMemoryRepository, tempfile::TempDir)
    {
        let (repo, tmp) = make_repo().await;
        // `profile_id` references `profiles(id)`, so the owners must exist first.
        for id in ["alice", "bob"] {
            sqlx::query("INSERT INTO profiles (id, display_name, avatar_emoji) VALUES (?, ?, ?)")
                .bind(id)
                .bind(id)
                .bind("duck")
                .execute(&repo.pool)
                .await
                .unwrap();
        }
        repo.add(MemoryFragment::from_chat(
            "a".into(),
            Some("alice".into()),
            None,
            "alice likes tea".into(),
        ))
        .await
        .unwrap();
        repo.add(MemoryFragment::from_chat(
            "b".into(),
            Some("bob".into()),
            None,
            "bob likes coffee".into(),
        ))
        .await
        .unwrap();
        repo.add(MemoryFragment::from_chat(
            "s".into(),
            None,
            None,
            "the bins go out on tuesday".into(),
        ))
        .await
        .unwrap();
        (repo, tmp)
    }

    #[tokio::test]
    async fn owner_sees_their_own_rows_and_shared_ones_but_never_another_persons() {
        let (repo, _tmp) = repo_with_two_owners_and_a_shared_row().await;
        let rows = repo
            .search_recent(&ProfileScope::Owner("alice".into()), 10)
            .await
            .unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"a"), "alice must see her own row");
        assert!(
            ids.contains(&"s"),
            "alice must see unattributed household context"
        );
        assert!(!ids.contains(&"b"), "alice must NOT see bob's row");
    }

    #[tokio::test]
    async fn household_sees_everything() {
        let (repo, _tmp) = repo_with_two_owners_and_a_shared_row().await;
        let rows = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
    }

    #[tokio::test]
    async fn guest_sees_nothing_through_any_read() {
        let (repo, _tmp) = repo_with_two_owners_and_a_shared_row().await;
        let g = ProfileScope::Guest;
        assert!(repo.search_recent(&g, 10).await.unwrap().is_empty());
        assert!(repo
            .search_similar(&[0.0f32; 4], &g, 10)
            .await
            .unwrap()
            .is_empty());
        assert!(repo
            .search_by_content(&["tea".to_string()], &g, 10)
            .await
            .unwrap()
            .is_empty());
        assert!(repo
            .search_by_segment(MemorySegment::Identity, &g, 10)
            .await
            .unwrap()
            .is_empty());
        assert!(repo.search_scoreable(&g).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn keyword_search_is_scoped_like_the_others() {
        let (repo, _tmp) = repo_with_two_owners_and_a_shared_row().await;
        let hits = repo
            .search_by_content(
                &["coffee".to_string()],
                &ProfileScope::Owner("alice".into()),
                10,
            )
            .await
            .unwrap();
        assert!(
            hits.is_empty(),
            "alice searching for 'coffee' must not surface bob's memory"
        );
    }

    #[tokio::test]
    async fn delete_removes_fragment() {
        let (repo, _tmp) = make_repo().await;
        let frag = MemoryFragment::from_chat("del1".to_string(), None, None, "bye".to_string());
        repo.add(frag).await.unwrap();
        repo.delete("del1").await.unwrap();
        assert!(repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn search_similar_falls_back_to_recent_when_no_embeddings() {
        let (repo, _tmp) = make_repo().await;
        let frag =
            MemoryFragment::from_chat("f2".to_string(), None, None, "no embedding".to_string());
        repo.add(frag).await.unwrap();
        let query = vec![0.0f32; 4];
        let results = repo
            .search_similar(&query, &ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn search_similar_falls_back_when_every_vector_is_from_another_model() {
        let (repo, _tmp) = make_repo().await;
        let mut stale =
            MemoryFragment::from_chat("old".to_string(), None, None, "stale vector".to_string());
        // 384-dim, as fastembed would have written.
        stale.embedding = Some(vec![0.5f32; 384]);
        repo.add(stale).await.unwrap();
        // Only `search_recent` can return this unembedded row, which makes the test discriminate.
        let plain = MemoryFragment::from_chat(
            "plain".to_string(),
            None,
            None,
            "never embedded".to_string(),
        );
        repo.add(plain).await.unwrap();

        // A 768-dim query, as the GGUF provider produces.
        let query = vec![0.1f32; 768];
        let results = repo
            .search_similar(&query, &ProfileScope::Household, 10)
            .await
            .unwrap();

        assert_eq!(
            results.len(),
            2,
            "expected the recency fallback to fire and return both rows; got {:?}",
            results.iter().map(|f| &f.content).collect::<Vec<_>>()
        );
        assert!(
            results.iter().any(|f| f.content == "never embedded"),
            "the unembedded row proves search_recent ran; without it the incomparable \
             row was merely scored 0.0 and returned as a semantic hit"
        );
    }

    #[tokio::test]
    async fn search_stale_dimension_finds_wrong_width_rows_and_only_those() {
        let (repo, _tmp) = make_repo().await;

        let mut stale = MemoryFragment::from_chat("stale".to_string(), None, None, "old".into());
        stale.embedding = Some(vec![0.5f32; 384]);
        repo.add(stale).await.unwrap();

        let mut current =
            MemoryFragment::from_chat("current".to_string(), None, None, "new".into());
        current.embedding = Some(vec![0.5f32; 768]);
        repo.add(current).await.unwrap();

        let never = MemoryFragment::from_chat("never".to_string(), None, None, "none".into());
        repo.add(never).await.unwrap();

        let stale_rows = repo.search_stale_dimension(768, 10).await.unwrap();
        let ids: Vec<&str> = stale_rows.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["stale"],
            "expected only the 384-dim row; a NULL-embedding row belongs to the \
             backfill and a 768-dim row is already correct"
        );

        // The backfill sees only the never-embedded row, hence the separate selector.
        let unembedded = repo.search_unembedded(10).await.unwrap();
        let ids: Vec<&str> = unembedded.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["never"]);
    }

    #[tokio::test]
    async fn incomparable_vectors_do_not_crowd_out_the_comparable_one() {
        let (repo, _tmp) = make_repo().await;
        for i in 0..5 {
            let mut stale =
                MemoryFragment::from_chat(format!("old{i}"), None, None, format!("stale {i}"));
            stale.embedding = Some(vec![0.9f32; 384]);
            repo.add(stale).await.unwrap();
        }
        let mut fresh = MemoryFragment::from_chat(
            "new".to_string(),
            None,
            None,
            "the only comparable one".to_string(),
        );
        let mut v = vec![0.0f32; 768];
        v[0] = 1.0;
        fresh.embedding = Some(v);
        repo.add(fresh).await.unwrap();

        let mut query = vec![0.0f32; 768];
        query[0] = 1.0;
        // Limit 10, not 1: at 1 the comparable row wins on score even without the filter.
        let results = repo
            .search_similar(&query, &ProfileScope::Household, 10)
            .await
            .unwrap();

        assert_eq!(
            results.len(),
            1,
            "only the comparable row may be returned; the 384-dim rows are not \
             judgeable and must be excluded rather than scored 0.0. got {:?}",
            results.iter().map(|f| &f.content).collect::<Vec<_>>()
        );
        assert_eq!(results[0].content, "the only comparable one");
    }

    #[tokio::test]
    async fn search_similar_ranks_by_cosine() {
        let (repo, _tmp) = make_repo().await;

        let mut high =
            MemoryFragment::from_chat("high".to_string(), None, None, "high sim".to_string());
        high.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);

        let mut low =
            MemoryFragment::from_chat("low".to_string(), None, None, "low sim".to_string());
        low.embedding = Some(vec![0.0, 1.0, 0.0, 0.0]);

        repo.add(high).await.unwrap();
        repo.add(low).await.unwrap();

        let query = vec![1.0f32, 0.0, 0.0, 0.0];
        let results = repo
            .search_similar(&query, &ProfileScope::Household, 2)
            .await
            .unwrap();
        assert_eq!(results[0].id, "high");
        assert_eq!(results[1].id, "low");
    }

    #[tokio::test]
    async fn add_with_segment_fields() {
        let (repo, _tmp) = make_repo().await;
        let frag = MemoryFragment::from_extraction(
            "ext1".to_string(),
            None,
            "User prefers dark mode".to_string(),
            MemorySegment::Preference,
            0.75,
            None,
        );
        repo.add(frag).await.unwrap();
        let results = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].segment, Some(MemorySegment::Preference));
        assert!((results[0].importance.unwrap() - 0.75).abs() < 0.01);
        assert_eq!(results[0].tier, Some(MemoryTier::Long));
        assert_eq!(results[0].lifecycle, Some(MemoryLifecycle::Active));
    }

    #[tokio::test]
    async fn record_access_increments_count() {
        let (repo, _tmp) = make_repo().await;
        let frag = MemoryFragment::from_extraction(
            "acc1".to_string(),
            None,
            "Test access".to_string(),
            MemorySegment::Knowledge,
            0.5,
            None,
        );
        repo.add(frag).await.unwrap();
        repo.record_access("acc1").await.unwrap();
        repo.record_access("acc1").await.unwrap();
        let results = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(results[0].access_count, 2);
        assert!(results[0].last_accessed_at.is_some());
    }

    #[tokio::test]
    async fn update_lifecycle_hides_from_search() {
        let (repo, _tmp) = make_repo().await;
        let frag = MemoryFragment::from_extraction(
            "arch1".to_string(),
            None,
            "To archive".to_string(),
            MemorySegment::Context,
            0.2,
            None,
        );
        repo.add(frag).await.unwrap();
        repo.update_lifecycle("arch1", MemoryLifecycle::Archived)
            .await
            .unwrap();
        let results = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn search_by_content_finds_matching_keywords() {
        let (repo, _tmp) = make_repo().await;
        repo.add(MemoryFragment::from_extraction(
            "cat1".to_string(),
            None,
            "User loves cats".to_string(),
            MemorySegment::Preference,
            0.8,
            None,
        ))
        .await
        .unwrap();
        repo.add(MemoryFragment::from_extraction(
            "dog1".to_string(),
            None,
            "User has a dog named Rex".to_string(),
            MemorySegment::Knowledge,
            0.6,
            None,
        ))
        .await
        .unwrap();
        repo.add(MemoryFragment::from_extraction(
            "work1".to_string(),
            None,
            "User works at Jarida".to_string(),
            MemorySegment::Identity,
            0.85,
            None,
        ))
        .await
        .unwrap();

        let results = repo
            .search_by_content(&["cats".to_string()], &ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "cat1");

        let results = repo
            .search_by_content(&["dog".to_string()], &ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "dog1");

        let results = repo
            .search_by_content(
                &["cats".to_string(), "dog".to_string()],
                &ProfileScope::Household,
                10,
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 2);

        let results = repo
            .search_by_content(&[], &ProfileScope::Household, 10)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn search_unembedded_returns_only_rows_without_a_vector() {
        let (repo, _tmp) = make_repo().await;

        let plain =
            MemoryFragment::from_chat("plain".to_string(), None, None, "no vec".to_string());
        let mut embedded =
            MemoryFragment::from_chat("embedded".to_string(), None, None, "has vec".to_string());
        embedded.embedding = Some(vec![0.1, 0.2, 0.3, 0.4]);
        let mut archived = MemoryFragment::from_extraction(
            "archived".to_string(),
            None,
            "archived, no vec".to_string(),
            MemorySegment::Context,
            0.2,
            None,
        );
        archived.lifecycle = Some(MemoryLifecycle::Archived);

        repo.add(plain).await.unwrap();
        repo.add(embedded).await.unwrap();
        repo.add(archived).await.unwrap();

        let pending = repo.search_unembedded(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "plain");
    }

    #[tokio::test]
    async fn update_embedding_makes_a_row_visible_to_similarity_search() {
        let (repo, _tmp) = make_repo().await;
        repo.add(MemoryFragment::from_chat(
            "backfilled".to_string(),
            None,
            None,
            "was unembedded".to_string(),
        ))
        .await
        .unwrap();

        repo.update_embedding("backfilled", &[1.0, 0.0, 0.0, 0.0])
            .await
            .unwrap();

        assert!(repo.search_unembedded(10).await.unwrap().is_empty());
        let stored = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(stored[0].embedding, Some(vec![1.0, 0.0, 0.0, 0.0]));

        // Now it participates in cosine ranking rather than being ignored.
        let hits = repo
            .search_similar(&[1.0, 0.0, 0.0, 0.0], &ProfileScope::Household, 5)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "backfilled");
    }

    #[tokio::test]
    async fn search_unembedded_respects_the_batch_limit_oldest_first() {
        let (repo, _tmp) = make_repo().await;
        for i in 0..5 {
            let mut frag =
                MemoryFragment::from_chat(format!("m{i}"), None, None, format!("fragment {i}"));
            frag.created_at = Utc::now() - chrono::Duration::days(10 - i as i64);
            repo.add(frag).await.unwrap();
        }
        let batch = repo.search_unembedded(2).await.unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].id, "m0");
        assert_eq!(batch[1].id, "m1");
    }

    #[tokio::test]
    async fn search_by_segment_filters() {
        let (repo, _tmp) = make_repo().await;
        repo.add(MemoryFragment::from_extraction(
            "id1".to_string(),
            None,
            "Name is Jerry".to_string(),
            MemorySegment::Identity,
            0.85,
            None,
        ))
        .await
        .unwrap();
        repo.add(MemoryFragment::from_extraction(
            "pref1".to_string(),
            None,
            "Likes dark mode".to_string(),
            MemorySegment::Preference,
            0.7,
            None,
        ))
        .await
        .unwrap();

        let identities = repo
            .search_by_segment(MemorySegment::Identity, &ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].id, "id1");
    }

    #[tokio::test]
    async fn log_and_get_events() {
        let (repo, _tmp) = make_repo().await;

        repo.log_event(MemoryEventKind::Extracted, "mem-1", Some("sess-1"), None)
            .await
            .unwrap();
        repo.log_event(MemoryEventKind::Written, "mem-1", None, Some("via MCP"))
            .await
            .unwrap();
        repo.log_event(MemoryEventKind::Recalled, "mem-2", None, None)
            .await
            .unwrap();

        let all = repo.get_events(None, 100).await.unwrap();
        assert_eq!(all.len(), 3);

        let mem1_events = repo.get_events(Some("mem-1"), 100).await.unwrap();
        assert_eq!(mem1_events.len(), 2);
        let kinds: Vec<_> = mem1_events.iter().map(|e| &e.event_kind).collect();
        assert!(kinds.contains(&&MemoryEventKind::Extracted));
        assert!(kinds.contains(&&MemoryEventKind::Written));
        let extracted = mem1_events
            .iter()
            .find(|e| e.event_kind == MemoryEventKind::Extracted)
            .unwrap();
        assert_eq!(extracted.session_id.as_deref(), Some("sess-1"));
    }

    // ── Legacy-row semantics ─────────────────────────────────────────────

    #[tokio::test]
    async fn a_legacy_unattributed_row_is_shared_not_owned() {
        let (repo, _tmp) = repo_with_two_owners_and_a_shared_row().await;

        // Both members see the shared row...
        for who in ["alice", "bob"] {
            let seen = repo
                .search_recent(&ProfileScope::Owner(who.to_string()), 50)
                .await
                .unwrap();
            assert!(
                seen.iter().any(|f| f.profile_id.is_none()),
                "{who} should see the unattributed household row"
            );
        }

        // ...and neither owns it, so member deletion's count and CASCADE leave it alone.
        for who in ["alice", "bob"] {
            let owned = repo.count_for_profile(who).await.unwrap();
            let all = repo
                .search_recent(&ProfileScope::Household, 50)
                .await
                .unwrap();
            // Exact, not `<`: a count wrongly including the shared row (2 of 3) would pass `<`.
            assert_eq!(
                owned, 1,
                "{who} owns exactly their own row -- not the shared one, which survives them"
            );
            assert_eq!(all.len(), 3, "fixture: two owned rows and one shared");
        }
    }

    #[tokio::test]
    async fn the_per_member_count_excludes_shared_rows() {
        let (repo, _tmp) = repo_with_two_owners_and_a_shared_row().await;

        let a = repo.count_for_profile("alice").await.unwrap();
        let b = repo.count_for_profile("bob").await.unwrap();
        let everything = repo
            .search_recent(&ProfileScope::Household, 100)
            .await
            .unwrap()
            .len() as u64;

        assert!(a >= 1 && b >= 1, "each member should own at least one row");
        assert!(
            a + b < everything,
            "owned counts ({a} + {b}) must not account for every row ({everything}) -- \
             the difference is the shared context that survives a deletion"
        );
    }

    #[tokio::test]
    async fn counting_a_member_who_owns_nothing_is_zero_not_an_error() {
        let (repo, _tmp) = repo_with_two_owners_and_a_shared_row().await;
        assert_eq!(repo.count_for_profile("nobody-at-all").await.unwrap(), 0);
    }
}
