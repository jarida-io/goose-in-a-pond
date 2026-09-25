//! SQLite-backed [`VectorIndex`] over `pond_vectors.db`.
//!
//! Each connection ATTACHes the system DB as `sys`, so reads JOIN live rows. Only this file is
//! written (cross-database transactions aren't atomic under WAL), hence orphans are expected.
//! Similarity is brute-force cosine on purpose: sub-millisecond at household scale.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use pond_core::context::vector_index::{
    Corpus, CorpusHealth, IndexHealth, ResolvedHit, VectorEntry, VectorHit, VectorIndex,
};
use pond_core::user_data::domain::profile::ProfileScope;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, Pool, Sqlite};
use std::path::Path;
use std::str::FromStr;

pub struct SqliteVectorIndex {
    pool: Pool<Sqlite>,
}

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// `None` for mismatched widths: `0.0` is a real score (orthogonal), not "incomparable".
fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some(dot / (na * nb))
}

/// A corpus's `sys.<table>` and id column, in one exhaustive match so none can be missed.
fn source_table(corpus: Corpus) -> (&'static str, &'static str) {
    match corpus {
        Corpus::Memory => ("sys.memory_fragments", "id"),
        Corpus::Context => ("sys.context_items", "id"),
        Corpus::Summary => ("sys.sessions", "id"),
    }
}

/// Live-row SQL filter, shared by every query so none can skip it. For summaries a NULL
/// `profile_id` means "unknown owner" (unlike memory's "shared"), so those are never live.
fn liveness_sql(corpus: Corpus) -> &'static str {
    match corpus {
        Corpus::Memory => "AND (s.lifecycle IS NULL OR s.lifecycle = 'active')",
        Corpus::Context => "",
        Corpus::Summary => {
            "AND s.rolling_summary IS NOT NULL AND s.rolling_summary != '' \
             AND s.profile_id IS NOT NULL"
        }
    }
}

/// Scope as SQL in the `WHERE`: post-filtering would let hidden rows take top-K slots.
fn scope_sql(corpus: Corpus, scope: &ProfileScope) -> (String, Option<String>) {
    match (corpus, scope) {
        (_, ProfileScope::Guest) => ("AND 1 = 0".into(), None),
        (_, ProfileScope::Household) => (String::new(), None),
        (Corpus::Memory, ProfileScope::Owner(id)) => (
            // Like `sqlite_memory::scope_sql`: unattributed memories are shared household context.
            "AND (s.profile_id = ? OR s.profile_id IS NULL)".into(),
            Some(id.clone()),
        ),
        (Corpus::Context, ProfileScope::Owner(id)) => {
            // `profile_id` is NOT NULL here; an `IS NULL` limb would only add a hiding place.
            ("AND s.profile_id = ?".into(), Some(id.clone()))
        }
        // No `IS NULL` limb, unlike Memory: an unattributed session is a guest's, not shared.
        (Corpus::Summary, ProfileScope::Owner(id)) => {
            ("AND s.profile_id = ?".into(), Some(id.clone()))
        }
    }
}

/// Live text via the JOIN: the index stores no text, so an orphan can't leak deleted words.
/// Context must match `ContextItem::embedding_text`.
fn text_sql(corpus: Corpus) -> &'static str {
    match corpus {
        Corpus::Memory => "s.content",
        Corpus::Context => {
            "CASE WHEN s.title = '' THEN s.body \
                            WHEN s.body = '' THEN s.title \
                            ELSE s.title || char(10) || s.body END"
        }
        Corpus::Summary => "s.rolling_summary",
    }
}

/// The freshness column a corpus compares against, if it has one.
fn source_rev_sql(corpus: Corpus) -> Option<&'static str> {
    match corpus {
        // Rewritten in place, so a present vector can describe an older summary.
        Corpus::Summary => Some("s.rolling_summary_updated_at"),
        // Stable once extracted: consolidation adds a new row rather than editing.
        Corpus::Memory => None,
        // A context item is re-synced by external_id, which rewrites the row.
        Corpus::Context => Some("s.ingested_at"),
    }
}

impl SqliteVectorIndex {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }

    /// Opens `pond_vectors.db`; `after_connect` ATTACHes `sys` because ATTACH is per-connection.
    pub async fn connect(vectors_path: &Path, system_path: &Path) -> Result<Pool<Sqlite>> {
        let opts =
            SqliteConnectOptions::from_str(&format!("sqlite:{}?mode=rwc", vectors_path.display()))?
                .create_if_missing(true)
                .pragma("journal_mode", "WAL")
                .pragma("synchronous", "NORMAL")
                .pragma("cache_size", "2000")
                .pragma("mmap_size", "33554432");

        let system = system_path.display().to_string();
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .after_connect(move |conn, _meta| {
                let system = system.clone();
                Box::pin(async move {
                    conn.execute(format!("ATTACH DATABASE '{system}' AS sys").as_str())
                        .await?;
                    Ok(())
                })
            })
            .connect_with(opts)
            .await
            .with_context(|| format!("opening {}", vectors_path.display()))?;
        Ok(pool)
    }
}

