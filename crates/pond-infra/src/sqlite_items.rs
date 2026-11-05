//! The only writer of `items` and `item_links`. A detail table's writer syncs its row's header
//! in the same transaction; [`reconcile`] writes the header of every row that has none, or
//! repairs every header, and changes nothing when run again.

use std::sync::OnceLock;

use anyhow::Result;
use pond_core::items::from_detail::{
    context_header, conversation_header, memory_header, suggestion_header, ContextRow,
    HeaderFields, MemoryRow, SessionRow, SuggestionRow,
};
use pond_core::items::{
    source, timestamp, Audience, ItemId, Kind, Modality, ProducedBy, Relation, Scope, State,
};
use pond_core::security::domain::event::PrivacySensitivity;
use pond_core::user_data::domain::profile::ProfileScope;
use sqlx::{Pool, Sqlite, SqliteConnection};

use crate::rule_redactor::RuleRedactor;

/// The rule redactor holds no state but compiled patterns, so one serves every header write.
fn redactor() -> &'static RuleRedactor {
    static REDACTOR: OnceLock<RuleRedactor> = OnceLock::new();
    REDACTOR.get_or_init(RuleRedactor::new)
}

/// What `scope` may read of `items` aliased `i`, as an `AND` clause and its binds: a member
/// sees their own, the household's and what is shared with them, the household everything,
/// a guest nothing. `schema` is `sys.` on the index's connections, which attach this database.
pub fn scope_sql(scope: &ProfileScope, schema: &str) -> (String, Vec<String>) {
    match scope {
        ProfileScope::Owner(id) => (
            format!(
                "AND (i.owner_profile_id = ? OR i.audience = 'household' OR EXISTS \
                 (SELECT 1 FROM {schema}item_shares s WHERE s.item_id = i.id AND s.profile_id = ?))"
            ),
            vec![id.clone(), id.clone()],
        ),
        ProfileScope::Household => (String::new(), vec![]),
        ProfileScope::Guest => ("AND 1 = 0".to_string(), vec![]),
    }
}

/// An item's id and revision, read through its detail row's key.
pub async fn header_of(
    pool: &Pool<Sqlite>,
    table: &str,
    key: &str,
) -> Result<Option<(String, String)>> {
    let sql = format!(
        "SELECT i.id, i.updated_at FROM {table} d JOIN items i ON i.id = d.item_id WHERE d.id = ?"
    );
    Ok(sqlx::query_as(&sql).bind(key).fetch_optional(pool).await?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Synced {
    Unchanged,
    Created,
    Updated,
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Rows with no header yet: cheap enough for every process that opens the database.
    Missing,
    /// Every row, repairing any header a writer left stale.
    All,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub created: u64,
    pub updated: u64,
    pub removed: u64,
    pub linked: u64,
}

impl ReconcileReport {
    pub fn changed(&self) -> bool {
        self.created + self.updated + self.removed + self.linked > 0
    }

    fn count(&mut self, synced: Synced) {
        match synced {
            Synced::Unchanged => {}
            Synced::Created => self.created += 1,
            Synced::Updated => self.updated += 1,
            Synced::Removed => self.removed += 1,
        }
    }
}

fn sensitivity_str(s: PrivacySensitivity) -> &'static str {
    match s {
        PrivacySensitivity::Public => "public",
        PrivacySensitivity::Internal => "internal",
        PrivacySensitivity::Sensitive => "sensitive",
        PrivacySensitivity::Secret => "secret",
    }
}

fn parse_sensitivity(raw: &str) -> Option<PrivacySensitivity> {
    Some(match raw {
        "public" => PrivacySensitivity::Public,
        "internal" => PrivacySensitivity::Internal,
        "sensitive" => PrivacySensitivity::Sensitive,
        "secret" => PrivacySensitivity::Secret,
        _ => return None,
    })
}

#[derive(sqlx::FromRow)]
struct StoredHeader {
    id: String,
    kind: String,
    source_id: String,
    external_id: String,
    owner_profile_id: Option<String>,
    audience: String,
    sensitivity: String,
    modality: String,
    observed_at: String,
    valid_until: Option<String>,
    title: String,
    body: String,
    context: String,
    state: String,
    produced_by: String,
    confidence: Option<f64>,
}

const HEADER_COLUMNS: &str = "id, kind, source_id, external_id, owner_profile_id, audience, \
     sensitivity, modality, observed_at, valid_until, title, body, context, state, produced_by, \
     confidence";

impl StoredHeader {
    /// `None` when a stored value no longer parses: the caller rewrites the header.
    fn fields(&self) -> Option<HeaderFields> {
        let scope = match Audience::parse(&self.audience)? {
            Audience::Private => Scope::private(self.owner_profile_id.as_deref()?).ok()?,
            Audience::Household => Scope::household(self.owner_profile_id.as_deref()),
        };
        Some(HeaderFields {
            kind: Kind::parse(&self.kind)?,
            source_id: self.source_id.clone(),
            external_id: self.external_id.clone(),
            scope,
            sensitivity: parse_sensitivity(&self.sensitivity)?,
            modality: Modality::parse(&self.modality)?,
            observed_at: Some(self.observed_at.clone()),
            valid_until: self.valid_until.clone(),
            title: self.title.clone(),
            body: self.body.clone(),
            context: self.context.clone(),
            state: State::parse(&self.state)?,
            produced_by: ProducedBy::parse(&self.produced_by)?,
            confidence: self.confidence.map(|c| c as f32),
        })
    }
}

async fn stored_by_id(conn: &mut SqliteConnection, id: &str) -> Result<Option<StoredHeader>> {
    Ok(
        sqlx::query_as(&format!("SELECT {HEADER_COLUMNS} FROM items WHERE id = ?"))
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

async fn stored_by_source(
    conn: &mut SqliteConnection,
    source_id: &str,
    external_id: &str,
) -> Result<Option<StoredHeader>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {HEADER_COLUMNS} FROM items WHERE source_id = ? AND external_id = ?"
    ))
    .bind(source_id)
    .bind(external_id)
    .fetch_optional(&mut *conn)
    .await?)
}

