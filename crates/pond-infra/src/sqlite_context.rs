//! SQLite-backed [`ContextRepository`] — PAI-8 P1.
//!
//! Tables `context_sources` (migration 0044) and `context_items` (0045).
//!
//! # Why this adapter holds a redactor
//!
//! Because [`ContextItem`] has exactly one constructor and it takes one. That is
//! PAI-8 invariant 3 expressed as a type rather than as a decorator: there is no
//! way to turn a stored row into a `ContextItem` without running the redaction
//! pass over it. For a row written by the pipeline that is a no-op —
//! [`Redactor::redact`] is contractually idempotent — and for a row that reached
//! the table some other way it is a repair.
//!
//! It also means the wiring cannot forget. `SqliteMemoryRepository::new` takes a
//! pool, and the redaction only happens because `run_server` remembers to wrap
//! it in `RedactingMemoryRepository`; this one does not construct at all without
//! the thing that protects it.
//!
//! # Rows that will not load
//!
//! An unparseable timestamp, an unknown source kind, an item whose owner is
//! blank: all become "not a row", not a partial one. There is no
//! trust-the-database path, for the same reason `sqlite_proposal.rs` has none —
//! on failure, access narrows.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use pond_core::context::domain::{
    ContextItem, ContextSource, ItemKind, ItemParts, SourceKind, SourceParts, SourceStatus,
};
use pond_core::context::ports::{ContextRepository, SourceItemStats};
use pond_core::context::retention::ContextRetention;
use pond_core::context::vector_index::{Corpus, VectorEntry, VectorIndex};
use pond_core::security::domain::event::PrivacySensitivity;
use pond_core::security::ports::redactor::Redactor;
use pond_core::user_data::domain::profile::ProfileScope;
use sqlx::{Pool, Sqlite};

/// The columns a source is rebuilt from. Stated once so a column added to one
/// query and not another shifts a tuple field at compile time rather than at
/// runtime — the same reason `sqlite_draft.rs` has `DRAFT_COLUMNS`.
const SOURCE_COLUMNS: &str =
    "id, kind, provider, profile_id, scopes, cursor, last_sync, status, secret_ref, created_at";

type SourceRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    String,
);

const ITEM_COLUMNS: &str = "id, source_id, external_id, profile_id, source_kind, item_kind, \
     occurred_at, ingested_at, title, body, participants, sensitivity, embedding";

type ItemRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<Vec<u8>>,
);

pub struct SqliteContextRepository {
    pool: Pool<Sqlite>,
    redactor: Arc<dyn Redactor>,
    /// Shared personal-context index (phase B). Optional; without it items are
    /// stored exactly as before and the sweep picks them up later.
    index: Option<Arc<dyn VectorIndex>>,
    model_id: Option<String>,
}

impl SqliteContextRepository {
    /// The redactor is a constructor parameter, not a setter: see the module
    /// docs. There is deliberately no `new(pool)`.
    pub fn new(pool: Pool<Sqlite>, redactor: Arc<dyn Redactor>) -> Self {
        Self {
            pool,
            redactor,
            index: None,
            model_id: None,
        }
    }

    /// Mirror stored vectors into the shared index.
    ///
    /// Placed at the adapter for the same reason as the memory side: this is the
    /// terminal write and nothing else issues SQL against `context_items`. It is
    /// simpler here than for memory, because a `ContextItem` cannot exist
    /// un-redacted — `from_parts` is the only constructor and it takes the
    /// redactor — so whatever vector the item carries is already of redacted
    /// text by construction.
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
    async fn mirror(&self, item: &ContextItem) {
        let Some(index) = &self.index else { return };
        let outcome = match (item.embedding(), self.model_id.as_deref()) {
            (Some(vector), Some(model_id)) if !vector.is_empty() => {
                index
                    .upsert(&VectorEntry {
                        corpus: Corpus::Context,
                        row_id: item.id().to_string(),
                        // Chunk 0 of the whole text. The pipeline computes one
                        // vector over `embedding_text` and mirrors it here; the
                        // maintenance sweep is what replaces it with a full set
                        // of passages, and it clears this one first so the two
                        // never co-exist as rival descriptions of one row.
                        chunk_ix: 0,
                        chunk_span: None,
                        model_id: model_id.to_string(),
                        vector: vector.to_vec(),
                        // A re-sync REWRITES the row in place (upsert on
                        // `source_id, external_id`), so the ingest timestamp is
                        // what tells a sweep the stored vector is of older text.
                        source_rev: Some(sql_ts(item.ingested_at())),
                    })
                    .await
            }
            // A vector we cannot attribute: leave it to the sweep rather than
            // strip an entry another process wrote correctly.
            (Some(vector), None) if !vector.is_empty() => return,
            // An upsert with no embedder wired NULLs a previously stored vector,
            // so the index must follow rather than keep an entry for a row that
            // no longer has one.
            _ => index.remove(Corpus::Context, item.id()).await,
        };
        if let Err(e) = outcome {
            tracing::warn!(item_id = %item.id(), "vector index write failed: {e:#}");
        }
    }
}

// ── Encoding ────────────────────────────────────────────────────────────────

/// Seconds precision, UTC, `Z`-suffixed — the format `sqlite_proposal.rs` pins
/// for the same reason: these values are compared in SQL, and a format
/// `datetime()` cannot parse would change what the comparison means.
fn sql_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_ts(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("unreadable context timestamp: {raw}"))
}

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn sensitivity_str(s: PrivacySensitivity) -> &'static str {
    match s {
        PrivacySensitivity::Public => "public",
        PrivacySensitivity::Internal => "internal",
        PrivacySensitivity::Sensitive => "sensitive",
        PrivacySensitivity::Secret => "secret",
    }
}