#[async_trait]
impl VectorIndex for SqliteVectorIndex {
    async fn upsert(&self, entry: &VectorEntry) -> Result<()> {
        sqlx::query(
            "INSERT INTO vectors \
               (corpus, row_id, chunk_ix, chunk_start, chunk_len, \
                model_id, dims, vector, source_rev, embedded_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(corpus, row_id, chunk_ix) DO UPDATE SET \
               chunk_start = excluded.chunk_start, chunk_len = excluded.chunk_len, \
               model_id = excluded.model_id, dims = excluded.dims, vector = excluded.vector, \
               source_rev = excluded.source_rev, embedded_at = excluded.embedded_at",
        )
        .bind(entry.corpus.as_str())
        .bind(&entry.row_id)
        .bind(entry.chunk_ix)
        .bind(entry.chunk_span.map(|(start, _)| start))
        .bind(entry.chunk_span.map(|(_, len)| len))
        .bind(&entry.model_id)
        .bind(entry.vector.len() as i64)
        .bind(vec_to_blob(&entry.vector))
        .bind(&entry.source_rev)
        .bind(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn remove(&self, corpus: Corpus, row_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM vectors WHERE corpus = ? AND row_id = ?")
            .bind(corpus.as_str())
            .bind(row_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn get(&self, corpus: Corpus, row_id: &str) -> Result<Option<VectorEntry>> {
        let row: Option<(String, Vec<u8>, Option<String>, Option<i64>, Option<i64>)> =
            sqlx::query_as(
                // Chunk 0: `get` only answers "is this row indexed?".
                "SELECT model_id, vector, source_rev, chunk_start, chunk_len FROM vectors \
             WHERE corpus = ? AND row_id = ? AND chunk_ix = 0",
            )
            .bind(corpus.as_str())
            .bind(row_id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(
            row.map(|(model_id, blob, source_rev, start, len)| VectorEntry {
                corpus,
                row_id: row_id.to_string(),
                chunk_ix: 0,
                chunk_span: match (start, len) {
                    (Some(s), Some(l)) => Some((s, l)),
                    _ => None,
                },
                model_id,
                vector: blob_to_vec(&blob),
                source_rev,
            }),
        )
    }

    async fn search(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        if query.is_empty() || limit == 0 || scope.excludes_everything() {
            return Ok(vec![]);
        }

        let mut scored: Vec<VectorHit> = Vec::new();
        for corpus in Corpus::ALL {
            let (table, id_col) = source_table(corpus);
            let (scope_pred, bind) = scope_sql(corpus, scope);
            let live = liveness_sql(corpus);
            // The JOIN doubles as the existence check: orphans never match.
            let sql = format!(
                "SELECT v.row_id, v.vector FROM vectors v \
                 JOIN {table} s ON s.{id_col} = v.row_id \
                 WHERE v.corpus = ? AND v.model_id = ? {scope_pred} {live}"
            );
            let mut q = sqlx::query_as::<_, (String, Vec<u8>)>(&sql)
                .bind(corpus.as_str())
                .bind(model_id);
            if let Some(b) = bind {
                q = q.bind(b);
            }
            let rows = q.fetch_all(&self.pool).await?;
            for (row_id, blob) in rows {
                // Wrong width is not a bad match but no match: skip it.
                if let Some(score) = cosine(query, &blob_to_vec(&blob)) {
                    scored.push(VectorHit {
                        corpus,
                        row_id,
                        score,
                    });
                }
            }
        }

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                // Stable tie-break so equal scores never reorder between runs.
                .then_with(|| a.row_id.cmp(&b.row_id))
        });
        scored.truncate(limit);
        Ok(scored)
    }

    async fn search_resolved(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<ResolvedHit>> {
        if query.is_empty() || limit == 0 || scope.excludes_everything() {
            return Ok(vec![]);
        }
        let mut scored: Vec<ResolvedHit> = Vec::new();
        for corpus in Corpus::ALL {
            let (table, id_col) = source_table(corpus);
            let (scope_pred, bind) = scope_sql(corpus, scope);
            let live = liveness_sql(corpus);
            let text = text_sql(corpus);
            // Spans are byte offsets, but `substr` on TEXT counts characters (and is 1-indexed),
            // so slice the BLOB at +1 and cast back. A NULL span is a whole-text vector.
            let sql = format!(
                "SELECT v.row_id, v.vector, \
                        CASE WHEN v.chunk_start IS NULL OR v.chunk_len IS NULL THEN {text} \
                             ELSE CAST(substr(CAST({text} AS BLOB), \
                                              v.chunk_start + 1, v.chunk_len) AS TEXT) END \
                 FROM vectors v \
                 JOIN {table} s ON s.{id_col} = v.row_id \
                 WHERE v.corpus = ? AND v.model_id = ? {scope_pred} {live}"
            );
            let mut q = sqlx::query_as::<_, (String, Vec<u8>, String)>(&sql)
                .bind(corpus.as_str())
                .bind(model_id);
            if let Some(b) = bind {
                q = q.bind(b);
            }
            // Keep only each row's best chunk, so one long document can't flood the top-K.
            let mut best: std::collections::HashMap<String, ResolvedHit> =
                std::collections::HashMap::new();
            for (row_id, blob, text) in q.fetch_all(&self.pool).await? {
                let Some(score) = cosine(query, &blob_to_vec(&blob)) else {
                    continue;
                };
                match best.get(&row_id) {
                    Some(existing) if existing.score >= score => {}
                    _ => {
                        best.insert(
                            row_id.clone(),
                            ResolvedHit {
                                corpus,
                                row_id,
                                score,
                                text,
                            },
                        );
                    }
                }
            }
            scored.extend(best.into_values());
        }
        // Score, then corpus order (the "memory wins ties" policy), then id for a stable order.
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.corpus.cmp(&b.corpus))
                .then_with(|| a.row_id.cmp(&b.row_id))
        });
        scored.truncate(limit);
        Ok(scored)
    }

    async fn needs_embedding(
        &self,
        corpus: Corpus,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<String>> {
        let (table, id_col) = source_table(corpus);
        let rev = source_rev_sql(corpus);

        // LEFT JOIN from the source finds never-embedded and stale rows in one query. The
        // `source_rev IS NOT NULL` guard stops an unstamped vector being re-embedded every sweep.
        let staleness = match rev {
            Some(col) => format!("OR (v.source_rev IS NOT NULL AND v.source_rev IS NOT {col})"),
            None => String::new(),
        };
        let extra = liveness_sql(corpus);
        let sql = format!(
            "SELECT s.{id_col} FROM {table} s \
             LEFT JOIN vectors v ON v.row_id = s.{id_col} AND v.corpus = ? \
             WHERE (v.row_id IS NULL OR v.model_id != ? {staleness}) {extra} \
             LIMIT ?"
        );
        let rows: Vec<(String,)> = sqlx::query_as(&sql)
            .bind(corpus.as_str())
            .bind(model_id)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    async fn needs_embedding_with_text(
        &self,
        corpus: Corpus,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let (table, id_col) = source_table(corpus);
        let rev = source_rev_sql(corpus);
        let staleness = match rev {
            Some(col) => format!("OR (v.source_rev IS NOT NULL AND v.source_rev IS NOT {col})"),
            None => String::new(),
        };
        let extra = liveness_sql(corpus);
        let text = text_sql(corpus);
        let sql = format!(
            "SELECT s.{id_col}, {text} FROM {table} s \
             LEFT JOIN vectors v ON v.row_id = s.{id_col} AND v.corpus = ? \
             WHERE (v.row_id IS NULL OR v.model_id != ? {staleness}) {extra} \
             LIMIT ?"
        );
        let rows: Vec<(String, String)> = sqlx::query_as(&sql)
            .bind(corpus.as_str())
            .bind(model_id)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    async fn backfill_from_source(
        &self,
        corpus: Corpus,
        model_id: &str,
        expected_dims: usize,
    ) -> Result<u64> {
        // Pure SQL across the ATTACH: copies vectors the source already holds, no inference.
        let (table, id_col) = source_table(corpus);
        // Sessions hold no vectors; the summary embedding sweep owns that corpus.
        if corpus == Corpus::Summary {
            return Ok(0);
        }
        let extra = liveness_sql(corpus);
        // Width filter: a vector from another model must not be restamped with this one.
        let expected_bytes = (expected_dims * std::mem::size_of::<f32>()) as i64;
        let rev = match source_rev_sql(corpus) {
            Some(col) => col,
            None => "NULL",
        };
        let sql = format!(
            "INSERT INTO vectors (corpus, row_id, model_id, dims, vector, source_rev, embedded_at) \
             SELECT ?, s.{id_col}, ?, ?, s.embedding, {rev}, ? \
             FROM {table} s \
             LEFT JOIN vectors v ON v.row_id = s.{id_col} AND v.corpus = ? \
             WHERE s.embedding IS NOT NULL AND length(s.embedding) = ? \
             AND v.row_id IS NULL {extra}"
        );
        let copied = sqlx::query(&sql)
            .bind(corpus.as_str())
            .bind(model_id)
            .bind(expected_dims as i64)
            .bind(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true))
            .bind(corpus.as_str())
            .bind(expected_bytes)
            .execute(&self.pool)
            .await?
            .rows_affected();
        if copied > 0 {
            tracing::info!(
                corpus = corpus.as_str(),
                copied,
                "adopted existing vectors into the index"
            );
        }
        Ok(copied)
    }

    async fn prune_orphans(&self) -> Result<u64> {
        let mut removed = 0u64;
        for corpus in Corpus::ALL {
            let (table, id_col) = source_table(corpus);
            let sql = format!(
                "DELETE FROM vectors WHERE corpus = ? AND row_id NOT IN \
                 (SELECT s.{id_col} FROM {table} s)"
            );
            let result = sqlx::query(&sql)
                .bind(corpus.as_str())
                .execute(&self.pool)
                .await?;
            removed += result.rows_affected();
        }
        if removed > 0 {
            tracing::info!(removed, "pruned vector index orphans");
        }
        Ok(removed)
    }

    /// Counted per corpus from the source side: coverage of what retrieval can reach, not of the
    /// file, so orphans and archived rows are excluded.
    async fn health(&self, model_id: &str) -> Result<IndexHealth> {
        let mut totals = IndexHealth::default();
        let mut per_corpus = Vec::with_capacity(Corpus::ALL.len());

        for corpus in Corpus::ALL {
            let (table, id_col) = source_table(corpus);
            let live = liveness_sql(corpus);
            // Corpus filter in the JOIN, not the WHERE, or un-indexed rows drop out of the count.
            // `?` order puts `model_id` (in the SELECT) first. DISTINCT: one row owns many vectors.
            let sql = format!(
                "SELECT COUNT(DISTINCT s.{id_col}), \
                 COUNT(DISTINCT CASE WHEN v.model_id = ? THEN s.{id_col} END), \
                 COUNT(DISTINCT CASE WHEN v.row_id IS NULL THEN s.{id_col} END) \
                 FROM {table} s \
                 LEFT JOIN vectors v ON v.row_id = s.{id_col} AND v.corpus = ? \
                 WHERE 1 = 1 {live}"
            );
            let (rows, indexed, missing): (i64, i64, i64) = sqlx::query_as(&sql)
                .bind(model_id)
                .bind(corpus.as_str())
                .fetch_one(&self.pool)
                .await?;

            // Unfiltered table count, so a reader can tell "corpus empty" from "corpus excluded".
            let (source,): (i64,) = sqlx::query_as(&format!("SELECT COUNT(*) FROM {table} s"))
                .fetch_one(&self.pool)
                .await?;

            let rows = rows.max(0) as u64;
            let source_rows = source.max(0) as u64;
            let indexed_rows = indexed.max(0) as u64;
            let missing_rows = missing.max(0) as u64;
            // Subtracted, not counted, so the three buckets always sum to `rows`.
            let mismatched = rows
                .saturating_sub(indexed_rows)
                .saturating_sub(missing_rows);

            totals.matching += indexed_rows;
            totals.mismatched += mismatched;
            totals.missing += missing_rows;
            per_corpus.push(CorpusHealth {
                corpus,
                rows,
                source_rows,
                indexed_rows,
                missing_rows,
                mismatched,
            });
        }

        totals.per_corpus = per_corpus;
        Ok(totals)
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::add_session;
    use super::*;
    use tempfile::TempDir;

    /// A pond with the real system schema plus a vector index attached to it.
    async fn wire() -> (TempDir, SqliteVectorIndex) {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index = SqliteVectorIndex::new(db.vectors.clone());
        (tmp, index)
    }

    /// Fixtures must create the member first: `profile_id` is an enforced foreign key.
    async fn add_profile(pool: &Pool<Sqlite>, id: &str) {
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
            .bind(id)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn add_memory(pool: &Pool<Sqlite>, id: &str, profile: Option<&str>) {
        sqlx::query(
            "INSERT INTO memory_fragments (id, profile_id, content, source, tags, created_at, \
             access_count, lifecycle) VALUES (?, ?, 'x', 'chat', '[]', datetime('now'), 0, 'active')",
        )
        .bind(id)
        .bind(profile)
        .execute(pool)
        .await
        .unwrap();
    }

    fn entry(id: &str, v: Vec<f32>) -> VectorEntry {
        VectorEntry {
            corpus: Corpus::Memory,
            row_id: id.to_string(),
            chunk_ix: 0,
            chunk_span: None,
            model_id: "nomic-embed-text-v1.5".into(),
            vector: v,
            source_rev: None,
        }
    }

    #[tokio::test]
    async fn a_vector_roundtrips() {
        let (_tmp, index) = wire().await;
        let e = entry("m1", vec![0.1, 0.2, 0.3]);
        index.upsert(&e).await.unwrap();

        let got = index.get(Corpus::Memory, "m1").await.unwrap().unwrap();
        assert_eq!(got, e, "what came back is not what went in");
        assert!(index.get(Corpus::Memory, "nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn re_embedding_replaces_rather_than_appends() {
        let (_tmp, index) = wire().await;
        index.upsert(&entry("m1", vec![1.0, 0.0])).await.unwrap();
        index.upsert(&entry("m1", vec![0.0, 1.0])).await.unwrap();

        let got = index.get(Corpus::Memory, "m1").await.unwrap().unwrap();
        assert_eq!(got.vector, vec![0.0, 1.0], "the old vector survived");
    }

    #[tokio::test]
    async fn deleting_the_index_file_rebuilds_it_empty_and_usable() {
        let tmp = TempDir::new().unwrap();
        {
            let db = crate::db::Database::init(tmp.path()).await.unwrap();
            let index = SqliteVectorIndex::new(db.vectors.clone());
            add_memory(&db.system, "m1", None).await;
            index.upsert(&entry("m1", vec![1.0, 0.0])).await.unwrap();
            assert!(index.get(Corpus::Memory, "m1").await.unwrap().is_some());
        }

        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(tmp.path().join(format!("pond_vectors.db{suffix}")));
        }
        assert!(!tmp.path().join("pond_vectors.db").exists());

        // Reopening recreates it, and the source row is still there to re-embed.
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index = SqliteVectorIndex::new(db.vectors.clone());
        assert!(
            index.get(Corpus::Memory, "m1").await.unwrap().is_none(),
            "the index came back populated, so it was not actually rebuilt"
        );
        let todo = index
            .needs_embedding(Corpus::Memory, "nomic-embed-text-v1.5", 10)
            .await
            .unwrap();
        assert_eq!(
            todo,
            vec!["m1".to_string()],
            "a rebuilt index must report the source row as needing embedding"
        );
    }

    #[tokio::test]
    async fn search_joins_against_live_rows_and_an_orphan_matches_nothing() {
        let (_tmp, index) = wire().await;

        // 'ghost' has a vector but no source row: an orphan.
        index.upsert(&entry("ghost", vec![1.0, 0.0])).await.unwrap();
        let hits = index
            .search(
                &[1.0, 0.0],
                "nomic-embed-text-v1.5",
                &ProfileScope::Household,
                10,
            )
            .await
            .unwrap();
        assert!(
            hits.is_empty(),
            "an orphan matched; the JOIN is not filtering by existence"
        );

        // And it is prunable.
        assert_eq!(index.prune_orphans().await.unwrap(), 1);
        assert!(index.get(Corpus::Memory, "ghost").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_vector_from_another_model_is_never_returned() {
        let (tmp, index) = wire().await;
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        add_memory(&db.system, "m1", None).await;

        let mut stale = entry("m1", vec![1.0, 0.0]);
        stale.model_id = "all-MiniLM-L6-v2".into();
        index.upsert(&stale).await.unwrap();

        let hits = index
            .search(
                &[1.0, 0.0],
                "nomic-embed-text-v1.5",
                &ProfileScope::Household,
                10,
            )
            .await
            .unwrap();
        assert!(hits.is_empty(), "a foreign-model vector was scored");

        // And the sweep must offer it for re-embedding rather than leaving it.
        let todo = index
            .needs_embedding(Corpus::Memory, "nomic-embed-text-v1.5", 10)
            .await
            .unwrap();
        assert_eq!(todo, vec!["m1".to_string()]);

        let health = index.health("nomic-embed-text-v1.5").await.unwrap();
        assert_eq!(health.matching, 0);
        assert_eq!(health.mismatched, 1);
    }

    /// The per-corpus row for `corpus`, failing loudly if it is absent.
    fn corpus_health(health: &IndexHealth, corpus: Corpus) -> &CorpusHealth {
        health
            .per_corpus
            .iter()
            .find(|c| c.corpus == corpus)
            .unwrap_or_else(|| panic!("{corpus:?} is missing from the health surface entirely"))
    }

    #[tokio::test]
    async fn a_corpus_with_qualifying_rows_and_no_vectors_reports_zero_coverage() {
        let (tmp, index) = wire().await;
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        add_memory(&db.system, "m1", None).await;
        add_memory(&db.system, "m2", None).await;

        let health = index.health("nomic-embed-text-v1.5").await.unwrap();
        let memory = corpus_health(&health, Corpus::Memory);
        assert_eq!(
            memory.rows, 2,
            "the qualifying source rows were not counted, so coverage has no denominator"
        );
        assert_eq!(
            memory.indexed_rows, 0,
            "nothing was ever embedded, yet the corpus claims coverage"
        );
        assert_eq!(memory.missing_rows, 2);
        assert_eq!(memory.mismatched, 0);
    }

    /// Mismatched is kept apart from `missing_rows` too: one needs a re-embed, the other an embed.
    #[tokio::test]
    async fn a_vector_from_another_model_counts_as_mismatched_not_indexed() {
        let (tmp, index) = wire().await;
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        add_memory(&db.system, "mine", None).await;
        add_memory(&db.system, "theirs", None).await;

        index.upsert(&entry("mine", vec![1.0, 0.0])).await.unwrap();
        let mut foreign = entry("theirs", vec![1.0, 0.0]);
        foreign.model_id = "all-MiniLM-L6-v2".into();
        index.upsert(&foreign).await.unwrap();

        let health = index.health("nomic-embed-text-v1.5").await.unwrap();
        let memory = corpus_health(&health, Corpus::Memory);
        assert_eq!(memory.rows, 2);
        assert_eq!(
            memory.indexed_rows, 1,
            "a foreign-model vector was counted as coverage"
        );
        assert_eq!(memory.mismatched, 1);
        assert_eq!(
            memory.missing_rows, 0,
            "a row that HAS a vector was reported as never embedded"
        );
    }

    /// The orphan and archived memory are indexed but unreachable, so neither may count.
    #[tokio::test]
    async fn the_per_corpus_numbers_sum_to_the_global_totals() {
        let (tmp, index) = wire().await;
        let db = crate::db::Database::init(tmp.path()).await.unwrap();

        // Memory: one indexed, one from another model, one bare.
        add_memory(&db.system, "indexed", None).await;
        add_memory(&db.system, "foreign", None).await;
        add_memory(&db.system, "bare", None).await;
        index
            .upsert(&entry("indexed", vec![1.0, 0.0]))
            .await
            .unwrap();
        let mut foreign = entry("foreign", vec![1.0, 0.0]);
        foreign.model_id = "all-MiniLM-L6-v2".into();
        index.upsert(&foreign).await.unwrap();

        // An archived memory that IS indexed, and an orphan with no source row.
        add_memory(&db.system, "archived", None).await;
        index
            .upsert(&entry("archived", vec![1.0, 0.0]))
            .await
            .unwrap();
        sqlx::query("UPDATE memory_fragments SET lifecycle = 'archived' WHERE id = 'archived'")
            .execute(&db.system)
            .await
            .unwrap();
        index.upsert(&entry("ghost", vec![1.0, 0.0])).await.unwrap();

        // A summary that qualifies, and one refused because its session is unattributed.
        add_session(
            &db.system,
            "owned",
            Some("we discussed the garden"),
            "2026-08-13 10:00:00",
        )
        .await;
        add_session(
            &db.system,
            "guest",
            Some("a guest chatted"),
            "2026-08-13 10:00:00",
        )
        .await;
        sqlx::query("UPDATE sessions SET profile_id = NULL WHERE id = 'guest'")
            .execute(&db.system)
            .await
            .unwrap();

        let health = index.health("nomic-embed-text-v1.5").await.unwrap();
        assert_eq!(
            health.per_corpus.len(),
            Corpus::ALL.len(),
            "a corpus vanished from the health surface"
        );

        let sum = |f: fn(&CorpusHealth) -> u64| health.per_corpus.iter().map(f).sum::<u64>();
        assert_eq!(
            health.matching,
            sum(|c| c.indexed_rows),
            "the matching total is not the sum of the per-corpus coverage"
        );
        assert_eq!(health.mismatched, sum(|c| c.mismatched));
        assert_eq!(health.missing, sum(|c| c.missing_rows));

        // Each corpus's parts sum to its own qualifying row count.
        for c in &health.per_corpus {
            assert_eq!(
                c.rows,
                c.indexed_rows + c.missing_rows + c.mismatched,
                "{:?} does not add up: {c:?}",
                c.corpus
            );
        }

        // Pinned values, so the sums above cannot be satisfied by three zeros.
        let memory = corpus_health(&health, Corpus::Memory);
        assert_eq!(
            (
                memory.rows,
                memory.indexed_rows,
                memory.mismatched,
                memory.missing_rows
            ),
            (3, 1, 1, 1),
            "the archived memory or the orphan leaked into the memory corpus"
        );
        let summary = corpus_health(&health, Corpus::Summary);
        assert_eq!(
            summary.rows, 1,
            "the unattributed session was counted as indexable"
        );
        assert_eq!(summary.missing_rows, 1);
    }

    /// Pinned directly: `search` returns early for a guest, so behavioural tests never reach it.
    #[test]
    fn the_scope_predicate_refuses_a_guest_for_every_corpus() {
        for corpus in Corpus::ALL {
            let (pred, bind) = scope_sql(corpus, &ProfileScope::Guest);
            assert_eq!(
                pred, "AND 1 = 0",
                "{corpus:?} does not refuse a guest in SQL"
            );
            assert!(bind.is_none());
        }
        // Household is unrestricted, and an owner is restricted with a bind.
        assert_eq!(scope_sql(Corpus::Memory, &ProfileScope::Household).0, "");
        let (pred, bind) = scope_sql(Corpus::Memory, &ProfileScope::Owner("jerry".into()));
        assert!(pred.contains("profile_id = ?"), "got: {pred}");
        assert_eq!(bind.as_deref(), Some("jerry"));
    }

    #[tokio::test]
    async fn a_guest_sees_nothing_and_an_owner_sees_only_their_own() {
        let (tmp, index) = wire().await;
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        add_profile(&db.system, "jerry").await;
        add_profile(&db.system, "sam").await;
        add_memory(&db.system, "mine", Some("jerry")).await;
        add_memory(&db.system, "theirs", Some("sam")).await;
        add_memory(&db.system, "shared", None).await;
        for id in ["mine", "theirs", "shared"] {
            index.upsert(&entry(id, vec![1.0, 0.0])).await.unwrap();
        }

        let guest = index
            .search(
                &[1.0, 0.0],
                "nomic-embed-text-v1.5",
                &ProfileScope::Guest,
                10,
            )
            .await
            .unwrap();
        assert!(guest.is_empty(), "a guest reached the household's index");

        let owner = index
            .search(
                &[1.0, 0.0],
                "nomic-embed-text-v1.5",
                &ProfileScope::Owner("jerry".into()),
                10,
            )
            .await
            .unwrap();
        let ids: Vec<&str> = owner.iter().map(|h| h.row_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["mine", "shared"],
            "an owner must see their own rows and unattributed household ones, \
             and never another member's"
        );
    }
}

/// Write-through tests, kept here because they assert the index after a normal store write.
#[cfg(test)]
mod write_through_tests {
    use super::tests_support::*;
    use super::*;
    use pond_core::context::vector_index::VectorIndex as _;
    use pond_core::user_data::domain::memory::MemoryFragment;
    use pond_core::user_data::ports::memory_repository::MemoryRepository;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn a_stored_memory_is_immediately_searchable() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));
        let repo = crate::sqlite_memory::SqliteMemoryRepository::new(db.system.clone())
            .with_vector_index(index.clone(), Some("nomic-embed-text-v1.5".into()));

        let mut frag =
            MemoryFragment::from_chat("m1".into(), None, None, "the spare key is out back".into());
        frag.embedding = Some(vec![1.0, 0.0]);
        repo.add(frag).await.unwrap();

        let hits = index
            .search(
                &[1.0, 0.0],
                "nomic-embed-text-v1.5",
                &ProfileScope::Household,
                10,
            )
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "a stored memory was not searchable");
        assert_eq!(hits[0].row_id, "m1");
        assert_eq!(hits[0].corpus, Corpus::Memory);
    }

    /// Write-through must mirror the stored row, below the redactor that drops secret vectors.
    #[tokio::test]
    async fn a_fragment_whose_vector_was_dropped_contributes_nothing_to_the_index() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));
        let repo = crate::sqlite_memory::SqliteMemoryRepository::new(db.system.clone())
            .with_vector_index(index.clone(), Some("nomic-embed-text-v1.5".into()));

        // Vacuity control: a sibling with a vector proves indexing works.
        let mut ok = MemoryFragment::from_chat("kept".into(), None, None, "harmless".into());
        ok.embedding = Some(vec![1.0, 0.0]);
        repo.add(ok).await.unwrap();

        // And the one the redactor stripped: same path, no vector.
        let stripped =
            MemoryFragment::from_chat("secret".into(), None, None, "[redacted:api-key]".into());
        assert!(stripped.embedding.is_none());
        repo.add(stripped).await.unwrap();

        assert!(
            index.get(Corpus::Memory, "kept").await.unwrap().is_some(),
            "the write-through is not live, so this test proves nothing"
        );
        assert!(
            index.get(Corpus::Memory, "secret").await.unwrap().is_none(),
            "a fragment the redactor stripped still reached the index"
        );
    }

    #[tokio::test]
    async fn one_row_contributes_one_hit_however_many_chunks_it_owns() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        // `long` owns four chunks, one closest to the query; `short` owns one, slightly further.
        let repo = crate::sqlite_memory::SqliteMemoryRepository::new(db.system.clone());
        for (id, content) in [("long", "a long one"), ("short", "a short one")] {
            repo.add(MemoryFragment::from_chat(
                id.into(),
                None,
                None,
                content.into(),
            ))
            .await
            .unwrap();
        }

        for (ix, v) in [[0.9f32, 0.1], [0.95, 0.05], [1.0, 0.0], [0.8, 0.2]]
            .into_iter()
            .enumerate()
        {
            index
                .upsert(&VectorEntry {
                    corpus: Corpus::Memory,
                    row_id: "long".into(),
                    chunk_ix: ix as i64,
                    chunk_span: Some((0, 4)),
                    model_id: "m".into(),
                    vector: v.to_vec(),
                    source_rev: None,
                })
                .await
                .unwrap();
        }
        index
            .upsert(&VectorEntry {
                corpus: Corpus::Memory,
                row_id: "short".into(),
                chunk_ix: 0,
                chunk_span: None,
                model_id: "m".into(),
                vector: vec![0.85, 0.15],
                source_rev: None,
            })
            .await
            .unwrap();

        let hits = index
            .search_resolved(&[1.0, 0.0], "m", &ProfileScope::Household, 10)
            .await
            .unwrap();

        let long_hits = hits.iter().filter(|h| h.row_id == "long").count();
        assert_eq!(
            long_hits, 1,
            "a four-chunk row took {long_hits} slots: {hits:#?}"
        );
        // And it is the BEST chunk that represents it, not the first stored.
        let long = hits.iter().find(|h| h.row_id == "long").unwrap();
        assert!(
            (long.score - 1.0).abs() < 1e-6,
            "the row was represented by a weaker chunk: {}",
            long.score
        );
        assert!(hits.iter().any(|h| h.row_id == "short"), "{hits:#?}");
    }

