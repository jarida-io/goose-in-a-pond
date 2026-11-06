//! SQLite-backed [`VectorIndex`] over `pond_vectors.db`, keyed by item.
//!
//! Each connection ATTACHes the system DB as `sys`, so reads JOIN live headers. Only this file
//! is written (cross-database transactions aren't atomic under WAL), hence orphans are expected.
//! Similarity is brute-force cosine on purpose: sub-millisecond at household scale.

use std::collections::HashMap;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use pond_core::context::vector_index::{
    IndexHealth, KindHealth, ResolvedHit, Stale, VectorEntry, VectorHit, VectorIndex, INDEXED,
};
use pond_core::items::{embedded_text, Kind};
use pond_core::user_data::domain::profile::ProfileScope;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, Pool, Sqlite};
use std::path::Path;
use std::str::FromStr;

use crate::sqlite_items;

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

/// `i.kind IN (...)`; the names come from [`Kind::as_str`], never from a caller's text.
fn kinds_sql(kinds: &[Kind]) -> String {
    if kinds.is_empty() {
        return "1 = 0".to_string();
    }
    let names: Vec<String> = kinds.iter().map(|k| format!("'{}'", k.as_str())).collect();
    format!("i.kind IN ({})", names.join(", "))
}

/// An item retrieval can return: active, and with text in a field its kind is embedded from.
/// Every query shares it, so none can reach an archived fact or an empty conversation.
fn live_sql() -> String {
    let cases: Vec<String> = INDEXED
        .iter()
        .map(|kind| {
            let any: Vec<String> = kind
                .embedded_fields()
                .iter()
                .map(|f| format!("i.{} <> ''", f.column()))
                .collect();
            format!("WHEN '{}' THEN ({})", kind.as_str(), any.join(" OR "))
        })
        .collect();
    format!(
        "i.state = 'active' AND (CASE i.kind {} ELSE 0 END)",
        cases.join(" ")
    )
}

/// The slot's order: an item's position in [`INDEXED`].
fn order_sql() -> String {
    let cases: Vec<String> = INDEXED
        .iter()
        .enumerate()
        .map(|(n, k)| format!("WHEN '{}' THEN {n}", k.as_str()))
        .collect();
    format!("CASE i.kind {} ELSE {} END", cases.join(" "), INDEXED.len())
}

/// Ties go to what was said, then what was observed, then a lossy summary.
fn rank(kind: Kind) -> u8 {
    match kind {
        Kind::Fact | Kind::Trait | Kind::Entity => 0,
        Kind::Conversation => 2,
        _ => 1,
    }
}

/// The passage a vector was made from: a byte span into the embedded text, or all of it.
fn passage(text: String, start: Option<i64>, len: Option<i64>) -> String {
    match (start, len) {
        (Some(s), Some(l)) if s >= 0 && l >= 0 => text
            .get(s as usize..(s + l) as usize)
            .map(str::to_string)
            .unwrap_or(text),
        _ => text,
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

    /// Every matching vector's item, kind and score, one per item (its best passage).
    async fn scored(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        kinds: &[Kind],
    ) -> Result<Vec<(String, Kind, f32, String)>> {
        let (scope_pred, binds) = sqlite_items::scope_sql(scope, "sys.");
        // The JOIN doubles as the existence check: orphans never match.
        let sql = format!(
            "SELECT v.item_id, i.kind, v.vector, v.chunk_start, v.chunk_len, \
                    i.context, i.title, i.body \
             FROM vectors v JOIN sys.items i ON i.id = v.item_id \
             WHERE v.model_id = ? AND {} AND {} {scope_pred}",
            kinds_sql(kinds),
            live_sql()
        );
        type Row = (
            String,
            String,
            Vec<u8>,
            Option<i64>,
            Option<i64>,
            String,
            String,
            String,
        );
        let mut q = sqlx::query_as::<_, Row>(&sql).bind(model_id);
        for b in binds {
            q = q.bind(b);
        }
        let mut best: HashMap<String, (String, Kind, f32, String)> = HashMap::new();
        for (item_id, kind, blob, start, len, context, title, body) in
            q.fetch_all(&self.pool).await?
        {
            let Some(kind) = Kind::parse(&kind) else {
                continue;
            };
            // Wrong width is not a bad match but no match: skip it.
            let Some(score) = cosine(query, &blob_to_vec(&blob)) else {
                continue;
            };
            if best.get(&item_id).is_some_and(|b| b.2 >= score) {
                continue;
            }
            let text = passage(embedded_text(kind, &context, &title, &body), start, len);
            best.insert(item_id.clone(), (item_id, kind, score, text));
        }
        let mut scored: Vec<_> = best.into_values().collect();
        // Score, then kind order (memory wins ties), then id, so equal scores never reorder.
        scored.sort_by(|a, b| {
            b.2.partial_cmp(&a.2)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| rank(a.1).cmp(&rank(b.1)))
                .then_with(|| a.0.cmp(&b.0))
        });
        Ok(scored)
    }
}