/// Read a stored classification. Anything unrecognised reads as `Secret` — the
/// most restrictive answer — because an unreadable classification must not be
/// the permissive one.
fn parse_sensitivity(raw: &str) -> PrivacySensitivity {
    match raw {
        "public" => PrivacySensitivity::Public,
        "internal" => PrivacySensitivity::Internal,
        "sensitive" => PrivacySensitivity::Sensitive,
        _ => PrivacySensitivity::Secret,
    }
}

fn json_list(values: &[String]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
}

fn parse_json_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

/// SQL predicate and optional bind for a [`ProfileScope`].
///
/// **This mirrors `pond_core::context::scope::owner_is_visible` and must keep
/// mirroring it.** Two implementations of one boundary rule is how a boundary
/// rule drifts, so `the_sql_scope_filter_agrees_with_the_domain_predicate` runs
/// both over the same fixture and fails on any disagreement.
///
/// Note what is NOT here: `sqlite_memory`'s `OR profile_id IS NULL` limb.
/// `context_items.profile_id` is `NOT NULL`, so nothing could match it, and
/// adding it would create a hiding place for a row that belongs to nobody.
fn scope_sql(scope: &ProfileScope) -> (&'static str, Option<&str>) {
    match scope {
        ProfileScope::Owner(id) => ("AND profile_id = ?", Some(id.as_str())),
        ProfileScope::Household => ("", None),
        // Callers short-circuit before running the query, but the predicate is
        // correct on its own so a missed short-circuit fails closed.
        ProfileScope::Guest => ("AND 1 = 0", None),
    }
}

// ── Row mapping ─────────────────────────────────────────────────────────────

fn row_to_source(row: SourceRow) -> Result<ContextSource> {
    let kind = SourceKind::parse(&row.1)
        .with_context(|| format!("unknown context source kind: {}", row.1))?;
    let last_sync = match row.6.as_deref() {
        Some(raw) => Some(parse_ts(raw)?),
        None => None,
    };
    ContextSource::from_parts(SourceParts {
        id: row.0,
        kind,
        provider: row.2,
        profile_id: row.3,
        scopes: parse_json_list(&row.4),
        cursor: row.5,
        last_sync,
        status: SourceStatus::parse(&row.7),
        secret_ref: row.8,
        created_at: parse_ts(&row.9)?,
    })
    .map_err(anyhow::Error::from)
}

impl SqliteContextRepository {
    fn row_to_item(&self, row: ItemRow) -> Result<ContextItem> {
        let source_kind = SourceKind::parse(&row.4)
            .with_context(|| format!("unknown context source kind: {}", row.4))?;
        let kind = ItemKind::parse(&row.5)
            .with_context(|| format!("unknown context item kind: {}", row.5))?;
        ContextItem::from_parts(
            self.redactor.as_ref(),
            ItemParts {
                id: row.0,
                source_id: row.1,
                external_id: row.2,
                profile_id: row.3,
                source_kind,
                kind,
                occurred_at: parse_ts(&row.6)?,
                ingested_at: parse_ts(&row.7)?,
                title: row.8,
                body: row.9,
                participants: parse_json_list(&row.10),
                stored_sensitivity: Some(parse_sensitivity(&row.11)),
                embedding: row.12.as_deref().map(blob_to_vec),
            },
        )
        .map_err(anyhow::Error::from)
    }

    /// Map rows, dropping any that will not load and saying so.
    ///
    /// A row that cannot be rebuilt is not returned as a partial item: every
    /// caller here turns a broken row into "no item", which narrows.
    fn rows_to_items(&self, rows: Vec<ItemRow>) -> Vec<ContextItem> {
        rows.into_iter()
            .filter_map(|row| match self.row_to_item(row) {
                Ok(item) => Some(item),
                Err(e) => {
                    tracing::warn!(error = %e, "skipping an unloadable context item row");
                    None
                }
            })
            .collect()
    }
}