    /// A process with no embedder (the `pond memories add` CLI) must leave the index alone.
    #[tokio::test]
    async fn an_unattributable_vector_is_left_alone_rather_than_removed() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        // The server already indexed this row.
        index
            .upsert(&VectorEntry {
                corpus: Corpus::Memory,
                row_id: "m1".into(),
                chunk_ix: 0,
                chunk_span: None,
                model_id: "nomic-embed-text-v1.5".into(),
                vector: vec![1.0, 0.0],
                source_rev: None,
            })
            .await
            .unwrap();

        // A second process stores the row with a vector it cannot name a model for.
        let no_model = crate::sqlite_memory::SqliteMemoryRepository::new(db.system.clone())
            .with_vector_index(index.clone(), None);
        let mut frag = MemoryFragment::from_chat("m1".into(), None, None, "kept".into());
        frag.embedding = Some(vec![1.0, 0.0]);
        no_model.add(frag).await.unwrap();

        assert!(
            index.get(Corpus::Memory, "m1").await.unwrap().is_some(),
            "a write with no embedder stripped an entry the server had written"
        );
    }

    /// Deleting a memory removes its vector, so the common case needs no sweep.
    #[tokio::test]
    async fn deleting_a_memory_drops_its_vector() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));
        let repo = crate::sqlite_memory::SqliteMemoryRepository::new(db.system.clone())
            .with_vector_index(index.clone(), Some("m".into()));

        let mut frag = MemoryFragment::from_chat("m1".into(), None, None, "x".into());
        frag.embedding = Some(vec![1.0, 0.0]);
        repo.add(frag).await.unwrap();
        repo.delete("m1").await.unwrap();

        assert!(index.get(Corpus::Memory, "m1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_archived_memory_is_not_returned_by_search() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_memory_with_vector(&db.system, "live", &[1.0, 0.0]).await;
        add_memory_with_vector(&db.system, "archived", &[1.0, 0.0]).await;
        index
            .backfill_from_source(Corpus::Memory, "m", 2)
            .await
            .unwrap();
        sqlx::query("UPDATE memory_fragments SET lifecycle = 'archived' WHERE id = 'archived'")
            .execute(&db.system)
            .await
            .unwrap();

        let hits = index
            .search(&[1.0, 0.0], "m", &ProfileScope::Household, 10)
            .await
            .unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.row_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["live"],
            "an archived memory came back from the index"
        );
    }

    #[tokio::test]
    async fn an_unattributed_session_summary_is_never_surfaced() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_profile(&db.system, "jerry").await;
        add_session(
            &db.system,
            "owned",
            Some("jerry chat"),
            "2026-08-13 10:00:00",
        )
        .await;
        add_session(
            &db.system,
            "guest",
            Some("guest chat"),
            "2026-08-13 10:00:00",
        )
        .await;
        sqlx::query("UPDATE sessions SET profile_id = 'jerry' WHERE id = 'owned'")
            .execute(&db.system)
            .await
            .unwrap();
        // The guest case: nobody was identified in this session.
        sqlx::query("UPDATE sessions SET profile_id = NULL WHERE id = 'guest'")
            .execute(&db.system)
            .await
            .unwrap();

        for id in ["owned", "guest"] {
            index
                .upsert(&VectorEntry {
                    corpus: Corpus::Summary,
                    row_id: id.into(),
                    chunk_ix: 0,
                    chunk_span: None,
                    model_id: "m".into(),
                    vector: vec![1.0, 0.0],
                    source_rev: Some("2026-08-13 10:00:00".into()),
                })
                .await
                .unwrap();
        }

        let hits = index
            .search(&[1.0, 0.0], "m", &ProfileScope::Household, 10)
            .await
            .unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.row_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["owned"],
            "an unattributed (guest) session summary reached a household read"
        );

        // Nor offered for embedding: no point paying inference for a row retrieval refuses.
        let todo = index
            .needs_embedding(Corpus::Summary, "other", 10)
            .await
            .unwrap();
        assert_eq!(todo, vec!["owned".to_string()]);
    }

    #[tokio::test]
    async fn resolved_search_returns_live_text_and_obeys_the_same_rules() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_memory_with_vector(&db.system, "m1", &[1.0, 0.0]).await;
        sqlx::query(
            "UPDATE memory_fragments SET content = 'the bill is eighty pounds' WHERE id='m1'",
        )
        .execute(&db.system)
        .await
        .unwrap();
        add_memory_with_vector(&db.system, "gone", &[1.0, 0.0]).await;
        index
            .backfill_from_source(Corpus::Memory, "m", 2)
            .await
            .unwrap();

        // A summary, which resolves from a different column entirely.
        add_session(
            &db.system,
            "s1",
            Some("we discussed the garden"),
            "2026-08-13 10:00:00",
        )
        .await;
        index
            .upsert(&VectorEntry {
                corpus: Corpus::Summary,
                row_id: "s1".into(),
                chunk_ix: 0,
                chunk_span: None,
                model_id: "m".into(),
                vector: vec![1.0, 0.0],
                source_rev: Some("2026-08-13 10:00:00".into()),
            })
            .await
            .unwrap();

        // Archive one: it must vanish from the resolved search too.
        sqlx::query("UPDATE memory_fragments SET lifecycle='archived' WHERE id='gone'")
            .execute(&db.system)
            .await
            .unwrap();

        let hits = index
            .search_resolved(&[1.0, 0.0], "m", &ProfileScope::Household, 10)
            .await
            .unwrap();
        let texts: Vec<&str> = hits.iter().map(|h| h.text.as_str()).collect();
        assert!(
            texts.contains(&"the bill is eighty pounds"),
            "memory text was not resolved: {texts:?}"
        );
        assert!(
            texts.contains(&"we discussed the garden"),
            "summary text was not resolved: {texts:?}"
        );
        assert!(
            !hits.iter().any(|h| h.row_id == "gone"),
            "an archived memory came back from the resolved search"
        );

        // Equal scores: memory first, via the `Corpus` declaration order.
        assert_eq!(
            hits[0].corpus,
            Corpus::Memory,
            "a summary outranked a memory on a tie"
        );

        // And a guest still gets nothing through this path.
        assert!(index
            .search_resolved(&[1.0, 0.0], "m", &ProfileScope::Guest, 10)
            .await
            .unwrap()
            .is_empty());
    }

    /// Bus events reach the index through the existing ingest write-through, not a subscriber.
    #[tokio::test]
    async fn an_ingested_item_reaches_the_index_and_disconnecting_removes_it() {
        use pond_core::context::domain::{SourceKind, SourceParts, SourceStatus};
        use pond_core::context::ingest::{IngestPipeline, RawItem};
        use pond_core::context::ports::ContextRepository;
        use pond_core::security::domain::redaction::RedactionKind;
        use pond_core::security::mocks::mock_redactor::MockRedactor;

        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));
        add_profile(&db.system, "jerry").await;

        let redactor = Arc::new(MockRedactor::replacing("nothing", RedactionKind::ApiKey));
        let repo = Arc::new(
            crate::sqlite_context::SqliteContextRepository::new(
                db.system.clone(),
                redactor.clone(),
            )
            .with_vector_index(index.clone(), Some("m".into())),
        );

        let source = pond_core::context::domain::ContextSource::from_parts(SourceParts {
            id: "src-sensor".into(),
            kind: SourceKind::Sensor,
            provider: "pond".into(),
            profile_id: "jerry".into(),
            scopes: vec![],
            cursor: None,
            last_sync: None,
            status: SourceStatus::Connected,
            secret_ref: None,
            created_at: chrono::Utc::now(),
        })
        .expect("valid source");
        repo.upsert_source(&source).await.unwrap();

        // Any vector will do; the point is that it reaches the index.
        struct E;
        #[async_trait]
        impl pond_core::models::ports::embedding::EmbeddingProvider for E {
            async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
                Ok(vec![1.0, 0.0])
            }
            fn dimensions(&self) -> usize {
                2
            }
            fn model_id(&self) -> String {
                "m".into()
            }
        }
        let pipeline = IngestPipeline::new(repo.clone(), redactor).with_embedder(Some(
            Arc::new(E) as Arc<dyn pond_core::models::ports::embedding::EmbeddingProvider>
        ));

        pipeline
            .ingest(
                &source,
                RawItem {
                    external_id: "evt-1".into(),
                    kind: pond_core::context::domain::ItemKind::Event,
                    occurred_at: chrono::Utc::now(),
                    title: "back door".into(),
                    body: "the back door opened".into(),
                    participants: vec![],
                },
                chrono::Utc::now(),
            )
            .await
            .expect("ingest");

        let hits = index
            .search_resolved(&[1.0, 0.0], "m", &ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "an ingested event never reached the index");
        assert_eq!(hits[0].corpus, Corpus::Context);
        assert!(
            hits[0].text.contains("the back door opened"),
            "resolved text was wrong: {}",
            hits[0].text
        );

        // Disconnecting is a deletion promise: vectors go now, not at the next sweep.
        repo.disconnect_source("src-sensor", &ProfileScope::Household)
            .await
            .unwrap();
        assert!(
            index
                .search_resolved(&[1.0, 0.0], "m", &ProfileScope::Household, 10)
                .await
                .unwrap()
                .is_empty(),
            "a disconnected source left its vectors behind"
        );
    }

    #[tokio::test]
    async fn a_passage_is_sliced_by_byte_not_by_character() {
        use pond_core::context::chunking::{chunk, DEFAULT_CHUNK_BYTES, DEFAULT_OVERLAP_BYTES};
        use pond_core::context::domain::{SourceKind, SourceParts, SourceStatus};
        use pond_core::context::ingest::{IngestPipeline, RawItem};
        use pond_core::context::ports::ContextRepository;
        use pond_core::security::domain::redaction::RedactionKind;
        use pond_core::security::mocks::mock_redactor::MockRedactor;

        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));
        add_profile(&db.system, "jerry").await;

        let redactor = Arc::new(MockRedactor::replacing("nothing", RedactionKind::ApiKey));
        let repo = Arc::new(
            crate::sqlite_context::SqliteContextRepository::new(
                db.system.clone(),
                redactor.clone(),
            )
            .with_vector_index(index.clone(), Some("m".into())),
        );
        let source = pond_core::context::domain::ContextSource::from_parts(SourceParts {
            id: "src-mail".into(),
            kind: SourceKind::Sensor,
            provider: "pond".into(),
            profile_id: "jerry".into(),
            scopes: vec![],
            cursor: None,
            last_sync: None,
            status: SourceStatus::Connected,
            secret_ref: None,
            created_at: chrono::Utc::now(),
        })
        .expect("valid source");
        repo.upsert_source(&source).await.unwrap();

        struct E;
        #[async_trait]
        impl pond_core::models::ports::embedding::EmbeddingProvider for E {
            async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
                Ok(vec![1.0, 0.0])
            }
            fn dimensions(&self) -> usize {
                2
            }
            fn model_id(&self) -> String {
                "m".into()
            }
        }
        let pipeline = IngestPipeline::new(repo.clone(), redactor).with_embedder(Some(
            Arc::new(E) as Arc<dyn pond_core::models::ports::embedding::EmbeddingProvider>
        ));

        // Multi-byte characters first, so every later span drifts under character indexing.
        let title = "Notice";
        let body = format!(
            "© 2026 — “quoted” café ©\n{}",
            "the invoice is attached and the rent is due on Friday. ".repeat(30)
        );
        pipeline
            .ingest(
                &source,
                RawItem {
                    external_id: "msg-1".into(),
                    kind: pond_core::context::domain::ItemKind::Message,
                    occurred_at: chrono::Utc::now(),
                    title: title.into(),
                    body: body.clone(),
                    participants: vec![],
                },
                chrono::Utc::now(),
            )
            .await
            .expect("ingest");

        // The text the index resolves against, mirroring `text_sql`.
        let text = format!("{title}\n{body}");
        let spans = chunk(&text, DEFAULT_CHUNK_BYTES, DEFAULT_OVERLAP_BYTES);
        assert!(spans.len() > 1, "test needs a body worth chunking");

        let row_id = index
            .needs_embedding(Corpus::Context, "m", 10)
            .await
            .unwrap()
            .first()
            .cloned()
            .unwrap_or_else(|| format!("{}:{}", source.id(), "msg-1"));

        index.remove(Corpus::Context, &row_id).await.unwrap();
        // The LAST chunk wins: chunk 0 starts at offset 0, where the bug can't show.
        let last = spans.len() - 1;
        for (ix, span) in spans.iter().enumerate() {
            index
                .upsert(&VectorEntry {
                    corpus: Corpus::Context,
                    row_id: row_id.clone(),
                    chunk_ix: ix as i64,
                    chunk_span: Some((span.start as i64, span.len as i64)),
                    model_id: "m".into(),
                    vector: if ix == last {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    },
                    source_rev: None,
                })
                .await
                .unwrap();
        }

        let hits = index
            .search_resolved(&[1.0, 0.0], "m", &ProfileScope::Household, 50)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "one row, rolled up from its best chunk");

        let expected = spans[last].slice(&text).expect("span is on a boundary");
        assert_eq!(
            hits[0].text, expected,
            "the resolved passage is not the bytes the chunk described"
        );
    }

    #[tokio::test]
    async fn three_way_isolation_two_members_and_a_guest() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_profile(&db.system, "jerry").await;
        add_profile(&db.system, "sam").await;
        add_memory_with_vector(&db.system, "jerry-own", &[1.0, 0.0]).await;
        add_memory_with_vector(&db.system, "sam-own", &[1.0, 0.0]).await;
        add_memory_with_vector(&db.system, "shared", &[1.0, 0.0]).await;
        sqlx::query("UPDATE memory_fragments SET profile_id='jerry' WHERE id='jerry-own'")
            .execute(&db.system)
            .await
            .unwrap();
        sqlx::query("UPDATE memory_fragments SET profile_id='sam' WHERE id='sam-own'")
            .execute(&db.system)
            .await
            .unwrap();
        index
            .backfill_from_source(Corpus::Memory, "m", 2)
            .await
            .unwrap();

        let ids = |hits: Vec<VectorHit>| {
            let mut v: Vec<String> = hits.into_iter().map(|h| h.row_id).collect();
            v.sort();
            v
        };

        let jerry = ids(index
            .search(&[1.0, 0.0], "m", &ProfileScope::Owner("jerry".into()), 10)
            .await
            .unwrap());
        assert_eq!(jerry, vec!["jerry-own", "shared"], "jerry's view is wrong");

        let sam = ids(index
            .search(&[1.0, 0.0], "m", &ProfileScope::Owner("sam".into()), 10)
            .await
            .unwrap());
        assert_eq!(sam, vec!["sam-own", "shared"], "sam's view is wrong");

        let guest = index
            .search(&[1.0, 0.0], "m", &ProfileScope::Guest, 10)
            .await
            .unwrap();
        assert!(guest.is_empty(), "a guest reached the household's index");
    }

    /// Memory sweeps only pick `embedding IS NULL` rows, so embedded ones must be adopted.
    #[tokio::test]
    async fn an_already_embedded_memory_is_adopted_when_the_index_is_rebuilt() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        // A store that is fully embedded, and an index that knows nothing.
        add_memory_with_vector(&db.system, "m1", &vec![0.5f32; 8]).await;
        add_memory_with_vector(&db.system, "m2", &vec![0.25f32; 8]).await;
        assert!(index.get(Corpus::Memory, "m1").await.unwrap().is_none());

        let copied = index
            .backfill_from_source(Corpus::Memory, "m", 8)
            .await
            .unwrap();
        assert_eq!(copied, 2, "existing vectors were not adopted");
        let got = index.get(Corpus::Memory, "m1").await.unwrap().unwrap();
        assert_eq!(
            got.vector,
            vec![0.5f32; 8],
            "the adopted vector is not the stored one"
        );
        assert_eq!(got.model_id, "m");

        // Idempotent: a second pass must not duplicate or re-copy.
        assert_eq!(
            index
                .backfill_from_source(Corpus::Memory, "m", 8)
                .await
                .unwrap(),
            0,
            "adoption ran twice over the same rows"
        );
    }

    #[tokio::test]
    async fn adoption_refuses_a_vector_of_the_wrong_width() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_memory_with_vector(&db.system, "current", &vec![0.5f32; 8]).await;
        add_memory_with_vector(&db.system, "legacy", &vec![0.5f32; 4]).await;

        let copied = index
            .backfill_from_source(Corpus::Memory, "m", 8)
            .await
            .unwrap();
        assert_eq!(copied, 1);
        assert!(index
            .get(Corpus::Memory, "current")
            .await
            .unwrap()
            .is_some());
        assert!(
            index.get(Corpus::Memory, "legacy").await.unwrap().is_none(),
            "a 4-wide vector was adopted as if this model had produced it"
        );
    }

    #[tokio::test]
    async fn a_vector_stored_without_a_revision_does_not_re_embed_forever() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_session(&db.system, "s1", Some("a summary"), "2026-08-13 10:00:00").await;
        index
            .upsert(&VectorEntry {
                corpus: Corpus::Summary,
                row_id: "s1".into(),
                chunk_ix: 0,
                chunk_span: None,
                model_id: "m".into(),
                vector: vec![1.0, 0.0],
                // No stamp -- what a writer that does not track revisions leaves.
                source_rev: None,
            })
            .await
            .unwrap();

        let todo = index
            .needs_embedding(Corpus::Summary, "m", 10)
            .await
            .unwrap();
        assert!(
            todo.is_empty(),
            "an un-stamped vector was reported stale, so the sweep would re-embed \
             it on every pass and never converge"
        );
    }

    /// Detected by the sweep alone: `source_rev` stops matching `rolling_summary_updated_at`.
    #[tokio::test]
    async fn a_resummarised_session_is_reported_stale_and_its_vector_changes() {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index: Arc<dyn VectorIndex> = Arc::new(SqliteVectorIndex::new(db.vectors.clone()));

        add_session(
            &db.system,
            "s1",
            Some("first summary"),
            "2026-08-13 10:00:00",
        )
        .await;

        // Not yet indexed -> the sweep offers it.
        let todo = index
            .needs_embedding(Corpus::Summary, "m", 10)
            .await
            .unwrap();
        assert_eq!(todo, vec!["s1".to_string()]);

        // Index it at the revision the sweep would have read.
        index
            .upsert(&VectorEntry {
                corpus: Corpus::Summary,
                row_id: "s1".into(),
                chunk_ix: 0,
                chunk_span: None,
                model_id: "m".into(),
                vector: vec![1.0, 0.0],
                source_rev: Some("2026-08-13 10:00:00".into()),
            })
            .await
            .unwrap();

        // Now current: the sweep must not offer it again.
        let todo = index
            .needs_embedding(Corpus::Summary, "m", 10)
            .await
            .unwrap();
        assert!(todo.is_empty(), "an up-to-date summary was reported stale");

        // Re-summarise: the column is rewritten in place with a new stamp.
        set_summary(&db.system, "s1", "a better summary", "2026-08-13 11:00:00").await;
        let todo = index
            .needs_embedding(Corpus::Summary, "m", 10)
            .await
            .unwrap();
        assert_eq!(
            todo,
            vec!["s1".to_string()],
            "a re-summarised session was not reported stale, so its vector would never change"
        );

        // And re-indexing REPLACES rather than appends.
        index
            .upsert(&VectorEntry {
                corpus: Corpus::Summary,
                row_id: "s1".into(),
                chunk_ix: 0,
                chunk_span: None,
                model_id: "m".into(),
                vector: vec![0.0, 1.0],
                source_rev: Some("2026-08-13 11:00:00".into()),
            })
            .await
            .unwrap();
        let got = index.get(Corpus::Summary, "s1").await.unwrap().unwrap();
        assert_eq!(
            got.vector,
            vec![0.0, 1.0],
            "the old summary vector survived"
        );
    }
}