#[async_trait]
impl VectorIndex for SqliteVectorIndex {
    async fn upsert(&self, entry: &VectorEntry) -> Result<()> {
        sqlx::query(
            "INSERT INTO vectors \
               (item_id, chunk_ix, chunk_start, chunk_len, model_id, dims, vector, \
                source_rev, embedded_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(item_id, chunk_ix) DO UPDATE SET \
               chunk_start = excluded.chunk_start, chunk_len = excluded.chunk_len, \
               model_id = excluded.model_id, dims = excluded.dims, vector = excluded.vector, \
               source_rev = excluded.source_rev, embedded_at = excluded.embedded_at",
        )
        .bind(&entry.item_id)
        .bind(entry.chunk_ix)
        .bind(entry.chunk_span.map(|(start, _)| start))
        .bind(entry.chunk_span.map(|(_, len)| len))
        .bind(&entry.model_id)
        .bind(entry.vector.len() as i64)
        .bind(vec_to_blob(&entry.vector))
        .bind(&entry.source_rev)
        .bind(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn remove(&self, item_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM vectors WHERE item_id = ?")
            .bind(item_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn get(&self, item_id: &str) -> Result<Option<VectorEntry>> {
        let row: Option<(String, Vec<u8>, Option<String>, Option<i64>, Option<i64>)> =
            sqlx::query_as(
                "SELECT model_id, vector, source_rev, chunk_start, chunk_len FROM vectors \
                 WHERE item_id = ? AND chunk_ix = 0",
            )
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(
            row.map(|(model_id, blob, source_rev, start, len)| VectorEntry {
                item_id: item_id.to_string(),
                chunk_ix: 0,
                chunk_span: start.zip(len),
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
        kinds: &[Kind],
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        if query.is_empty() || limit == 0 || scope.excludes_everything() {
            return Ok(vec![]);
        }
        let mut scored = self.scored(query, model_id, scope, kinds).await?;
        scored.truncate(limit);
        Ok(scored
            .into_iter()
            .map(|(item_id, kind, score, _)| VectorHit {
                item_id,
                kind,
                score,
            })
            .collect())
    }

    async fn search_resolved(
        &self,
        query: &[f32],
        model_id: &str,
        scope: &ProfileScope,
        kinds: &[Kind],
        limit: usize,
    ) -> Result<Vec<ResolvedHit>> {
        if query.is_empty() || limit == 0 || scope.excludes_everything() {
            return Ok(vec![]);
        }
        let mut scored = self.scored(query, model_id, scope, kinds).await?;
        scored.truncate(limit);
        Ok(scored
            .into_iter()
            .map(|(item_id, kind, score, text)| ResolvedHit {
                item_id,
                kind,
                score,
                text,
            })
            .collect())
    }

    async fn needs_embedding(
        &self,
        kinds: &[Kind],
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<Stale>> {
        // Chunk 0 stands for the item: every passage is written under one revision.
        let sql = format!(
            "SELECT i.id, i.kind, i.context, i.title, i.body, i.updated_at \
             FROM sys.items i LEFT JOIN vectors v ON v.item_id = i.id AND v.chunk_ix = 0 \
             WHERE {} AND {} \
               AND (v.item_id IS NULL OR v.model_id <> ? OR v.source_rev IS NOT i.updated_at) \
             ORDER BY {}, i.id LIMIT ?",
            kinds_sql(kinds),
            live_sql(),
            order_sql()
        );
        let rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(&sql)
            .bind(model_id)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(item_id, kind, context, title, body, rev)| {
                let kind = Kind::parse(&kind)?;
                Some(Stale {
                    item_id,
                    kind,
                    text: embedded_text(kind, &context, &title, &body),
                    rev,
                })
            })
            .collect())
    }

    async fn prune_orphans(&self) -> Result<u64> {
        let removed =
            sqlx::query("DELETE FROM vectors WHERE item_id NOT IN (SELECT id FROM sys.items)")
                .execute(&self.pool)
                .await?
                .rows_affected();
        if removed > 0 {
            tracing::info!(removed, "pruned vector index orphans");
        }
        Ok(removed)
    }

    /// Counted per kind from the header side: coverage of what retrieval can reach, not of the
    /// file, so orphans and archived items are excluded.
    async fn health(&self, model_id: &str) -> Result<IndexHealth> {
        let sql = format!(
            "SELECT i.kind, COUNT(*), \
                    COALESCE(SUM(CASE WHEN v.model_id = ? THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN v.item_id IS NULL THEN 1 ELSE 0 END), 0) \
             FROM sys.items i LEFT JOIN vectors v ON v.item_id = i.id AND v.chunk_ix = 0 \
             WHERE {} GROUP BY i.kind",
            live_sql()
        );
        let live: Vec<(String, i64, i64, i64)> = sqlx::query_as(&sql)
            .bind(model_id)
            .fetch_all(&self.pool)
            .await?;
        let all: Vec<(String, i64)> =
            sqlx::query_as("SELECT kind, COUNT(*) FROM sys.items GROUP BY kind")
                .fetch_all(&self.pool)
                .await?;

        let mut totals = IndexHealth::default();
        for kind in INDEXED {
            let (rows, indexed_rows, missing_rows) = live
                .iter()
                .find(|(k, ..)| k == kind.as_str())
                .map(|(_, r, i, m)| (*r as u64, *i as u64, *m as u64))
                .unwrap_or((0, 0, 0));
            let source_rows = all
                .iter()
                .find(|(k, _)| k == kind.as_str())
                .map(|(_, n)| *n as u64)
                .unwrap_or(0);
            // Subtracted, not counted, so the three buckets always sum to `rows`.
            let mismatched = rows
                .saturating_sub(indexed_rows)
                .saturating_sub(missing_rows);
            totals.matching += indexed_rows;
            totals.mismatched += mismatched;
            totals.missing += missing_rows;
            totals.per_kind.push(KindHealth {
                kind,
                rows,
                source_rows,
                indexed_rows,
                missing_rows,
                mismatched,
            });
        }
        Ok(totals)
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use super::*;
    use crate::sqlite_memory::SqliteMemoryRepository;
    use pond_core::context::vector_index::MEMORY;
    use pond_core::user_data::domain::memory::{MemoryFragment, MemoryLifecycle};
    use pond_core::user_data::ports::memory_repository::MemoryRepository;
    use std::sync::Arc;
    use tempfile::TempDir;

    struct Pond {
        _tmp: TempDir,
        system: Pool<Sqlite>,
        index: Arc<dyn VectorIndex>,
    }

    async fn pond() -> Pond {
        let tmp = TempDir::new().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        Pond {
            _tmp: tmp,
            index: Arc::new(SqliteVectorIndex::new(db.vectors.clone())),
            system: db.system,
        }
    }

    impl Pond {
        fn memories(&self, model: &str, dims: usize) -> SqliteMemoryRepository {
            SqliteMemoryRepository::new(self.system.clone())
                .with_vector_index(self.index.clone(), stamp(model, dims))
        }

        async fn search(&self, q: &[f32], model: &str, scope: &ProfileScope) -> Vec<String> {
            self.index
                .search(q, model, scope, &INDEXED, 10)
                .await
                .unwrap()
                .into_iter()
                .map(|h| h.item_id)
                .collect()
        }
    }

    #[tokio::test]
    async fn a_vector_roundtrips_and_re_embedding_replaces_it() {
        let p = pond().await;
        let m1 = add_memory(&p.system, "m1", None, "the spare key").await;
        p.index
            .upsert(&VectorEntry::whole(
                m1.clone(),
                "m".into(),
                vec![1.0, 0.0],
                None,
            ))
            .await
            .unwrap();
        p.index
            .upsert(&VectorEntry::whole(
                m1.clone(),
                "m".into(),
                vec![0.0, 1.0],
                None,
            ))
            .await
            .unwrap();
        let got = p.index.get(&m1).await.unwrap().unwrap();
        assert_eq!(got.vector, vec![0.0, 1.0]);
        assert!(p.index.get("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn deleting_the_index_file_rebuilds_it_empty_and_usable() {
        let tmp = TempDir::new().unwrap();
        let m1 = {
            let db = crate::db::Database::init(tmp.path()).await.unwrap();
            let index = SqliteVectorIndex::new(db.vectors.clone());
            let m1 = add_memory(&db.system, "m1", None, "x").await;
            index
                .upsert(&VectorEntry::whole(m1.clone(), "m".into(), vec![1.0], None))
                .await
                .unwrap();
            m1
        };
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(tmp.path().join(format!("pond_vectors.db{suffix}")));
        }
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let index = SqliteVectorIndex::new(db.vectors.clone());
        assert!(index.get(&m1).await.unwrap().is_none());
        let stale = index.needs_embedding(&INDEXED, "m", 10).await.unwrap();
        assert_eq!(
            stale.len(),
            1,
            "the memory is found again by the staleness join"
        );
    }

    #[tokio::test]
    async fn an_orphan_matches_nothing_and_is_pruned() {
        let p = pond().await;
        let live = add_memory(&p.system, "live", None, "x").await;
        index_whole(&p.index, &live, &[1.0, 0.0]).await;
        index_whole(
            &p.index,
            "0190d8d5-0000-7000-8000-000000000000",
            &[1.0, 0.0],
        )
        .await;
        assert_eq!(
            p.search(&[1.0, 0.0], "m", &ProfileScope::Household).await,
            vec![live.clone()]
        );
        assert_eq!(p.index.prune_orphans().await.unwrap(), 1);
        assert!(p.index.get(&live).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_vector_from_another_model_is_never_returned_and_is_stale() {
        let p = pond().await;
        let m1 = add_memory(&p.system, "m1", None, "x").await;
        p.index
            .upsert(&VectorEntry::whole(
                m1.clone(),
                "old".into(),
                vec![1.0, 0.0],
                None,
            ))
            .await
            .unwrap();
        assert!(p
            .search(&[1.0, 0.0], "new", &ProfileScope::Household)
            .await
            .is_empty());
        let stale = p.index.needs_embedding(&INDEXED, "new", 10).await.unwrap();
        assert_eq!(
            stale.iter().map(|s| s.item_id.clone()).collect::<Vec<_>>(),
            vec![m1]
        );
    }

    #[tokio::test]
    async fn a_vector_older_than_its_header_is_stale_and_a_current_one_is_not() {
        let p = pond().await;
        let m1 = add_memory(&p.system, "m1", None, "x").await;
        let rev = rev_of(&p.system, &m1).await;
        p.index
            .upsert(&VectorEntry::whole(
                m1.clone(),
                "m".into(),
                vec![1.0],
                Some(rev),
            ))
            .await
            .unwrap();
        assert!(p
            .index
            .needs_embedding(&INDEXED, "m", 10)
            .await
            .unwrap()
            .is_empty());
        sqlx::query(
            "UPDATE items SET body = 'y', updated_at = '2099-01-01T00:00:00.000Z' WHERE id = ?",
        )
        .bind(&m1)
        .execute(&p.system)
        .await
        .unwrap();
        let stale = p.index.needs_embedding(&INDEXED, "m", 10).await.unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(
            (stale[0].text.as_str(), stale[0].rev.as_str()),
            ("y", "2099-01-01T00:00:00.000Z")
        );
    }

    #[tokio::test]
    async fn the_backlog_is_memory_first_and_a_chunked_item_is_offered_once() {
        let p = pond().await;
        add_session(&p.system, "s1", Some("We planned the vents"), "2026-11-01").await;
        sync_all(&p.system).await;
        let m1 = add_memory(&p.system, "m1", None, "x").await;
        for chunk_ix in 0..3 {
            p.index
                .upsert(&VectorEntry {
                    chunk_ix,
                    chunk_span: Some((0, 1)),
                    ..VectorEntry::whole(m1.clone(), "old".into(), vec![1.0], None)
                })
                .await
                .unwrap();
        }
        let stale = p.index.needs_embedding(&INDEXED, "new", 10).await.unwrap();
        let kinds: Vec<Kind> = stale.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![Kind::Fact, Kind::Conversation]);
        assert_eq!(stale[1].text, "We planned the vents");
    }

    #[tokio::test]
    async fn every_indexed_kind_reports_health_and_the_buckets_sum() {
        let p = pond().await;
        let indexed = add_memory(&p.system, "a", None, "x").await;
        let other = add_memory(&p.system, "b", None, "y").await;
        add_memory(&p.system, "c", None, "z").await;
        add_memory(&p.system, "d", None, "w").await;
        sqlx::query("UPDATE memory_fragments SET lifecycle = 'archived' WHERE id = 'd'")
            .execute(&p.system)
            .await
            .unwrap();
        sync_all(&p.system).await;
        index_whole(&p.index, &indexed, &[1.0]).await;
        p.index
            .upsert(&VectorEntry::whole(other, "old".into(), vec![1.0], None))
            .await
            .unwrap();

        let h = p.index.health("m").await.unwrap();
        assert_eq!(
            h.per_kind.iter().map(|k| k.kind).collect::<Vec<_>>(),
            INDEXED.to_vec()
        );
        let fact = h.per_kind.iter().find(|k| k.kind == Kind::Fact).unwrap();
        assert_eq!(
            (
                fact.rows,
                fact.source_rows,
                fact.indexed_rows,
                fact.missing_rows,
                fact.mismatched
            ),
            (3, 4, 1, 1, 1),
            "the archived memory is a header but not a row retrieval can reach"
        );
        let sum = |f: fn(&KindHealth) -> u64| h.per_kind.iter().map(f).sum::<u64>();
        assert_eq!(sum(|k| k.indexed_rows), h.matching);
        assert_eq!(sum(|k| k.missing_rows), h.missing);
        assert_eq!(sum(|k| k.mismatched), h.mismatched);
    }

    #[tokio::test]
    async fn a_guest_sees_nothing_and_members_see_their_own_and_the_households() {
        let p = pond().await;
        add_profile(&p.system, "amina").await;
        add_profile(&p.system, "baraka").await;
        let hers = add_memory(&p.system, "hers", Some("amina"), "a").await;
        let his = add_memory(&p.system, "his", Some("baraka"), "b").await;
        let ours = add_memory(&p.system, "ours", None, "c").await;
        for id in [&hers, &his, &ours] {
            index_whole(&p.index, id, &[1.0, 0.0]).await;
        }
        let sorted = |mut v: Vec<String>| {
            v.sort();
            v
        };
        assert!(p
            .search(&[1.0, 0.0], "m", &ProfileScope::Guest)
            .await
            .is_empty());
        assert_eq!(
            sorted(
                p.search(&[1.0, 0.0], "m", &ProfileScope::Owner("amina".into()))
                    .await
            ),
            sorted(vec![hers.clone(), ours.clone()])
        );
        assert_eq!(
            sorted(
                p.search(&[1.0, 0.0], "m", &ProfileScope::Owner("baraka".into()))
                    .await
            ),
            sorted(vec![his.clone(), ours.clone()])
        );
        assert_eq!(
            p.search(&[1.0, 0.0], "m", &ProfileScope::Household)
                .await
                .len(),
            3
        );

        sqlx::query(
            "INSERT INTO item_shares (item_id, profile_id, created_at) VALUES (?, 'baraka', ?)",
        )
        .bind(&hers)
        .bind(pond_core::items::timestamp::now())
        .execute(&p.system)
        .await
        .unwrap();
        assert!(
            p.search(&[1.0, 0.0], "m", &ProfileScope::Owner("baraka".into()))
                .await
                .contains(&hers),
            "an item shared with a member is theirs to find"
        );
        let (pred, _) = sqlite_items::scope_sql(&ProfileScope::Guest, "sys.");
        assert_eq!(pred, "AND 1 = 0");
    }

    #[tokio::test]
    async fn an_archived_memory_and_an_unattributed_conversation_are_never_found() {
        let p = pond().await;
        let gone = add_memory(&p.system, "gone", None, "x").await;
        index_whole(&p.index, &gone, &[1.0, 0.0]).await;
        let repo = p.memories("m", 2);
        repo.update_lifecycle("gone", MemoryLifecycle::Archived)
            .await
            .unwrap();
        assert!(p
            .search(&[1.0, 0.0], "m", &ProfileScope::Household)
            .await
            .is_empty());

        sqlx::query(
            "INSERT INTO sessions (id, created_at, updated_at, rolling_summary) \
             VALUES ('nobody', datetime('now'), datetime('now'), 'A guest asked about the wifi')",
        )
        .execute(&p.system)
        .await
        .unwrap();
        sync_all(&p.system).await;
        assert!(
            p.index
                .needs_embedding(&INDEXED, "m", 10)
                .await
                .unwrap()
                .is_empty(),
            "an unattributed conversation has no header, so nothing to embed"
        );
    }

    #[tokio::test]
    async fn resolved_search_returns_the_best_passage_of_the_redacted_header() {
        let p = pond().await;
        add_session(
            &p.system,
            "s1",
            Some("We agreed on Tuesday. Key sk-abcdefghijklmnopqrstuvwxyz123456."),
            "2026-11-01",
        )
        .await;
        sync_all(&p.system).await;
        let s1 = item_of(&p.system, "sessions", "s1").await;
        for (ix, (span, v)) in [((0, 20), [1.0, 0.0]), ((21, 10), [0.0, 1.0])]
            .iter()
            .enumerate()
        {
            p.index
                .upsert(&VectorEntry {
                    item_id: s1.clone(),
                    chunk_ix: ix as i64,
                    chunk_span: Some(*span),
                    model_id: "m".into(),
                    vector: v.to_vec(),
                    source_rev: None,
                })
                .await
                .unwrap();
        }
        let hits = p
            .index
            .search_resolved(
                &[1.0, 0.0],
                "m",
                &ProfileScope::Owner("owner".into()),
                &INDEXED,
                10,
            )
            .await
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "one item is one hit, however many passages it has"
        );
        assert_eq!(hits[0].kind, Kind::Conversation);
        assert_eq!(hits[0].text, "We agreed on Tuesday");
        let whole = p
            .index
            .search_resolved(&[0.0, 1.0], "m", &ProfileScope::Household, &INDEXED, 10)
            .await
            .unwrap();
        assert!(!whole[0].text.contains("sk-abc"), "{}", whole[0].text);
    }

    #[tokio::test]
    async fn a_passage_is_sliced_by_byte_and_a_bad_span_reads_the_whole_text() {
        assert_eq!(
            passage("Mzee é Tuesday".into(), Some(8), Some(7)),
            "Tuesday"
        );
        assert_eq!(
            passage("é".into(), Some(1), Some(1)),
            "é",
            "not a char boundary"
        );
        assert_eq!(passage("abc".into(), Some(2), Some(9)), "abc");
        assert_eq!(passage("abc".into(), None, None), "abc");
    }

    #[tokio::test]
    async fn a_stored_memory_is_immediately_searchable_and_deleting_it_drops_its_vector() {
        let p = pond().await;
        let repo = p.memories("m", 2);
        let mut frag = MemoryFragment::from_chat("m1".into(), None, None, "the spare key".into());
        frag.embedding = Some(vec![1.0, 0.0]);
        repo.add(frag).await.unwrap();
        let m1 = item_of(&p.system, "memory_fragments", "m1").await;
        assert_eq!(
            p.search(&[1.0, 0.0], "m", &ProfileScope::Household).await,
            vec![m1.clone()]
        );
        assert!(p
            .index
            .needs_embedding(&INDEXED, "m", 10)
            .await
            .unwrap()
            .is_empty());

        let found = repo
            .search_similar(&[1.0, 0.0], &ProfileScope::Household, 5)
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].embedding.as_deref(), Some(&[1.0, 0.0][..]));

        repo.delete("m1").await.unwrap();
        assert!(p.index.get(&m1).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rewording_a_memory_replaces_its_vector_or_leaves_it_to_the_slot() {
        let p = pond().await;
        let repo = p.memories("m", 2);
        let mut frag = MemoryFragment::from_chat("w1".into(), None, None, "the old words".into());
        frag.embedding = Some(vec![1.0, 0.0]);
        repo.add(frag).await.unwrap();
        let w1 = item_of(&p.system, "memory_fragments", "w1").await;

        repo.update_content("w1", "the new words", Some(&[0.0, 1.0]))
            .await
            .unwrap();
        assert_eq!(
            p.index.get(&w1).await.unwrap().unwrap().vector,
            vec![0.0, 1.0]
        );
        assert!(p
            .index
            .needs_embedding(&INDEXED, "m", 10)
            .await
            .unwrap()
            .is_empty());

        repo.update_content("w1", "newer words", None)
            .await
            .unwrap();
        assert!(p.index.get(&w1).await.unwrap().is_none());
        let stale = p.index.needs_embedding(&INDEXED, "m", 10).await.unwrap();
        assert_eq!(stale[0].text, "newer words");

        repo.update_content("missing", "x", Some(&[1.0, 0.0]))
            .await
            .unwrap();
        assert_eq!(p.index.health("m").await.unwrap().matching, 0);
    }

    #[tokio::test]
    async fn memory_search_never_compares_another_models_vector() {
        let p = pond().await;
        let m1 = add_memory(&p.system, "m1", None, "x").await;
        p.index
            .upsert(&VectorEntry::whole(
                m1,
                "other".into(),
                vec![1.0, 0.0],
                None,
            ))
            .await
            .unwrap();
        let repo = p.memories("m", 2);
        let found = repo
            .search_similar(&[1.0, 0.0], &ProfileScope::Household, 5)
            .await
            .unwrap();
        assert_eq!(found.len(), 1, "it falls back to recency");
        assert!(found[0].embedding.is_none(), "with no vector to band by");
    }

    #[tokio::test]
    async fn a_vector_of_another_width_or_from_a_loading_model_is_not_stamped() {
        let p = pond().await;
        let repo = p.memories("m", 3);
        let mut frag = MemoryFragment::from_chat("m1".into(), None, None, "x".into());
        frag.embedding = Some(vec![1.0, 0.0]);
        repo.add(frag).await.unwrap();
        let m1 = item_of(&p.system, "memory_fragments", "m1").await;
        assert!(p.index.get(&m1).await.unwrap().is_none());

        let loading =
            SqliteMemoryRepository::new(p.system.clone()).with_vector_index(p.index.clone(), None);
        let mut frag = MemoryFragment::from_chat("m2".into(), None, None, "y".into());
        frag.embedding = Some(vec![1.0, 0.0, 0.0]);
        loading.add(frag).await.unwrap();
        let m2 = item_of(&p.system, "memory_fragments", "m2").await;
        assert!(p.index.get(&m2).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn memory_search_reads_only_memory_kinds() {
        let p = pond().await;
        let m1 = add_memory(&p.system, "m1", None, "x").await;
        add_session(&p.system, "s1", Some("a summary"), "2026-11-01").await;
        sync_all(&p.system).await;
        let s1 = item_of(&p.system, "sessions", "s1").await;
        index_whole(&p.index, &m1, &[1.0, 0.0]).await;
        index_whole(&p.index, &s1, &[1.0, 0.0]).await;
        let hits = p
            .index
            .search(&[1.0, 0.0], "m", &ProfileScope::Household, &MEMORY, 10)
            .await
            .unwrap();
        assert_eq!(
            hits.iter().map(|h| h.item_id.clone()).collect::<Vec<_>>(),
            vec![m1]
        );
        let all = p.search(&[1.0, 0.0], "m", &ProfileScope::Household).await;
        assert_eq!(all.len(), 2);
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use pond_core::context::vector_index::{VectorEntry, VectorIndex};
    use pond_core::models::ports::embedding::EmbeddingProvider;
    use sqlx::{Pool, Sqlite};
    use std::sync::Arc;

    use crate::sqlite_items::{self, Mode};

    /// Names the model a repository stamps on the vectors it stores.
    pub fn stamp(model_id: &str, dims: usize) -> Option<Arc<dyn EmbeddingProvider>> {
        struct Stamp(String, usize);
        #[async_trait::async_trait]
        impl EmbeddingProvider for Stamp {
            async fn embed(&self, _: &str) -> anyhow::Result<Vec<f32>> {
                Ok(vec![0.0; self.1])
            }
            fn dimensions(&self) -> usize {
                self.1
            }
            fn model_id(&self) -> String {
                self.0.clone()
            }
        }
        Some(Arc::new(Stamp(model_id.to_string(), dims)))
    }

    pub async fn sync_all(pool: &Pool<Sqlite>) {
        sqlite_items::reconcile(pool, Mode::All).await.unwrap();
    }

    /// The item id of a detail row.
    pub async fn item_of(pool: &Pool<Sqlite>, table: &str, key: &str) -> String {
        sqlite_items::header_of(pool, table, key)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{table} {key} has no header"))
            .0
    }

    pub async fn rev_of(pool: &Pool<Sqlite>, item_id: &str) -> String {
        sqlx::query_scalar("SELECT updated_at FROM items WHERE id = ?")
            .bind(item_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

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

    /// A memory row with its header, as an older pond's backfill would leave it.
    pub async fn add_memory(
        pool: &Pool<Sqlite>,
        id: &str,
        owner: Option<&str>,
        content: &str,
    ) -> String {
        sqlx::query(
            "INSERT INTO memory_fragments (id, profile_id, content, source, tags, created_at, \
             access_count, lifecycle) VALUES (?, ?, ?, 'chat', '[]', datetime('now'), 0, 'active')",
        )
        .bind(id)
        .bind(owner)
        .bind(content)
        .execute(pool)
        .await
        .unwrap();
        sync_all(pool).await;
        item_of(pool, "memory_fragments", id).await
    }

    /// Indexes an item's whole-text vector under model `m`, current for its header.
    pub async fn index_whole(index: &Arc<dyn VectorIndex>, item_id: &str, v: &[f32]) {
        index
            .upsert(&VectorEntry::whole(
                item_id.into(),
                "m".into(),
                v.to_vec(),
                None,
            ))
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