#[async_trait]
impl ContextRepository for SqliteContextRepository {
    async fn upsert_source(&self, source: &ContextSource) -> Result<()> {
        // `kind` and `profile_id` are absent from the DO UPDATE list on purpose:
        // they are the source's identity and 0044 refuses to change either.
        sqlx::query(
            "INSERT INTO context_sources \
             (id, kind, provider, profile_id, scopes, cursor, last_sync, status, secret_ref, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
               provider = excluded.provider, \
               scopes = excluded.scopes, \
               cursor = excluded.cursor, \
               last_sync = excluded.last_sync, \
               status = excluded.status, \
               secret_ref = excluded.secret_ref",
        )
        .bind(source.id())
        .bind(source.kind().as_str())
        .bind(source.provider())
        .bind(source.profile_id())
        .bind(json_list(source.scopes()))
        .bind(source.cursor())
        .bind(source.last_sync().map(sql_ts))
        .bind(source.status().as_str())
        .bind(source.secret_ref())
        .bind(sql_ts(source.created_at()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_source(&self, id: &str, scope: &ProfileScope) -> Result<Option<ContextSource>> {
        if scope.excludes_everything() {
            return Ok(None);
        }
        let (filter, bind) = scope_sql(scope);
        let query =
            format!("SELECT {SOURCE_COLUMNS} FROM context_sources WHERE id = ? {filter} LIMIT 1");
        let mut q = sqlx::query_as::<_, SourceRow>(&query).bind(id);
        if let Some(pid) = bind {
            q = q.bind(pid);
        }
        match q.fetch_optional(&self.pool).await? {
            Some(row) => Ok(Some(row_to_source(row)?)),
            None => Ok(None),
        }
    }

    async fn list_sources(&self, scope: &ProfileScope) -> Result<Vec<ContextSource>> {
        if scope.excludes_everything() {
            return Ok(vec![]);
        }
        let (filter, bind) = scope_sql(scope);
        let query = format!(
            "SELECT {SOURCE_COLUMNS} FROM context_sources WHERE 1 = 1 {filter} ORDER BY created_at DESC"
        );
        let mut q = sqlx::query_as::<_, SourceRow>(&query);
        if let Some(pid) = bind {
            q = q.bind(pid);
        }
        Ok(q.fetch_all(&self.pool)
            .await?
            .into_iter()
            .filter_map(|row| match row_to_source(row) {
                Ok(source) => Some(source),
                Err(e) => {
                    tracing::warn!(error = %e, "skipping an unloadable context source row");
                    None
                }
            })
            .collect())
    }

    async fn disconnect_source(&self, id: &str, scope: &ProfileScope) -> Result<u64> {
        // Reported before the delete, because after it there is nothing to
        // count and PAI-8 invariant 6 is "deletes its items **and says how
        // many**". Counted through the scoped read so a caller who may not see
        // the source is told about zero items rather than about somebody
        // else's.
        if self.get_source(id, scope).await?.is_none() {
            return Ok(0);
        }
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM context_items WHERE source_id = ?")
                .bind(id)
                .fetch_one(&self.pool)
                .await?;

        // The items go with an explicit DELETE rather than by relying on the
        // foreign key's ON DELETE CASCADE: `PRAGMA foreign_keys` is per
        // connection, and a cascade that silently does not happen would leave
        // orphaned personal data behind while reporting that it was deleted.
        // Collect the ids BEFORE deleting, so their vectors can follow. An
        // orphaned vector is harmless -- the index holds no text and the JOIN
        // drops it -- and the maintenance sweep would prune it eventually. But a
        // member who disconnects a source is asking for their data to be gone,
        // and "eventually, at the next restart" is a poor answer to that.
        let doomed: Vec<(String,)> =
            sqlx::query_as("SELECT id FROM context_items WHERE source_id = ?")
                .bind(id)
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default();

        sqlx::query("DELETE FROM context_items WHERE source_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;

        if let Some(index) = &self.index {
            for (item_id,) in &doomed {
                if let Err(e) = index.remove(Corpus::Context, item_id).await {
                    // Best effort: the sweep is the backstop, and failing the
                    // disconnect would leave the member with neither.
                    tracing::warn!(item_id = %item_id, "index cleanup on disconnect failed: {e:#}");
                }
            }
        }
        sqlx::query("DELETE FROM context_sources WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(count.max(0) as u64)
    }

    async fn save_item(&self, item: &ContextItem) -> Result<()> {
        sqlx::query(
            "INSERT INTO context_items \
             (id, source_id, external_id, profile_id, source_kind, item_kind, occurred_at, \
              ingested_at, title, body, participants, sensitivity, embedding) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(source_id, external_id) DO UPDATE SET \
               item_kind = excluded.item_kind, \
               occurred_at = excluded.occurred_at, \
               ingested_at = excluded.ingested_at, \
               title = excluded.title, \
               body = excluded.body, \
               participants = excluded.participants, \
               sensitivity = excluded.sensitivity, \
               embedding = excluded.embedding",
        )
        .bind(item.id())
        .bind(item.source_id())
        .bind(item.external_id())
        .bind(item.profile_id())
        .bind(item.source_kind().as_str())
        .bind(item.kind().as_str())
        .bind(sql_ts(item.occurred_at()))
        .bind(sql_ts(item.ingested_at()))
        .bind(item.title())
        .bind(item.body())
        .bind(json_list(item.participants()))
        .bind(sensitivity_str(item.sensitivity()))
        .bind(item.embedding().map(vec_to_blob))
        .execute(&self.pool)
        .await?;
        // Write-through, after the row is durable. Safe by construction here:
        // `ContextItem` has one constructor and it redacts, so the vector this
        // mirrors is already of redacted text.
        self.mirror(item).await;
        Ok(())
    }

    async fn recent_items(&self, scope: &ProfileScope, limit: usize) -> Result<Vec<ContextItem>> {
        if scope.excludes_everything() {
            return Ok(vec![]);
        }
        let (filter, bind) = scope_sql(scope);
        let query = format!(
            "SELECT {ITEM_COLUMNS} FROM context_items WHERE 1 = 1 {filter} \
             ORDER BY occurred_at DESC LIMIT ?"
        );
        let mut q = sqlx::query_as::<_, ItemRow>(&query);
        if let Some(pid) = bind {
            q = q.bind(pid);
        }
        let rows = q.bind(limit as i64).fetch_all(&self.pool).await?;
        Ok(self.rows_to_items(rows))
    }

    async fn search_items(
        &self,
        keywords: &[String],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<ContextItem>> {
        if keywords.is_empty() || scope.excludes_everything() {
            return Ok(vec![]);
        }
        let (filter, bind) = scope_sql(scope);
        let clauses: Vec<&str> = keywords
            .iter()
            .map(|_| "(title LIKE ? OR body LIKE ?)")
            .collect();
        let query = format!(
            "SELECT {ITEM_COLUMNS} FROM context_items WHERE ({}) {filter} \
             ORDER BY occurred_at DESC LIMIT ?",
            clauses.join(" OR ")
        );
        let mut q = sqlx::query_as::<_, ItemRow>(&query);
        for keyword in keywords {
            let pattern = format!("%{keyword}%");
            q = q.bind(pattern.clone()).bind(pattern);
        }
        if let Some(pid) = bind {
            q = q.bind(pid);
        }
        let rows = q.bind(limit as i64).fetch_all(&self.pool).await?;
        Ok(self.rows_to_items(rows))
    }

    async fn search_similar(
        &self,
        query_embedding: &[f32],
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<(ContextItem, f32)>> {
        if scope.excludes_everything() {
            return Ok(vec![]);
        }
        let (filter, bind) = scope_sql(scope);
        let query = format!(
            "SELECT {ITEM_COLUMNS} FROM context_items WHERE embedding IS NOT NULL {filter}"
        );
        let mut q = sqlx::query_as::<_, ItemRow>(&query);
        if let Some(pid) = bind {
            q = q.bind(pid);
        }
        let rows = q.fetch_all(&self.pool).await?;

        let candidates = rows.len();
        let mut scored: Vec<(ContextItem, f32)> = self
            .rows_to_items(rows)
            .into_iter()
            .filter_map(|item| {
                let emb = item.embedding()?;
                // Different width means a different embedding model, and `cosine`
                // answers 0.0 for that — a valid score. Scoring such an item would
                // fill the result with incomparable rows ranked as if judged, and
                // leave the caller's `is_empty()` keyword fallback unable to fire.
                if emb.len() != query_embedding.len() {
                    return None;
                }
                let score = cosine(query_embedding, emb);
                Some((item, score))
            })
            .collect();
        if scored.len() < candidates {
            tracing::warn!(
                incomparable = candidates - scored.len(),
                comparable = scored.len(),
                "some context items were embedded by a different model and were \
                 excluded from semantic search"
            );
        }
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.id().cmp(b.0.id()))
        });
        scored.truncate(limit);
        Ok(scored)
    }

    async fn search_unembedded(&self, limit: usize) -> Result<Vec<ContextItem>> {
        let query = format!(
            "SELECT {ITEM_COLUMNS} FROM context_items WHERE embedding IS NULL \
             ORDER BY ingested_at DESC LIMIT ?"
        );
        let rows = sqlx::query_as::<_, ItemRow>(&query)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        Ok(self.rows_to_items(rows))
    }

    async fn update_embedding(&self, id: &str, embedding: &[f32]) -> Result<()> {
        sqlx::query("UPDATE context_items SET embedding = ? WHERE id = ?")
            .bind(vec_to_blob(embedding))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn count_for_profile(&self, profile_id: &str) -> Result<u64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM context_items WHERE profile_id = ?")
                .bind(profile_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(count.max(0) as u64)
    }

    async fn count_in_window(
        &self,
        scope: &ProfileScope,
        kind: SourceKind,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<u64> {
        // The guest check comes first and returns zero rather than running a
        // query that would also return zero. PAI-8 invariant 2 is that a
        // `Guest` sees no context items, and the cheapest way to honour it is
        // not to ask.
        if scope.excludes_everything() {
            return Ok(0);
        }
        let (filter, bind) = scope_sql(scope);
        // Half-open [from, to): a midnight boundary belongs to the day it
        // opens, and a closed range would count an event at exactly midnight
        // on both of the days either side of it.
        let query = format!(
            "SELECT COUNT(*) FROM context_items \
             WHERE source_kind = ? AND occurred_at >= ? AND occurred_at < ? {filter}"
        );
        // `sql_ts`, the same formatter every row is written with, so the bound
        // and the column are the same shape of string -- these are compared as
        // TEXT, not as instants.
        //
        // Worth recording what this is NOT protecting against, because the
        // obvious worry turns out to be unfounded and the next reader will have
        // it too. Rows read "...:00Z" and `to_rfc3339()` would bind
        // "...:00+00:00"; 'Z' is 0x5A and '+' is 0x2B, so an exactly-equal
        // instant sorts ABOVE the bound -- which is what `>= from` (include)
        // and `< to` (exclude) each already want. Mutating this back to
        // `to_rfc3339()` leaves all four tests below green, checked. The reason
        // to use `sql_ts` anyway is that the agreement is a coincidence of two
        // formats and one comparison direction: change `sql_ts` to emit offsets,
        // or make either bound inclusive, and it stops holding silently.
        let mut q = sqlx::query_scalar::<_, i64>(&query)
            .bind(kind.as_str())
            .bind(sql_ts(from))
            .bind(sql_ts(to));
        if let Some(pid) = bind {
            q = q.bind(pid);
        }
        let count = q.fetch_one(&self.pool).await?;
        Ok(count.max(0) as u64)
    }

    async fn item_stats_by_source(&self) -> Result<Vec<SourceItemStats>> {
        // GROUP BY, not a query per source. The screen that reads this already
        // has the source list, so the alternative is N+1 round trips to answer
        // one sentence -- a cost that only bites the pond with the most data.
        let rows: Vec<(String, i64, i64)> = sqlx::query_as(
            "SELECT source_id, \
                    COUNT(*), \
                    SUM(CASE WHEN embedding IS NULL THEN 1 ELSE 0 END) \
             FROM context_items \
             GROUP BY source_id",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(source_id, items, awaiting)| SourceItemStats {
                source_id,
                items: items.max(0) as u64,
                awaiting_index: awaiting.max(0) as u64,
            })
            .collect())
    }

    async fn purge_expired(&self, retention: &ContextRetention, now: DateTime<Utc>) -> Result<u64> {
        let mut deleted = 0u64;
        for bucket in retention.sweep_plan(now) {
            let Some(cutoff) = bucket.cutoff else {
                continue;
            };
            // The sensitivity class is spelled out as a set rather than as an
            // ordering, because these are strings in SQLite and 'internal' <
            // 'sensitive' < 'secret' is true only by accident of the alphabet.
            let sensitivities: &[&str] = if bucket.sensitive {
                &["sensitive", "secret"]
            } else {
                &["public", "internal"]
            };
            let placeholders = vec!["?"; sensitivities.len()].join(", ");
            let query = format!(
                "DELETE FROM context_items WHERE source_kind = ? \
                 AND sensitivity IN ({placeholders}) AND datetime(occurred_at) < datetime(?)"
            );
            let mut q = sqlx::query(&query).bind(bucket.kind.as_str());
            for s in sensitivities {
                q = q.bind(*s);
            }
            let result = q.bind(sql_ts(cutoff)).execute(&self.pool).await?;
            deleted += result.rows_affected();
        }
        Ok(deleted)
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::rule_redactor::RuleRedactor;
    use chrono::Duration;
    use pond_core::context::ingest::{IngestPipeline, RawItem};
    use pond_core::context::scope::owner_is_visible;
    use pond_core::user_data::domain::profile::{EXEMPLAR_OWNER_ID, SECOND_EXEMPLAR_OWNER_ID};
    use std::collections::HashMap;
    use tempfile::TempDir;

    const KEY: &str = "sk-abcdefghijklmnopqrstuvwxyz123456";

    async fn make_repo() -> (SqliteContextRepository, Pool<Sqlite>, TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let repo = SqliteContextRepository::new(db.system.clone(), Arc::new(RuleRedactor::new()));
        (repo, db.system, tmp)
    }

    /// The FK on `context_sources.profile_id` is real and `PRAGMA foreign_keys`
    /// is on, so a member has to exist before a source can point at them.
    async fn add_member(pool: &Pool<Sqlite>, id: &str) {
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
            .bind(id)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    fn source(id: &str, owner: &str, kind: SourceKind) -> ContextSource {
        ContextSource::from_parts(SourceParts {
            id: id.into(),
            kind,
            provider: "pond".into(),
            profile_id: owner.into(),
            scopes: vec!["read".into()],
            cursor: None,
            last_sync: None,
            status: SourceStatus::Connected,
            secret_ref: None,
            created_at: Utc::now(),
        })
        .expect("valid source")
    }

    fn raw(external_id: &str, body: &str, occurred_at: DateTime<Utc>) -> RawItem {
        RawItem {
            external_id: external_id.into(),
            kind: ItemKind::Message,
            occurred_at,
            title: "a subject".into(),
            body: body.into(),
            participants: vec![],
        }
    }

    async fn ingest(
        repo: &SqliteContextRepository,
        src: &ContextSource,
        raw_item: RawItem,
    ) -> Result<()> {
        let pipeline = IngestPipeline::new(
            Arc::new(SqliteContextRepository::new(
                repo.pool.clone(),
                repo.redactor.clone(),
            )),
            repo.redactor.clone(),
        );
        pipeline
            .ingest(src, raw_item, Utc::now())
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    #[tokio::test]
    async fn a_credential_never_reaches_the_column() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, "jerry").await;
        let src = source("s1", "jerry", SourceKind::Voice);
        repo.upsert_source(&src).await.unwrap();
        ingest(
            &repo,
            &src,
            raw("e1", &format!("the key is {KEY} ok"), Utc::now()),
        )
        .await
        .unwrap();

        // Read the raw column, not the mapped value: the mapper re-redacts, so
        // asserting on it would pass even if the write path had stored the
        // secret.
        let stored: String = sqlx::query_scalar("SELECT body FROM context_items")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!stored.contains(KEY), "the raw body is in SQLite: {stored}");
        assert!(stored.contains("[redacted:api-key]"));
    }

    /// The read-path repair, driven through the database. A row written by hand
    /// with a credential in it does not come back out with the credential.
    #[tokio::test]
    async fn a_row_written_around_the_pipeline_is_repaired_on_read() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, "jerry").await;
        repo.upsert_source(&source("s1", "jerry", SourceKind::Voice))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO context_items (id, source_id, external_id, profile_id, source_kind, \
             item_kind, occurred_at, ingested_at, title, body, participants, sensitivity) \
             VALUES ('i1', 's1', 'e1', 'jerry', 'voice', 'message', ?, ?, 'subject', ?, '[]', 'public')",
        )
        .bind(sql_ts(Utc::now()))
        .bind(sql_ts(Utc::now()))
        .bind(format!("smuggled {KEY} in"))
        .execute(&pool)
        .await
        .unwrap();

        let items = repo
            .recent_items(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert!(!items[0].body().contains(KEY), "{}", items[0].body());
        assert_ne!(
            items[0].sensitivity(),
            PrivacySensitivity::Public,
            "a 'public' written into the column must lose to the derived classification"
        );
    }

    #[tokio::test]
    async fn a_guest_sees_no_item_and_no_source() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, "jerry").await;
        let src = source("s1", "jerry", SourceKind::Voice);
        repo.upsert_source(&src).await.unwrap();
        ingest(&repo, &src, raw("e1", "something private", Utc::now()))
            .await
            .unwrap();

        assert!(repo
            .recent_items(&ProfileScope::Guest, 10)
            .await
            .unwrap()
            .is_empty());
        assert!(repo
            .search_items(&["something".into()], &ProfileScope::Guest, 10)
            .await
            .unwrap()
            .is_empty());
        assert!(repo
            .search_similar(&[1.0, 0.0], &ProfileScope::Guest, 10)
            .await
            .unwrap()
            .is_empty());
        assert!(repo
            .list_sources(&ProfileScope::Guest)
            .await
            .unwrap()
            .is_empty());
        assert!(repo
            .get_source("s1", &ProfileScope::Guest)
            .await
            .unwrap()
            .is_none());

        // Vacuity control: the same reads under Household return the rows, so
        // the assertions above are about the scope and not about an empty
        // store.
        assert_eq!(
            repo.recent_items(&ProfileScope::Household, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            repo.list_sources(&ProfileScope::Household)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn one_member_cannot_read_another_members_items() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, EXEMPLAR_OWNER_ID).await;
        add_member(&pool, SECOND_EXEMPLAR_OWNER_ID).await;
        let mine = source("s1", EXEMPLAR_OWNER_ID, SourceKind::Voice);
        let theirs = source("s2", SECOND_EXEMPLAR_OWNER_ID, SourceKind::Voice);
        repo.upsert_source(&mine).await.unwrap();
        repo.upsert_source(&theirs).await.unwrap();
        ingest(&repo, &mine, raw("e1", "my dentist", Utc::now()))
            .await
            .unwrap();
        ingest(&repo, &theirs, raw("e2", "their dentist", Utc::now()))
            .await
            .unwrap();

        let scope = ProfileScope::Owner(EXEMPLAR_OWNER_ID.to_string());
        let recent = repo.recent_items(&scope, 10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].profile_id(), EXEMPLAR_OWNER_ID);

        let searched = repo
            .search_items(&["dentist".into()], &scope, 10)
            .await
            .unwrap();
        assert_eq!(searched.len(), 1, "keyword search is not scoped");
        assert_eq!(searched[0].profile_id(), EXEMPLAR_OWNER_ID);

        assert_eq!(repo.list_sources(&scope).await.unwrap().len(), 1);
        assert!(
            repo.get_source("s2", &scope).await.unwrap().is_none(),
            "one member read another member's source"
        );
        assert_eq!(
            repo.recent_items(&ProfileScope::Household, 10)
                .await
                .unwrap()
                .len(),
            2,
            "the household should still see both -- otherwise the test above proves nothing"
        );
    }

    /// The SQL filter and the domain predicate are two implementations of one
    /// rule. Run both over the same fixture and fail on any disagreement.
    #[tokio::test]
    async fn the_sql_scope_filter_agrees_with_the_domain_predicate() {
        let (repo, pool, _tmp) = make_repo().await;
        let owners = [EXEMPLAR_OWNER_ID, SECOND_EXEMPLAR_OWNER_ID];
        for (n, owner) in owners.iter().enumerate() {
            add_member(&pool, owner).await;
            let src = source(&format!("s{n}"), owner, SourceKind::Voice);
            repo.upsert_source(&src).await.unwrap();
            ingest(&repo, &src, raw(&format!("e{n}"), "hello", Utc::now()))
                .await
                .unwrap();
        }

        let mut scopes = ProfileScope::every_shape();
        scopes.push(ProfileScope::Owner(SECOND_EXEMPLAR_OWNER_ID.to_string()));

        let mut visible_total = 0usize;
        for scope in &scopes {
            let from_sql: Vec<String> = repo
                .recent_items(scope, 100)
                .await
                .unwrap()
                .into_iter()
                .map(|i| i.profile_id().to_string())
                .collect();
            let from_domain: Vec<String> = owners
                .iter()
                .filter(|o| owner_is_visible(o, scope))
                .map(|o| o.to_string())
                .collect();
            visible_total += from_domain.len();
            let mut a = from_sql.clone();
            let mut b = from_domain.clone();
            a.sort();
            b.sort();
            assert_eq!(
                a, b,
                "SQL and the domain predicate disagree for {scope:?}: SQL {from_sql:?}, domain \
                 {from_domain:?}"
            );
        }
        // Vacuity control: `every_shape` includes an Owner whose id matches no
        // fixture row, so without the second owner pushed above this loop would
        // compare four empty lists with four empty lists.
        assert_eq!(
            visible_total, 4,
            "the fixture stopped exercising the rule: 2 for Household, 1 for each real Owner, \
             0 for Guest and 0 for the exemplar owner with no rows"
        );
    }

    #[tokio::test]
    async fn re_ingesting_updates_the_same_row() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, "jerry").await;
        let src = source("s1", "jerry", SourceKind::Voice);
        repo.upsert_source(&src).await.unwrap();
        let when = Utc::now();
        ingest(&repo, &src, raw("e1", "first", when)).await.unwrap();
        ingest(&repo, &src, raw("e1", "corrected", when))
            .await
            .unwrap();