async fn profile_exists(conn: &mut SqliteConnection, id: &str) -> Result<bool> {
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM profiles WHERE id = ?")
        .bind(id)
        .fetch_one(&mut *conn)
        .await?;
    Ok(n > 0)
}

/// Make the header of one detail row match `desired`, or remove it when `desired` is `None`.
/// `table` and `key_column` come from this file only, never from a caller's input.
async fn apply(
    conn: &mut SqliteConnection,
    table: &str,
    key_column: &str,
    key: &str,
    linked: Option<String>,
    mut desired: Option<HeaderFields>,
) -> Result<Synced> {
    // An owner who has left gets no header (suggestion_queue's owner has no foreign key).
    if let Some(owner) = desired.as_ref().and_then(|d| d.scope.owner()) {
        if !profile_exists(conn, owner).await? {
            desired = None;
        }
    }
    let mut existing = match &linked {
        Some(id) => stored_by_id(conn, id).await?,
        None => None,
    };
    if existing.is_none() {
        if let Some(d) = &desired {
            existing = stored_by_source(conn, &d.source_id, &d.external_id).await?;
        }
    }

    let Some(mut desired) = desired else {
        let Some(existing) = existing else {
            return Ok(Synced::Unchanged);
        };
        sqlx::query(&format!(
            "UPDATE {table} SET item_id = NULL WHERE {key_column} = ?"
        ))
        .bind(key)
        .execute(&mut *conn)
        .await?;
        sqlx::query("DELETE FROM items WHERE id = ?")
            .bind(&existing.id)
            .execute(&mut *conn)
            .await?;
        return Ok(Synced::Removed);
    };

    let now = timestamp::now();
    if desired.observed_at.is_none() {
        desired.observed_at = Some(
            existing
                .as_ref()
                .map(|e| e.observed_at.clone())
                .unwrap_or_else(|| now.clone()),
        );
    }

    let (id, outcome) = match &existing {
        Some(stored) if stored.fields().as_ref() == Some(&desired) => {
            (stored.id.clone(), Synced::Unchanged)
        }
        Some(stored) => {
            write(conn, &stored.id, &desired, None, &now).await?;
            (stored.id.clone(), Synced::Updated)
        }
        None => {
            let id = ItemId::generate().to_string();
            write(conn, &id, &desired, Some(&now), &now).await?;
            (id, Synced::Created)
        }
    };
    if linked.as_deref() != Some(id.as_str()) {
        sqlx::query(&format!(
            "UPDATE {table} SET item_id = ? WHERE {key_column} = ?"
        ))
        .bind(&id)
        .bind(key)
        .execute(&mut *conn)
        .await?;
    }
    Ok(outcome)
}