#[cfg(test)]
mod tests_support {
    use sqlx::{Pool, Sqlite};

    /// Creates the session attributed to a member; unattributed summaries are never retrievable.
    pub async fn add_session(pool: &Pool<Sqlite>, id: &str, summary: Option<&str>, updated: &str) {
        sqlx::query("INSERT OR IGNORE INTO profiles (id, display_name) VALUES ('owner','Owner')")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO sessions (id, created_at, profile_id) VALUES (?, datetime('now'), 'owner')",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
        if let Some(s) = summary {
            set_summary(pool, id, s, updated).await;
        }
    }

    pub async fn add_profile(pool: &Pool<Sqlite>, id: &str) {
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
            .bind(id)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    pub async fn add_memory_with_vector(pool: &Pool<Sqlite>, id: &str, v: &[f32]) {
        let blob: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        sqlx::query(
            "INSERT INTO memory_fragments (id, content, embedding, source, tags, created_at, \
             access_count, lifecycle) VALUES (?, 'x', ?, 'chat', '[]', datetime('now'), 0, 'active')",
        )
        .bind(id)
        .bind(blob)
        .execute(pool)
        .await
        .unwrap();
    }

    pub async fn set_summary(pool: &Pool<Sqlite>, id: &str, summary: &str, updated: &str) {
        sqlx::query(
            "UPDATE sessions SET rolling_summary = ?, rolling_summary_updated_at = ? WHERE id = ?",
        )
        .bind(summary)
        .bind(updated)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }
}