        let items = repo
            .recent_items(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(items.len(), 1, "a re-sync duplicated the row");
        assert_eq!(items[0].body(), "corrected");
    }

    #[tokio::test]
    async fn disconnecting_deletes_the_items_and_reports_how_many() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, "jerry").await;
        let src = source("s1", "jerry", SourceKind::Voice);
        repo.upsert_source(&src).await.unwrap();
        for n in 0..3 {
            ingest(&repo, &src, raw(&format!("e{n}"), "hello", Utc::now()))
                .await
                .unwrap();
        }

        let deleted = repo
            .disconnect_source("s1", &ProfileScope::Household)
            .await
            .unwrap();
        assert_eq!(deleted, 3, "the reported count must be the real one");
        assert_eq!(repo.count_for_profile("jerry").await.unwrap(), 0);
        assert!(repo
            .list_sources(&ProfileScope::Household)
            .await
            .unwrap()
            .is_empty());

        // A second disconnect reports zero rather than failing, and deletes
        // nothing that is not there.
        assert_eq!(
            repo.disconnect_source("s1", &ProfileScope::Household)
                .await
                .unwrap(),
            0
        );
    }

    /// A member who may not see the source may not delete its items either, and
    /// is told about zero rather than about somebody else's data.
    #[tokio::test]
    async fn a_stranger_cannot_disconnect_someone_elses_source() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, EXEMPLAR_OWNER_ID).await;
        add_member(&pool, SECOND_EXEMPLAR_OWNER_ID).await;
        let theirs = source("s1", SECOND_EXEMPLAR_OWNER_ID, SourceKind::Voice);
        repo.upsert_source(&theirs).await.unwrap();
        ingest(&repo, &theirs, raw("e1", "private", Utc::now()))
            .await
            .unwrap();

        let deleted = repo
            .disconnect_source("s1", &ProfileScope::Owner(EXEMPLAR_OWNER_ID.into()))
            .await
            .unwrap();
        assert_eq!(deleted, 0);
        assert_eq!(
            repo.count_for_profile(SECOND_EXEMPLAR_OWNER_ID)
                .await
                .unwrap(),
            1,
            "a stranger's disconnect deleted the owner's items"
        );
    }

    /// Deleting a household member takes their sources and items with them.
    #[tokio::test]
    async fn removing_a_member_removes_their_context() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, "jerry").await;
        let src = source("s1", "jerry", SourceKind::Voice);
        repo.upsert_source(&src).await.unwrap();
        ingest(&repo, &src, raw("e1", "private", Utc::now()))
            .await
            .unwrap();

        sqlx::query("DELETE FROM profiles WHERE id = 'jerry'")
            .execute(&pool)
            .await
            .unwrap();

        assert!(repo
            .list_sources(&ProfileScope::Household)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            repo.recent_items(&ProfileScope::Household, 10)
                .await
                .unwrap()
                .len(),
            0,
            "a removed member's items outlived them"
        );
    }

    #[tokio::test]
    async fn a_source_cannot_change_owner_or_kind() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, EXEMPLAR_OWNER_ID).await;
        add_member(&pool, SECOND_EXEMPLAR_OWNER_ID).await;
        repo.upsert_source(&source("s1", EXEMPLAR_OWNER_ID, SourceKind::Voice))
            .await
            .unwrap();

        let moved = sqlx::query("UPDATE context_sources SET profile_id = ? WHERE id = 's1'")
            .bind(SECOND_EXEMPLAR_OWNER_ID)
            .execute(&pool)
            .await;
        assert!(moved.is_err(), "a source changed owner under its items");

        let rekinded = sqlx::query("UPDATE context_sources SET kind = 'mail' WHERE id = 's1'")
            .execute(&pool)
            .await;
        assert!(rekinded.is_err(), "a source changed kind under its items");

        // The upsert path takes the same route and must not move either.
        repo.upsert_source(&source("s1", EXEMPLAR_OWNER_ID, SourceKind::Voice))
            .await
            .expect("re-upserting an unchanged source must work");
        let stored = repo
            .get_source("s1", &ProfileScope::Household)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.profile_id(), EXEMPLAR_OWNER_ID);
        assert_eq!(stored.kind(), SourceKind::Voice);
    }

    #[tokio::test]
    async fn an_item_cannot_be_stored_under_a_different_owner_than_its_source() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, EXEMPLAR_OWNER_ID).await;
        add_member(&pool, SECOND_EXEMPLAR_OWNER_ID).await;
        repo.upsert_source(&source("s1", EXEMPLAR_OWNER_ID, SourceKind::Voice))
            .await
            .unwrap();

        let smuggled = sqlx::query(
            "INSERT INTO context_items (id, source_id, external_id, profile_id, source_kind, \
             item_kind, occurred_at, ingested_at, title, body, participants, sensitivity) \
             VALUES ('i1', 's1', 'e1', ?, 'voice', 'message', ?, ?, 't', 'b', '[]', 'sensitive')",
        )
        .bind(SECOND_EXEMPLAR_OWNER_ID)
        .bind(sql_ts(Utc::now()))
        .bind(sql_ts(Utc::now()))
        .execute(&pool)
        .await;
        assert!(
            smuggled.is_err(),
            "an item was stored under an owner its source does not have"
        );
    }

    #[tokio::test]
    async fn retention_deletes_the_expired_and_keeps_the_rest() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, "jerry").await;
        let src = source("s1", "jerry", SourceKind::Voice);
        repo.upsert_source(&src).await.unwrap();
        let now = Utc::now();
        ingest(
            &repo,
            &src,
            raw("old", "long ago", now - Duration::days(40)),
        )
        .await
        .unwrap();
        ingest(
            &repo,
            &src,
            raw("new", "yesterday", now - Duration::days(1)),
        )
        .await
        .unwrap();

        // Voice items are Sensitive, so the sensitive cap governs them.
        let retention = ContextRetention::new(HashMap::new(), 30, 7);
        let deleted = repo.purge_expired(&retention, now).await.unwrap();
        assert_eq!(deleted, 1);
        let left = repo
            .recent_items(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].external_id(), "new");

        // A retention of zero is "keep forever" and must delete nothing.
        let forever = ContextRetention::new(HashMap::new(), 0, 0);
        assert_eq!(repo.purge_expired(&forever, now).await.unwrap(), 0);
        assert_eq!(
            repo.recent_items(&ProfileScope::Household, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        let _ = pool;
    }

    /// PAI-8 invariant 4, asserted against the live schema rather than against
    /// the migration's comment. A column a token could be written to is a column
    /// a token eventually is written to.
    #[tokio::test]
    async fn the_schema_has_nowhere_to_put_a_token() {
        let (_repo, pool, _tmp) = make_repo().await;
        let columns: Vec<(i64, String, String, i64, Option<String>, i64)> =
            sqlx::query_as("SELECT * FROM pragma_table_info('context_sources')")
                .fetch_all(&pool)
                .await
                .unwrap();
        let names: Vec<String> = columns.iter().map(|c| c.1.to_lowercase()).collect();
        assert!(
            !names.is_empty(),
            "pragma_table_info returned nothing, so this guard is asserting about no table at all"
        );
        assert!(
            names.iter().any(|n| n == "secret_ref"),
            "the reference column is gone, so this guard is no longer looking at the table it \
             was written for: {names:?}"
        );
        for banned in [
            "token",
            "access_token",
            "refresh_token",
            "password",
            "secret",
        ] {
            assert!(
                !names.iter().any(|n| n == banned),
                "context_sources has a `{banned}` column; connector credentials belong in the \
                 encrypted secret store, and `secret_ref` is the key to look one up"
            );
        }
    }

    #[tokio::test]
    async fn semantic_search_ranks_by_cosine_and_the_backfill_finds_the_unembedded() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, "jerry").await;
        let src = source("s1", "jerry", SourceKind::Voice);
        repo.upsert_source(&src).await.unwrap();
        ingest(&repo, &src, raw("a", "one", Utc::now()))
            .await
            .unwrap();
        ingest(&repo, &src, raw("b", "two", Utc::now()))
            .await
            .unwrap();

        let unembedded = repo.search_unembedded(10).await.unwrap();
        assert_eq!(unembedded.len(), 2, "the backfill cannot see the new rows");

        repo.update_embedding("s1:a", &[1.0, 0.0]).await.unwrap();
        repo.update_embedding("s1:b", &[0.0, 1.0]).await.unwrap();
        assert_eq!(repo.search_unembedded(10).await.unwrap().len(), 0);

        let hits = repo
            .search_similar(&[1.0, 0.0], &ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0.external_id(), "a");
        assert!(
            hits[0].1 > hits[1].1,
            "results are not ranked by similarity"
        );
    }

    /// The window counts the rows inside it and not the ones outside.
    ///
    /// Four rows an hour apart, a window covering two of them. This is the
    /// plain correctness test; the boundary and scope cases are separate below.
    #[tokio::test]
    async fn the_window_counts_rows_in_the_format_they_are_stored_in() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, "jerry").await;
        let src = source("s1", "jerry", SourceKind::Voice);
        repo.upsert_source(&src).await.unwrap();

        let noon = "2026-09-15T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        for (i, offset) in [-2i64, -1, 0, 1].iter().enumerate() {
            ingest(
                &repo,
                &src,
                raw(
                    &format!("e{i}"),
                    "hello",
                    noon + chrono::Duration::hours(*offset),
                ),
            )
            .await
            .unwrap();
        }

        let from = "2026-09-15T10:30:00Z".parse::<DateTime<Utc>>().unwrap();
        let to = "2026-09-15T12:30:00Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(
            repo.count_in_window(&ProfileScope::Household, SourceKind::Voice, from, to)
                .await
                .unwrap(),
            2,
            "the window counted the wrong rows -- check the bound timestamp format"
        );
    }

    /// The range is half-open, so a midnight boundary belongs to one day only.
    #[tokio::test]
    async fn the_window_is_half_open_at_both_ends() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, "jerry").await;
        let src = source("s1", "jerry", SourceKind::Voice);
        repo.upsert_source(&src).await.unwrap();

        let from = "2026-09-15T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let to = "2026-09-16T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        ingest(&repo, &src, raw("open", "at the open", from))
            .await
            .unwrap();
        ingest(&repo, &src, raw("close", "at the close", to))
            .await
            .unwrap();

        assert_eq!(
            repo.count_in_window(&ProfileScope::Household, SourceKind::Voice, from, to)
                .await
                .unwrap(),
            1,
            "an event at exactly midnight was counted on both days, or on neither"
        );
    }

    /// A kind that is not asked for is not counted, or every suggestor would
    /// quote the whole corpus.
    #[tokio::test]
    async fn the_window_counts_one_source_kind_only() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, "jerry").await;
        let voice = source("s1", "jerry", SourceKind::Voice);
        let camera = source("s2", "jerry", SourceKind::Camera);
        repo.upsert_source(&voice).await.unwrap();
        repo.upsert_source(&camera).await.unwrap();

        let at = "2026-09-15T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        ingest(&repo, &voice, raw("v", "spoken", at)).await.unwrap();
        ingest(&repo, &camera, raw("c", "seen", at)).await.unwrap();

        let from = at - chrono::Duration::hours(1);
        let to = at + chrono::Duration::hours(1);
        assert_eq!(
            repo.count_in_window(&ProfileScope::Household, SourceKind::Voice, from, to)
                .await
                .unwrap(),
            1
        );
    }

    /// PAI-8 invariant 2: a guest sees no context items, counts included.
    #[tokio::test]
    async fn a_guest_counts_nothing() {
        let (repo, pool, _tmp) = make_repo().await;
        add_member(&pool, "jerry").await;
        let src = source("s1", "jerry", SourceKind::Voice);
        repo.upsert_source(&src).await.unwrap();
        let at = "2026-09-15T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        ingest(&repo, &src, raw("v", "spoken", at)).await.unwrap();

        // The control: the same window is non-zero for the household, so a zero
        // for the guest is the scope and not an empty table.
        let from = at - chrono::Duration::hours(1);
        let to = at + chrono::Duration::hours(1);
        assert_eq!(
            repo.count_in_window(&ProfileScope::Household, SourceKind::Voice, from, to)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            repo.count_in_window(&ProfileScope::Guest, SourceKind::Voice, from, to)
                .await
                .unwrap(),
            0
        );
    }

    /// Every install after the first is an upgrade: applying the migrations to a
    /// database that already has rows must work, and must not disturb them.
    #[tokio::test]
    async fn the_migrations_apply_to_a_database_that_already_has_rows() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let db = Database::init(tmp.path()).await.unwrap();
            add_member(&db.system, "jerry").await;
            let repo =
                SqliteContextRepository::new(db.system.clone(), Arc::new(RuleRedactor::new()));
            let src = source("s1", "jerry", SourceKind::Voice);
            repo.upsert_source(&src).await.unwrap();
            ingest(&repo, &src, raw("e1", "hello", Utc::now()))
                .await
                .unwrap();
        }
        // Second init over the same directory: sqlx re-runs its migrator against
        // a populated file.
        let db = Database::init(tmp.path()).await.unwrap();
        let repo = SqliteContextRepository::new(db.system.clone(), Arc::new(RuleRedactor::new()));
        assert_eq!(
            repo.recent_items(&ProfileScope::Household, 10)
                .await
                .unwrap()
                .len(),
            1,
            "the row did not survive a restart"
        );
    }
}