/// Insert when `created_at` is given, otherwise update every field of the header `id`.
async fn write(
    conn: &mut SqliteConnection,
    id: &str,
    h: &HeaderFields,
    created_at: Option<&str>,
    updated_at: &str,
) -> Result<()> {
    let query = match created_at {
        Some(_) => {
            "INSERT INTO items (kind, source_id, external_id, owner_profile_id, audience, \
             sensitivity, modality, observed_at, valid_until, title, body, context, state, \
             produced_by, confidence, updated_at, created_at, id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        }
        None => {
            "UPDATE items SET kind = ?, source_id = ?, external_id = ?, owner_profile_id = ?, \
             audience = ?, sensitivity = ?, modality = ?, observed_at = ?, valid_until = ?, \
             title = ?, body = ?, context = ?, state = ?, produced_by = ?, confidence = ?, \
             updated_at = ? WHERE id = ?"
        }
    };
    let mut q = sqlx::query(query)
        .bind(h.kind.as_str())
        .bind(&h.source_id)
        .bind(&h.external_id)
        .bind(h.scope.owner())
        .bind(h.scope.audience().as_str())
        .bind(sensitivity_str(h.sensitivity))
        .bind(h.modality.as_str())
        .bind(h.observed_at.as_deref())
        .bind(h.valid_until.as_deref())
        .bind(&h.title)
        .bind(&h.body)
        .bind(&h.context)
        .bind(h.state.as_str())
        .bind(h.produced_by.to_stored())
        .bind(h.confidence.map(f64::from))
        .bind(updated_at);
    if let Some(created_at) = created_at {
        q = q.bind(created_at);
    }
    q.bind(id).execute(&mut *conn).await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct MemoryDetail {
    id: String,
    item_id: Option<String>,
    profile_id: Option<String>,
    segment: Option<String>,
    lifecycle: Option<String>,
    content: String,
    source: String,
    created_at: String,
}

/// Keep a memory's header and links in step with its row.
pub async fn sync_memory(conn: &mut SqliteConnection, id: &str) -> Result<Synced> {
    let synced = sync_memory_header(conn, id).await?;
    link_memory(conn, Some(id)).await?;
    Ok(synced)
}

async fn sync_memory_header(conn: &mut SqliteConnection, id: &str) -> Result<Synced> {
    let row: Option<MemoryDetail> = sqlx::query_as(
        "SELECT id, item_id, profile_id, segment, lifecycle, content, source, created_at \
         FROM memory_fragments WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(Synced::Unchanged);
    };
    let desired = memory_header(
        &MemoryRow {
            id: &row.id,
            profile_id: row.profile_id.as_deref(),
            segment: row.segment.as_deref(),
            lifecycle: row.lifecycle.as_deref(),
            content: &row.content,
            source: &row.source,
            created_at: &row.created_at,
        },
        redactor(),
    );
    apply(
        conn,
        "memory_fragments",
        "id",
        id,
        row.item_id,
        Some(desired),
    )
    .await
}

#[derive(sqlx::FromRow)]
struct SessionDetail {
    item_id: Option<String>,
    profile_id: Option<String>,
    title: Option<String>,
    rolling_summary: Option<String>,
    created_at: String,
}

/// Keep a conversation's header in step with its session: owner, title and summary.
pub async fn sync_conversation(conn: &mut SqliteConnection, session_id: &str) -> Result<Synced> {
    let synced = sync_conversation_header(conn, session_id).await?;
    link_conversation(conn, Some(session_id)).await?;
    Ok(synced)
}

async fn sync_conversation_header(conn: &mut SqliteConnection, session_id: &str) -> Result<Synced> {
    let row: Option<SessionDetail> = sqlx::query_as(
        "SELECT item_id, profile_id, title, rolling_summary, created_at \
         FROM sessions WHERE id = ?",
    )
    .bind(session_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(Synced::Unchanged);
    };
    let desired = conversation_header(
        &SessionRow {
            id: session_id,
            profile_id: row.profile_id.as_deref(),
            title: row.title.as_deref(),
            rolling_summary: row.rolling_summary.as_deref(),
            created_at: &row.created_at,
        },
        redactor(),
    );
    apply(conn, "sessions", "id", session_id, row.item_id, desired).await
}

#[derive(sqlx::FromRow)]
struct ContextDetail {
    item_id: Option<String>,
    source_id: String,
    external_id: String,
    profile_id: String,
    source_kind: String,
    item_kind: String,
    occurred_at: String,
    title: String,
    body: String,
    sensitivity: String,
}

/// Keep a context item's header in step with its row; `id` is `context_items.id`.
pub async fn sync_context_item(conn: &mut SqliteConnection, id: &str) -> Result<Synced> {
    let row: Option<ContextDetail> = sqlx::query_as(
        "SELECT item_id, source_id, external_id, profile_id, source_kind, item_kind, \
         occurred_at, title, body, sensitivity FROM context_items WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(Synced::Unchanged);
    };
    // Unreadable means stored around the pipeline; the strictest reading is the safe one.
    let sensitivity = parse_sensitivity(&row.sensitivity).unwrap_or(PrivacySensitivity::Secret);
    let desired = context_header(
        &ContextRow {
            source_id: &row.source_id,
            external_id: &row.external_id,
            profile_id: &row.profile_id,
            source_kind: &row.source_kind,
            item_kind: &row.item_kind,
            occurred_at: &row.occurred_at,
            title: &row.title,
            body: &row.body,
            sensitivity,
        },
        redactor(),
    );
    apply(conn, "context_items", "id", id, row.item_id, desired).await
}

#[derive(sqlx::FromRow)]
struct SuggestionDetail {
    item_id: Option<String>,
    profile_id: Option<String>,
    prompt: String,
    reason: String,
    state: String,
    created_at: String,
}

pub async fn sync_suggestion(conn: &mut SqliteConnection, id: &str) -> Result<Synced> {
    let synced = sync_suggestion_header(conn, id).await?;
    link_suggestion(conn, Some(id)).await?;
    Ok(synced)
}

async fn sync_suggestion_header(conn: &mut SqliteConnection, id: &str) -> Result<Synced> {
    let row: Option<SuggestionDetail> = sqlx::query_as(
        "SELECT item_id, profile_id, prompt, reason, state, created_at \
         FROM suggestion_queue WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(Synced::Unchanged);
    };
    let desired = suggestion_header(
        &SuggestionRow {
            id,
            profile_id: row.profile_id.as_deref(),
            prompt: &row.prompt,
            reason: &row.reason,
            state: &row.state,
            created_at: &row.created_at,
        },
        redactor(),
    );
    apply(
        conn,
        "suggestion_queue",
        "id",
        id,
        row.item_id,
        Some(desired),
    )
    .await
}

/// Links are only ever added here: they go when either end's header does (ON DELETE CASCADE),
/// and an agent's own links between the same items are never undone by a later sync.
/// `select` yields `(from_id, to_id, row_key)`; `key` limits it to one row, `None` to none.
async fn link(
    conn: &mut SqliteConnection,
    relation: Relation,
    select: &str,
    key: Option<&str>,
) -> Result<u64> {
    let sql = format!(
        "INSERT OR IGNORE INTO item_links (from_id, relation, to_id, created_at) \
         SELECT l.from_id, ?, l.to_id, ? FROM ({select}) l \
         WHERE l.from_id IS NOT NULL AND l.to_id IS NOT NULL AND l.from_id <> l.to_id \
         AND (? IS NULL OR l.row_key = ?)"
    );
    Ok(sqlx::query(&sql)
        .bind(relation.as_str())
        .bind(timestamp::now())
        .bind(key)
        .bind(key)
        .execute(&mut *conn)
        .await?
        .rows_affected())
}

/// The conversation a memory came from is evidence for it.
fn conversation_taught(row_key: &str) -> String {
    format!(
        "SELECT s.item_id AS from_id, m.item_id AS to_id, {row_key} AS row_key \
         FROM memory_fragments m JOIN sessions s ON s.id = m.session_id"
    )
}

/// The memory a merge produced supersedes the one it replaced.
fn newer_supersedes(row_key: &str) -> String {
    format!(
        "SELECT n.item_id AS from_id, o.item_id AS to_id, {row_key} AS row_key \
         FROM memory_fragments o JOIN memory_fragments n ON n.id = o.superseded_by"
    )
}

/// A suggestion is derived from the memory it was composed from.
fn suggested_from(row_key: &str) -> String {
    format!(
        "SELECT q.item_id AS from_id, m.item_id AS to_id, {row_key} AS row_key \
         FROM suggestion_queue q JOIN memory_fragments m ON m.id = q.source_memory_id"
    )
}

/// A memory's links, whichever end it is: its conversation, its merges, its suggestions.
async fn link_memory(conn: &mut SqliteConnection, id: Option<&str>) -> Result<u64> {
    Ok(link(
        conn,
        Relation::EvidenceFor,
        &conversation_taught("m.id"),
        id,
    )
    .await?
        + link(conn, Relation::Supersedes, &newer_supersedes("n.id"), id).await?
        + link(conn, Relation::Supersedes, &newer_supersedes("o.id"), id).await?
        + link(conn, Relation::DerivedFrom, &suggested_from("m.id"), id).await?)
}

async fn link_conversation(conn: &mut SqliteConnection, session_id: Option<&str>) -> Result<u64> {
    link(
        conn,
        Relation::EvidenceFor,
        &conversation_taught("s.id"),
        session_id,
    )
    .await
}

async fn link_suggestion(conn: &mut SqliteConnection, id: Option<&str>) -> Result<u64> {
    link(conn, Relation::DerivedFrom, &suggested_from("q.id"), id).await
}

/// Every link any row implies, once each.
async fn link_all(conn: &mut SqliteConnection) -> Result<u64> {
    Ok(link(
        conn,
        Relation::EvidenceFor,
        &conversation_taught("NULL"),
        None,
    )
    .await?
        + link(conn, Relation::Supersedes, &newer_supersedes("NULL"), None).await?
        + link(conn, Relation::DerivedFrom, &suggested_from("NULL"), None).await?)
}

/// Headers whose detail row is gone; the delete triggers make this a repair, not a routine.
async fn remove_orphans(conn: &mut SqliteConnection) -> Result<u64> {
    let mut removed = 0;
    for (source_id, table) in [
        (source::MEMORY, "memory_fragments"),
        (source::SESSION, "sessions"),
        (source::SUGGESTION, "suggestion_queue"),
    ] {
        removed += sqlx::query(&format!(
            "DELETE FROM items WHERE source_id = ? AND id NOT IN \
             (SELECT item_id FROM {table} WHERE item_id IS NOT NULL)"
        ))
        .bind(source_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    }
    let context_kinds: Vec<&str> = Kind::ALL
        .into_iter()
        .filter(|k| k.detail_table() == "context_items")
        .map(Kind::as_str)
        .collect();
    let placeholders = vec!["?"; context_kinds.len()].join(", ");
    let sql = format!(
        "DELETE FROM items WHERE source_id NOT LIKE 'pond:%' AND kind IN ({placeholders}) \
         AND id NOT IN (SELECT item_id FROM context_items WHERE item_id IS NOT NULL)"
    );
    let mut q = sqlx::query(&sql);
    for kind in context_kinds {
        q = q.bind(kind);
    }
    removed += q.execute(&mut *conn).await?.rows_affected();
    Ok(removed)
}

/// Rows per transaction: long enough to be quick, short enough not to hold off a turn's write.
const BATCH: usize = 200;

async fn keys(pool: &Pool<Sqlite>, sql: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(sql).fetch_all(pool).await?)
}

/// Write the header of every row that lacks one ([`Mode::Missing`]) or bring every header into
/// line with its row ([`Mode::All`]), then add the links those rows imply.
pub async fn reconcile(pool: &Pool<Sqlite>, mode: Mode) -> Result<ReconcileReport> {
    let missing = matches!(mode, Mode::Missing);
    let only_missing = |sql: &str, extra: &str| -> String {
        if missing {
            format!("{sql} WHERE item_id IS NULL {extra}")
        } else {
            sql.to_string()
        }
    };
    let mut report = ReconcileReport::default();

    // Sessions first, so a memory's evidence link finds its conversation in the same pass.
    for (table, extra) in [
        ("sessions", "AND profile_id IS NOT NULL"),
        ("memory_fragments", ""),
        ("context_items", ""),
        ("suggestion_queue", ""),
    ] {
        let ids = keys(
            pool,
            &only_missing(&format!("SELECT id FROM {table}"), extra),
        )
        .await?;
        for chunk in ids.chunks(BATCH) {
            let mut tx = pool.begin().await?;
            for id in chunk {
                report.count(sync_header(&mut tx, table, id).await?);
            }
            tx.commit().await?;
        }
    }

    let mut tx = pool.begin().await?;
    report.removed += remove_orphans(&mut tx).await?;
    report.linked += link_all(&mut tx).await?;
    tx.commit().await?;
    Ok(report)
}

/// The per-row header sync for one detail table, without its link pass.
async fn sync_header(conn: &mut SqliteConnection, table: &str, id: &str) -> Result<Synced> {
    match table {
        "sessions" => sync_conversation_header(conn, id).await,
        "memory_fragments" => sync_memory_header(conn, id).await,
        "context_items" => sync_context_item(conn, id).await,
        "suggestion_queue" => sync_suggestion_header(conn, id).await,
        other => anyhow::bail!("no header sync for {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::sqlite_memory::SqliteMemoryRepository;
    use crate::sqlite_session_storage::SqliteSessionStorage;
    use crate::sqlite_suggestion_queue::SqliteSuggestionQueue;
    use pond_core::user_data::domain::memory::{MemoryFragment, MemoryLifecycle};
    use pond_core::user_data::domain::session::{IdentificationSource, SessionIdentity};
    use pond_core::user_data::ports::memory_repository::MemoryRepository;
    use pond_core::user_data::ports::session_storage::SessionStorage;
    use pond_core::user_data::ports::suggestion_queue::{Settled, SuggestionQueueRepository};
    use pond_core::user_data::services::suggestion_generation::GeneratedSuggestion;
    use tempfile::TempDir;

    const KEY: &str = "sk-abcdefghijklmnopqrstuvwxyz123456";

    async fn pond() -> (TempDir, Pool<Sqlite>) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::init(dir.path()).await.unwrap();
        (dir, db.system)
    }

    async fn run(pool: &Pool<Sqlite>, sql: &str) {
        sqlx::query(sql)
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    /// Rows as a pond had them before items existed: no `item_id` anywhere.
    async fn legacy_rows(pool: &Pool<Sqlite>) {
        run(pool, "INSERT INTO profiles (id, display_name) VALUES ('amina', 'Amina'), ('baraka', 'Baraka')").await;
        run(
            pool,
            &format!(
                "INSERT INTO sessions (id, created_at, updated_at, profile_id, title, rolling_summary) VALUES \
                 ('s-amina', '2026-10-01 08:00:00', '2026-10-01 08:00:00', 'amina', 'Tea', 'Amina pasted {KEY} by mistake.'), \
                 ('s-unknown', '2026-10-02 08:00:00', '2026-10-02 08:00:00', NULL, 'Hello', 'Someone said hello.')"
            ),
        )
        .await;
        run(
            pool,
            "INSERT INTO memory_fragments (id, profile_id, session_id, content, source, tags, created_at, segment, lifecycle, superseded_by) VALUES \
             ('m-old', 'amina', 's-amina', 'Amina drinks tea', 'extraction', '[]', '2026-10-01 08:01:00', 'preference', 'merged', 'm-new'), \
             ('m-new', 'amina', 's-amina', 'Amina drinks green tea', 'consolidation', '[]', '2026-10-03 08:01:00', 'preference', 'active', NULL), \
             ('m-shared', NULL, NULL, 'The wifi is on the router', 'chat', '[]', '2026-10-04T08:01:00+00:00', 'knowledge', 'active', NULL), \
             ('m-heard', NULL, 's-unknown', 'Someone likes jazz', 'extraction', '[]', 'not a date', NULL, 'archived', NULL)",
        )
        .await;
        run(
            pool,
            "INSERT INTO context_sources (id, kind, provider, profile_id, created_at) \
             VALUES ('src', 'mail', 'imap', 'baraka', '2026-10-01T00:00:00Z')",
        )
        .await;
        run(
            pool,
            "INSERT INTO context_items (id, source_id, external_id, profile_id, source_kind, item_kind, \
             occurred_at, ingested_at, title, body, sensitivity) VALUES \
             ('src:1', 'src', '1', 'baraka', 'mail', 'message', '2026-10-05T07:00:00.123456789+00:00', \
              '2026-10-05T07:01:00Z', 'Invoice', 'Due Friday', 'sensitive')",
        )
        .await;
        run(
            pool,
            "INSERT INTO suggestion_queue (id, profile_id, prompt, reason, source_memory_id, answered_by, created_at, state) VALUES \
             ('q-amina', 'amina', 'Brew some tea?', 'You like green tea.', 'm-new', 'giap-memory', '2026-10-06T00:00:00Z', 'taken'), \
             ('q-house', NULL, 'Check the router?', 'The wifi is on the router.', 'm-shared', 'giap-memory', '2026-10-06T00:00:00Z', 'queued'), \
             ('q-gone', 'left-the-house', 'Ask about it?', 'A memory.', 'm-shared', 'giap-memory', '2026-10-06T00:00:00Z', 'dismissed')",
        )
        .await;
    }

    #[derive(Debug, sqlx::FromRow, PartialEq)]
    struct Row {
        kind: String,
        owner_profile_id: Option<String>,
        audience: String,
        state: String,
        title: String,
        body: String,
        observed_at: String,
        produced_by: String,
    }

    async fn header(pool: &Pool<Sqlite>, table: &str, id: &str) -> Option<Row> {
        sqlx::query_as(&format!(
            "SELECT i.kind, i.owner_profile_id, i.audience, i.state, i.title, i.body, \
             i.observed_at, i.produced_by FROM {table} d JOIN items i ON i.id = d.item_id \
             WHERE d.id = ?"
        ))
        .bind(id)
        .fetch_optional(pool)
        .await
        .unwrap()
    }

    async fn links(pool: &Pool<Sqlite>) -> Vec<(String, String, String)> {
        sqlx::query_as(
            "SELECT f.external_id, l.relation, t.external_id FROM item_links l \
             JOIN items f ON f.id = l.from_id JOIN items t ON t.id = l.to_id \
             ORDER BY 1, 2, 3",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn everything(pool: &Pool<Sqlite>) -> Vec<(String, String, String, String)> {
        sqlx::query_as(
            "SELECT id, source_id || '/' || external_id, updated_at, \
             kind || state || audience || coalesce(owner_profile_id, '') || title || body || observed_at \
             FROM items ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn the_backfill_maps_each_table_by_its_own_rule() {
        let (_dir, pool) = pond().await;
        legacy_rows(&pool).await;
        let report = reconcile(&pool, Mode::Missing).await.unwrap();
        assert_eq!(report.created, 8, "{report:?}");

        let amina = header(&pool, "sessions", "s-amina").await.unwrap();
        assert_eq!(
            (
                amina.kind.as_str(),
                amina.owner_profile_id.as_deref(),
                amina.audience.as_str()
            ),
            ("conversation", Some("amina"), "private")
        );
        assert!(
            !amina.body.contains(KEY),
            "a header kept a credential: {}",
            amina.body
        );
        assert!(amina.body.contains("[redacted:api-key]"), "{}", amina.body);
        let replayed: String =
            sqlx::query_scalar("SELECT rolling_summary FROM sessions WHERE id = 's-amina'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            replayed.contains(KEY),
            "the backfill changed what the conversation replays"
        );
        assert!(
            header(&pool, "sessions", "s-unknown").await.is_none(),
            "a conversation with no known speaker got a header"
        );

        let old = header(&pool, "memory_fragments", "m-old").await.unwrap();
        assert_eq!(
            (old.kind.as_str(), old.state.as_str()),
            ("trait", "superseded")
        );
        assert_eq!(old.observed_at, "2026-10-01T08:01:00.000Z");
        assert_eq!(old.produced_by, "pond:extraction");
        let shared = header(&pool, "memory_fragments", "m-shared").await.unwrap();
        assert_eq!(
            (
                shared.kind.as_str(),
                shared.owner_profile_id,
                shared.audience.as_str()
            ),
            ("fact", None, "household")
        );
        let heard = header(&pool, "memory_fragments", "m-heard").await.unwrap();
        assert_eq!(heard.state, "archived");

        let mail = header(&pool, "context_items", "src:1").await.unwrap();
        assert_eq!(
            (
                mail.kind.as_str(),
                mail.owner_profile_id.as_deref(),
                mail.produced_by.as_str()
            ),
            ("mail", Some("baraka"), "ingest")
        );
        assert_eq!(mail.observed_at, "2026-10-05T07:00:00.123Z");

        let taken = header(&pool, "suggestion_queue", "q-amina").await.unwrap();
        assert_eq!(
            (taken.state.as_str(), taken.audience.as_str()),
            ("accepted", "private")
        );
        let house = header(&pool, "suggestion_queue", "q-house").await.unwrap();
        assert_eq!(
            (house.state.as_str(), house.audience.as_str()),
            ("active", "household")
        );
        assert!(
            header(&pool, "suggestion_queue", "q-gone").await.is_none(),
            "a suggestion for a member who left got a header"
        );

        assert_eq!(
            links(&pool).await,
            vec![
                ("m-new".into(), "supersedes".into(), "m-old".into()),
                ("q-amina".into(), "derived_from".into(), "m-new".into()),
                ("q-house".into(), "derived_from".into(), "m-shared".into()),
                ("s-amina".into(), "evidence_for".into(), "m-new".into()),
                ("s-amina".into(), "evidence_for".into(), "m-old".into()),
            ]
        );
    }

    #[tokio::test]
    async fn running_it_again_changes_nothing() {
        let (_dir, pool) = pond().await;
        legacy_rows(&pool).await;
        reconcile(&pool, Mode::Missing).await.unwrap();
        let before = everything(&pool).await;
        let links_before = links(&pool).await;
        for mode in [Mode::Missing, Mode::All, Mode::All] {
            let report = reconcile(&pool, mode).await.unwrap();
            assert!(!report.changed(), "{mode:?} changed something: {report:?}");
        }
        assert_eq!(everything(&pool).await, before);
        assert_eq!(links(&pool).await, links_before);
    }

    #[tokio::test]
    async fn a_full_pass_repairs_a_header_a_writer_left_stale() {
        let (_dir, pool) = pond().await;
        legacy_rows(&pool).await;
        reconcile(&pool, Mode::Missing).await.unwrap();
        run(
            &pool,
            "UPDATE memory_fragments SET content = 'Amina drinks oolong' WHERE id = 'm-new'",
        )
        .await;
        run(
            &pool,
            "UPDATE sessions SET profile_id = NULL WHERE id = 's-amina'",
        )
        .await;
        run(&pool, "DELETE FROM items WHERE external_id = 'm-shared'").await;

        let report = reconcile(&pool, Mode::All).await.unwrap();
        assert_eq!(
            (report.created, report.updated, report.removed),
            (1, 1, 1),
            "{report:?}"
        );
        assert_eq!(
            header(&pool, "memory_fragments", "m-new")
                .await
                .unwrap()
                .body,
            "Amina drinks oolong"
        );
        assert!(header(&pool, "sessions", "s-amina").await.is_none());
        assert!(header(&pool, "memory_fragments", "m-shared")
            .await
            .is_some());
    }

    #[tokio::test]
    async fn memory_writers_keep_the_header_current() {
        let (_dir, pool) = pond().await;
        run(
            &pool,
            "INSERT INTO profiles (id, display_name) VALUES ('amina', 'Amina')",
        )
        .await;
        let repo = SqliteMemoryRepository::new(pool.clone());
        let fragment =
            MemoryFragment::from_chat("m1".into(), Some("amina".into()), None, "likes chai".into());
        repo.add(fragment).await.unwrap();
        let h = header(&pool, "memory_fragments", "m1").await.unwrap();
        assert_eq!(
            (h.body.as_str(), h.state.as_str()),
            ("likes chai", "active")
        );

        repo.update_content("m1", "likes masala chai", None)
            .await
            .unwrap();
        assert_eq!(
            header(&pool, "memory_fragments", "m1").await.unwrap().body,
            "likes masala chai"
        );
        repo.update_lifecycle("m1", MemoryLifecycle::Archived)
            .await
            .unwrap();
        assert_eq!(
            header(&pool, "memory_fragments", "m1").await.unwrap().state,
            "archived"
        );

        repo.add(MemoryFragment::from_chat(
            "m2".into(),
            Some("amina".into()),
            None,
            "likes coffee".into(),
        ))
        .await
        .unwrap();
        repo.mark_superseded("m1", "m2").await.unwrap();
        assert_eq!(
            header(&pool, "memory_fragments", "m1").await.unwrap().state,
            "superseded"
        );
        assert_eq!(
            links(&pool).await,
            vec![("m2".into(), "supersedes".into(), "m1".into())]
        );

        repo.delete("m1").await.unwrap();
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM items WHERE external_id = 'm1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
        assert!(links(&pool).await.is_empty(), "a link outlived its end");
        assert!(!reconcile(&pool, Mode::All).await.unwrap().changed());
    }

    #[tokio::test]
    async fn a_conversation_gets_its_header_when_its_speaker_is_known() {
        let (_dir, pool) = pond().await;
        run(
            &pool,
            "INSERT INTO profiles (id, display_name) VALUES ('amina', 'Amina')",
        )
        .await;
        let sessions = SqliteSessionStorage::new(pool.clone());
        sessions.create_session("s1".into()).await.unwrap();
        sessions
            .set_rolling_summary("s1", "Talked about tea.", "msg-1")
            .await
            .unwrap();
        assert!(header(&pool, "sessions", "s1").await.is_none());

        let amina = SessionIdentity {
            profile_id: Some("amina".into()),
            source: IdentificationSource::PairedDevice,
            confidence: None,
        };
        sessions.set_session_identity("s1", &amina).await.unwrap();
        let h = header(&pool, "sessions", "s1").await.unwrap();
        assert_eq!(
            (h.owner_profile_id.as_deref(), h.body.as_str()),
            (Some("amina"), "Talked about tea.")
        );

        sessions
            .set_generated_title("s1", "Tea", "msg-1")
            .await
            .unwrap();
        sessions
            .set_rolling_summary("s1", &format!("Talked about tea and {KEY}."), "msg-2")
            .await
            .unwrap();
        let h = header(&pool, "sessions", "s1").await.unwrap();
        assert_eq!(h.title, "Tea");
        assert!(!h.body.contains(KEY), "{}", h.body);

        let nobody = SessionIdentity {
            profile_id: None,
            source: IdentificationSource::Unknown,
            confidence: None,
        };
        sessions.set_session_identity("s1", &nobody).await.unwrap();
        assert!(header(&pool, "sessions", "s1").await.is_none());
        let headers: i64 = sqlx::query_scalar("SELECT count(*) FROM items")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            headers, 0,
            "an unattributed conversation's header was left behind"
        );
    }

    #[tokio::test]
    async fn suggestion_writers_keep_the_header_current() {
        let (_dir, pool) = pond().await;
        run(
            &pool,
            "INSERT INTO memory_fragments (id, content, source, tags, created_at) \
             VALUES ('m1', 'The wifi is on the router', 'chat', '[]', datetime('now'))",
        )
        .await;
        reconcile(&pool, Mode::Missing).await.unwrap();
        let queue = SqliteSuggestionQueue::new(pool.clone());
        queue
            .queue(&[GeneratedSuggestion {
                source_memory_id: "m1".into(),
                profile_id: None,
                prompt: "Check the router?".into(),
                reason: "The wifi is on the router.".into(),
            }])
            .await
            .unwrap();
        let id: String = sqlx::query_scalar("SELECT id FROM suggestion_queue")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            header(&pool, "suggestion_queue", &id).await.unwrap().state,
            "active"
        );
        assert_eq!(
            links(&pool).await,
            vec![(id.clone(), "derived_from".into(), "m1".into())]
        );
        queue.settle(&id, Settled::Dismissed).await.unwrap();
        assert_eq!(
            header(&pool, "suggestion_queue", &id).await.unwrap().state,
            "dismissed"
        );
    }
}
