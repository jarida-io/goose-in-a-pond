//! SQLite-backed implementation of the SessionStorage port.
//!
//! Wraps `Pool<Sqlite>` pointing at `pond_system.db`.
//! Tables are created by `migrations/system/0001_initial.sql`.

use async_trait::async_trait;
use base64::Engine as _;
use pond_core::models::domain::image_limits::extension_for_mime;
use pond_core::models::domain::message::{ChatMessage, ImageAttachment, Role, ToolCallRecord};
use pond_core::user_data::domain::session::{
    ExtractionCursor, IdentificationSource, MessageAttachment, Session, SessionIdentity,
    SessionMessage,
};
use pond_core::user_data::ports::session_storage::{SessionStorage, SessionStorageError};
use sqlx::{Pool, Row, Sqlite};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── Raw DB row types ──────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: String,
    title: Option<String>,
    profile_id: Option<String>,
    total_prompt_tokens: i64,
    total_completion_tokens: i64,
    model_name: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(sqlx::FromRow)]
struct MessageRow {
    id: String,
    session_id: String,
    role: String,
    content: String,
    tool_call_id: Option<String>,
    tool_calls_json: Option<String>,
    created_at: String,
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
    liked: Option<i64>,
}

// ── Conversion helpers ────────────────────────────────────────────────────────

/// Parse SQLite's `datetime('now')` format ("YYYY-MM-DD HH:MM:SS") to DateTime<Utc>.
fn parse_dt(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .map(|ndt| ndt.and_utc())
        .unwrap_or_else(|_| chrono::Utc::now())
}

/// Same parse, but an unreadable stamp reads as absent rather than as now.
///
/// The extraction cursor's stamp orders the backlog, and [`parse_dt`]'s
/// fallback would silently move a conversation with a malformed stamp to the
/// *back* of the queue -- i.e. a row the engine cannot read the time of would
/// be the last one it ever got to. `None` sorts to the front instead, which is
/// where a conversation of unknown status belongs.
fn parse_dt_opt(s: Option<String>) -> Option<chrono::DateTime<chrono::Utc>> {
    let s = s?;
    chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|ndt| ndt.and_utc())
}

fn role_to_str(role: &Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    }
}

fn str_to_role(s: &str) -> Result<Role, SessionStorageError> {
    match s {
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "system" => Ok(Role::System),
        "tool" => Ok(Role::Tool),
        other => Err(SessionStorageError::StorageError(format!(
            "Unknown role in DB: '{}'",
            other
        ))),
    }
}

impl TryFrom<SessionRow> for Session {
    type Error = SessionStorageError;
    fn try_from(r: SessionRow) -> Result<Self, Self::Error> {
        Ok(Session {
            id: r.id,
            title: r.title,
            profile_id: r.profile_id,
            total_prompt_tokens: r.total_prompt_tokens as u32,
            total_completion_tokens: r.total_completion_tokens as u32,
            model_name: r.model_name,
            created_at: parse_dt(&r.created_at),
            updated_at: parse_dt(&r.updated_at),
        })
    }
}

impl TryFrom<MessageRow> for SessionMessage {
    type Error = SessionStorageError;
    fn try_from(r: MessageRow) -> Result<Self, Self::Error> {
        // Tool-call metadata is stored as JSON. Malformed JSON degrades gracefully:
        // we drop the tool_calls but keep the message rather than failing the read.
        let tool_calls: Vec<ToolCallRecord> = r
            .tool_calls_json
            .as_deref()
            .filter(|s| !s.is_empty())
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        Ok(SessionMessage {
            id: r.id,
            session_id: r.session_id,
            message: ChatMessage {
                role: str_to_role(&r.role)?,
                content: r.content,
                images: Vec::new(),
                tool_calls,
                tool_call_id: r.tool_call_id,
            },
            created_at: parse_dt(&r.created_at),
            prompt_tokens: r.prompt_tokens.map(|v| v as u32),
            completion_tokens: r.completion_tokens.map(|v| v as u32),
            reasoning_tokens: r.reasoning_tokens.map(|v| v as u32),
            liked: r.liked.map(|v| v != 0),
        })
    }
}

// ── Adapter ───────────────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct AttachmentRow {
    id: String,
    message_id: String,
    session_id: String,
    ordinal: i64,
    mime_type: String,
    byte_size: i64,
    created_at: String,
}

impl From<AttachmentRow> for MessageAttachment {
    fn from(r: AttachmentRow) -> Self {
        MessageAttachment {
            id: r.id,
            message_id: r.message_id,
            session_id: r.session_id,
            ordinal: r.ordinal.max(0) as u32,
            mime_type: r.mime_type,
            byte_size: r.byte_size.max(0) as u64,
            created_at: parse_dt(&r.created_at),
        }
    }
}

/// Directory holding image attachment bytes.
///
/// Resolved here rather than threaded down from `pond-server` for the same
/// reason `model_download.rs` duplicates `default_data_dir()`: the storage
/// adapter is constructed from a pool alone in several places, and an extra
/// constructor argument would have to be plumbed through all of them. Honours
/// `POND_DATA_DIR` so an isolated test/measurement run never writes into the
/// real profile.
fn default_attachment_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("POND_DATA_DIR") {
        return PathBuf::from(dir).join("attachments");
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("goose-in-a-pond")
        .join("attachments")
}

/// Reduce an untrusted id to something that cannot escape its parent directory.
///
/// Session and message ids are UUIDs today, but they arrive from the network on
/// several paths (`session_id` is caller-supplied on `/chat/stream`), so a
/// `../../` in one must not become a path. Mirrors `sanitize_camera_id` in
/// `pond-adapters-vision`.
fn sanitize_path_component(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(128)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

pub struct SqliteSessionStorage {
    pool: Pool<Sqlite>,
    /// Where attachment bytes are written. Overridable for tests.
    attachment_dir: PathBuf,
}

impl SqliteSessionStorage {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self {
            pool,
            attachment_dir: default_attachment_dir(),
        }
    }

    /// Point attachment storage at a specific directory (tests, and any future
    /// caller that already knows the data dir).
    #[must_use]
    pub fn with_attachment_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.attachment_dir = dir.into();
        self
    }

    /// Persist one message's images: bytes to disk, one index row each.
    ///
    /// Best-effort per attachment. A full disk or an undecodable payload must
    /// not fail the turn — the message itself is already committed by the time
    /// this runs, and losing a picture is strictly better than losing the
    /// conversation. Every failure is logged with the message id so it is
    /// diagnosable.
    async fn persist_attachments(&self, message: &SessionMessage) {
        if message.message.images.is_empty() {
            return;
        }
        let dir = self
            .attachment_dir
            .join(sanitize_path_component(&message.session_id));
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            tracing::warn!(
                message_id = %message.id,
                dir = %dir.display(),
                error = %e,
                "could not create attachment directory; images for this message are not persisted"
            );
            return;
        }

        for (ordinal, img) in message.message.images.iter().enumerate() {
            let bytes = match base64::engine::general_purpose::STANDARD.decode(
                // Tolerate a data-URL prefix from a client that sent one.
                img.data
                    .rsplit_once("base64,")
                    .map(|(_, b)| b)
                    .unwrap_or(&img.data)
                    .trim(),
            ) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(
                        message_id = %message.id,
                        ordinal,
                        error = %e,
                        "attachment is not valid base64; skipping"
                    );
                    continue;
                }
            };

            let attachment_id = uuid::Uuid::new_v4().to_string();
            let path = dir.join(format!(
                "{}.{}",
                attachment_id,
                extension_for_mime(&img.mime_type)
            ));
            let byte_size = bytes.len() as i64;
            if let Err(e) = tokio::fs::write(&path, &bytes).await {
                tracing::warn!(
                    message_id = %message.id,
                    ordinal,
                    path = %path.display(),
                    error = %e,
                    "could not write attachment bytes; skipping"
                );
                continue;
            }

            if let Err(e) = sqlx::query(
                "INSERT INTO message_attachments \
                     (id, message_id, session_id, ordinal, mime_type, byte_size, file_path, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, datetime('now'))",
            )
            .bind(&attachment_id)
            .bind(&message.id)
            .bind(&message.session_id)
            .bind(ordinal as i64)
            .bind(&img.mime_type)
            .bind(byte_size)
            .bind(path.to_string_lossy().as_ref())
            .execute(&self.pool)
            .await
            {
                // The row is the index; without it the file is unreachable, so
                // do not leave it behind.
                let _ = tokio::fs::remove_file(&path).await;
                tracing::warn!(
                    message_id = %message.id,
                    ordinal,
                    error = %e,
                    "could not index attachment; bytes removed"
                );
            }
        }
    }

    /// Remove a session's attachment directory. Called on session delete, where
    /// the rows disappear via `ON DELETE CASCADE` and would otherwise orphan
    /// their files.
    async fn remove_session_attachment_files(&self, session_id: &str) {
        let dir = self
            .attachment_dir
            .join(sanitize_path_component(session_id));
        if !dir.exists() {
            return;
        }
        if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
            tracing::warn!(
                session_id = %session_id,
                dir = %dir.display(),
                error = %e,
                "could not remove attachment files for deleted session"
            );
        }
    }

    /// Read one attachment file, refusing anything that resolved outside the
    /// attachment root.
    ///
    /// `file_path` comes from our own DB, but a stored row is still a value that
    /// travels (backup restores, hand edits), and this function is reachable from
    /// an HTTP handler — so the containment check is cheap insurance rather than
    /// paranoia.
    async fn read_attachment_file(&self, file_path: &str) -> Option<Vec<u8>> {
        let path = Path::new(file_path);
        let root = self.attachment_dir.canonicalize().ok()?;
        let real = path.canonicalize().ok()?;
        if !real.starts_with(&root) {
            tracing::warn!(
                path = %file_path,
                "attachment path resolved outside the attachment root; refusing to read"
            );
            return None;
        }
        tokio::fs::read(&real).await.ok()
    }
}

#[async_trait]
impl SessionStorage for SqliteSessionStorage {
    async fn create_session(&self, session_id: String) -> Result<Session, SessionStorageError> {
        sqlx::query(
            "INSERT INTO sessions (id, title, created_at, updated_at) \
             VALUES (?, NULL, datetime('now'), datetime('now'))",
        )
        .bind(&session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        self.get_session(&session_id).await
    }

    async fn get_session(&self, session_id: &str) -> Result<Session, SessionStorageError> {
        let row = sqlx::query_as::<_, SessionRow>(
            "SELECT id, title, profile_id, total_prompt_tokens, total_completion_tokens, model_name, created_at, updated_at FROM sessions WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        match row {
            Some(r) => Session::try_from(r),
            None => Err(SessionStorageError::SessionNotFound(session_id.to_string())),
        }
    }

    async fn get_rolling_summary_with_revision(
        &self,
        session_id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionStorageError> {
        // One SELECT, so the text and the stamp cannot disagree. See the port
        // doc: reading them separately races `set_rolling_summary` and lets a
        // stale vector be stamped as current.
        let row = sqlx::query(
            "SELECT rolling_summary, rolling_summary_updated_at FROM sessions WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(row
            .map(|r| {
                (
                    r.get("rolling_summary"),
                    r.get("rolling_summary_updated_at"),
                )
            })
            .unwrap_or((None, None)))
    }

    async fn get_rolling_summary(
        &self,
        session_id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionStorageError> {
        let row = sqlx::query(
            "SELECT rolling_summary, rolling_summary_through_id FROM sessions WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(row
            .map(|r| {
                (
                    r.get("rolling_summary"),
                    r.get("rolling_summary_through_id"),
                )
            })
            .unwrap_or((None, None)))
    }

    async fn set_rolling_summary(
        &self,
        session_id: &str,
        summary: &str,
        through_message_id: &str,
    ) -> Result<(), SessionStorageError> {
        sqlx::query(
            "UPDATE sessions SET rolling_summary = ?, rolling_summary_through_id = ?, \
             rolling_summary_updated_at = datetime('now') WHERE id = ?",
        )
        .bind(summary)
        .bind(through_message_id)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(())
    }

    async fn get_session_id_for_engine(
        &self,
        engine_session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        // Newest pairing wins. engine_session_id is not declared UNIQUE and the
        // table is written from paths that run before a sessions row exists, so
        // a stale duplicate is possible; taking the most recent is the only
        // answer that stays right after a re-pair.
        let row = sqlx::query(
            "SELECT session_id FROM engine_session_map WHERE engine_session_id = ? \
             ORDER BY updated_at DESC LIMIT 1",
        )
        .bind(engine_session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(row.map(|r| r.get("session_id")))
    }

    async fn get_engine_session_id(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        let row =
            sqlx::query("SELECT engine_session_id FROM engine_session_map WHERE session_id = ?")
                .bind(session_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(row.map(|r| r.get("engine_session_id")))
    }

    async fn set_engine_session_id(
        &self,
        session_id: &str,
        engine_session_id: &str,
    ) -> Result<(), SessionStorageError> {
        // No `sessions` existence guard on purpose — see the port doc-comment:
        // the pairing can be established before a GIAP session row exists.
        sqlx::query(
            "INSERT INTO engine_session_map (session_id, engine_session_id, updated_at) \
             VALUES (?, ?, datetime('now')) \
             ON CONFLICT(session_id) DO UPDATE SET \
               engine_session_id = excluded.engine_session_id, \
               updated_at = excluded.updated_at",
        )
        .bind(session_id)
        .bind(engine_session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(())
    }

    async fn get_session_identity(
        &self,
        session_id: &str,
    ) -> Result<SessionIdentity, SessionStorageError> {
        let row = sqlx::query(
            "SELECT profile_id, identification_source, identification_confidence \
             FROM sessions WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        let Some(row) = row else {
            return Ok(SessionIdentity::unknown());
        };

        let stored: Option<String> = row.get("identification_source");
        Ok(SessionIdentity {
            profile_id: row.get("profile_id"),
            // A NULL source is a legacy row, and legacy rows are unattributed.
            source: stored
                .as_deref()
                .map(IdentificationSource::parse)
                .unwrap_or(IdentificationSource::Unknown),
            confidence: row
                .get::<Option<f64>, _>("identification_confidence")
                .map(|c| c as f32),
        })
    }

    async fn set_session_identity(
        &self,
        session_id: &str,
        identity: &SessionIdentity,
    ) -> Result<(), SessionStorageError> {
        // Deliberately does NOT touch `updated_at`. `list_sessions` orders by it,
        // so bumping it here would push a conversation to the top of the user's
        // history because a camera recognised somebody -- reordering what they
        // see without a message having been sent. Attribution is metadata about
        // the session, not activity in it.
        let result = sqlx::query(
            "UPDATE sessions SET \
               profile_id                = ?, \
               identification_source     = ?, \
               identification_confidence = ? \
             WHERE id = ?",
        )
        .bind(identity.profile_id.as_deref())
        .bind(identity.source.as_str())
        .bind(identity.confidence.map(|c| c as f64))
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(SessionStorageError::SessionNotFound(session_id.to_string()));
        }
        Ok(())
    }

    async fn set_session_identity_if_stronger(
        &self,
        session_id: &str,
        identity: &SessionIdentity,
    ) -> Result<bool, SessionStorageError> {
        // The rank comparison happens INSIDE the update, so two concurrent
        // identifications cannot both win off the same stale read. The ranking
        // itself is policy and lives in the domain -- this builds the CASE from
        // `IdentificationSource::ALL_RANKED` rather than restating the order,
        // and a test pins the two together.
        let cases: String = IdentificationSource::ALL_RANKED
            .iter()
            .map(|(name, rank)| format!("WHEN '{name}' THEN {rank}"))
            .collect::<Vec<_>>()
            .join(" ");

        // A NULL source is a legacy row: unattributed, so anything beats it.
        // The literal must be >= the weakest real rank, hence ALL_RANKED's len.
        let unattributed = IdentificationSource::ALL_RANKED.len();

        let sql = format!(
            "UPDATE sessions SET \
               profile_id                = ?, \
               identification_source     = ?, \
               identification_confidence = ? \
             WHERE id = ? \
               AND ? <= (CASE COALESCE(identification_source, '') {cases} ELSE {unattributed} END)"
        );

        let result = sqlx::query(&sql)
            .bind(identity.profile_id.as_deref())
            .bind(identity.source.as_str())
            .bind(identity.confidence.map(|c| c as f64))
            .bind(session_id)
            .bind(identity.source.rank() as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        if result.rows_affected() > 0 {
            return Ok(true);
        }

        // Zero rows is ambiguous: either the session does not exist, or a
        // stronger identification holds it. The caller needs those apart --
        // one is a 404 and the other is a normal refusal.
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE id = ?")
            .bind(session_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        if exists == 0 {
            return Err(SessionStorageError::SessionNotFound(session_id.to_string()));
        }
        Ok(false)
    }

    async fn claim_session_identity(
        &self,
        session_id: &str,
        identity: &SessionIdentity,
    ) -> Result<bool, SessionStorageError> {
        let cases: String = IdentificationSource::ALL_RANKED
            .iter()
            .map(|(name, rank)| format!("WHEN '{name}' THEN {rank}"))
            .collect::<Vec<_>>()
            .join(" ");
        let unattributed = IdentificationSource::ALL_RANKED.len();

        // The strength write's statement, plus the one clause that makes it a
        // claim: the session is unattributed, or already this member's. Inside
        // the UPDATE, not read first, for the reason the trait gives -- a
        // read-then-write lets a second device slip in between.
        let sql = format!(
            "UPDATE sessions SET \
               profile_id                = ?, \
               identification_source     = ?, \
               identification_confidence = ? \
             WHERE id = ? \
               AND (profile_id IS NULL OR profile_id = ?) \
               AND ? <= (CASE COALESCE(identification_source, '') {cases} ELSE {unattributed} END)"
        );

        let result = sqlx::query(&sql)
            .bind(identity.profile_id.as_deref())
            .bind(identity.source.as_str())
            .bind(identity.confidence.map(|c| c as f64))
            .bind(session_id)
            .bind(identity.profile_id.as_deref())
            .bind(identity.source.rank() as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        if result.rows_affected() > 0 {
            return Ok(true);
        }
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE id = ?")
            .bind(session_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        if exists == 0 {
            return Err(SessionStorageError::SessionNotFound(session_id.to_string()));
        }
        Ok(false)
    }

    async fn get_session_tool_groups(
        &self,
        session_id: &str,
    ) -> Result<Option<Vec<String>>, SessionStorageError> {
        let row = sqlx::query("SELECT groups FROM session_tool_groups WHERE session_id = ?")
            .bind(session_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(row.map(|r| {
            let raw: String = r.get("groups");
            raw.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        }))
    }

    async fn set_session_tool_groups(
        &self,
        session_id: &str,
        groups: &[String],
    ) -> Result<(), SessionStorageError> {
        // No `sessions` existence guard on purpose — see the port doc-comment.
        sqlx::query(
            "INSERT INTO session_tool_groups (session_id, groups, updated_at) \
             VALUES (?, ?, datetime('now')) \
             ON CONFLICT(session_id) DO UPDATE SET \
               groups = excluded.groups, \
               updated_at = excluded.updated_at",
        )
        .bind(session_id)
        .bind(groups.join("\n"))
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(())
    }

    async fn add_message(
        &self,
        session_id: String,
        message: SessionMessage,
    ) -> Result<SessionMessage, SessionStorageError> {
        self.get_session(&session_id).await?; // guard: session must exist

        // Serialize tool_calls only when present — keeps storage compact for
        // the common user/system path.
        let tool_calls_json = if message.message.tool_calls.is_empty() {
            None
        } else {
            serde_json::to_string(&message.message.tool_calls).ok()
        };

        sqlx::query(
            "INSERT INTO session_messages \
                 (id, session_id, role, content, tool_call_id, tool_calls_json, created_at, \
                  prompt_tokens, completion_tokens, reasoning_tokens) \
             VALUES (?, ?, ?, ?, ?, ?, datetime('now'), ?, ?, ?)",
        )
        .bind(&message.id)
        .bind(&session_id)
        .bind(role_to_str(&message.message.role))
        .bind(&message.message.content)
        .bind(&message.message.tool_call_id)
        .bind(&tool_calls_json)
        .bind(message.prompt_tokens.map(|v| v as i64))
        .bind(message.completion_tokens.map(|v| v as i64))
        .bind(message.reasoning_tokens.map(|v| v as i64))
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        sqlx::query("UPDATE sessions SET updated_at = datetime('now') WHERE id = ?")
            .bind(&session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        // Phase F2. After the message row commits, so an attachment can never
        // reference a message that does not exist.
        self.persist_attachments(&message).await;

        Ok(message)
    }

    async fn get_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        self.get_session(session_id).await?; // guard: session must exist

        let rows = sqlx::query_as::<_, MessageRow>(
            "SELECT id, session_id, role, content, tool_call_id, tool_calls_json, created_at, \
             prompt_tokens, completion_tokens, reasoning_tokens, liked \
             FROM session_messages \
             WHERE session_id = ? \
             ORDER BY created_at ASC, rowid ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        rows.into_iter().map(SessionMessage::try_from).collect()
    }

    /// Rename a session on a person's behalf.
    ///
    /// Stamps `title_source = 'user'`, which puts the session permanently out
    /// of the re-titling job's reach. Its only production caller is the rename
    /// endpoint; the two machine writers have their own methods precisely so
    /// this one can mean "somebody typed this" without ambiguity.
    async fn update_title(
        &self,
        session_id: &str,
        title: String,
    ) -> Result<(), SessionStorageError> {
        self.get_session(session_id).await?; // guard: session must exist

        sqlx::query(
            "UPDATE sessions SET title = ?, title_source = 'user', \
             title_through_message_id = NULL, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&title)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(())
    }

    /// Two indexed queries rather than a history load: find the anchor's sort
    /// position, then count what sorts after it. The ordering pair matches
    /// `get_messages` (`created_at ASC, rowid ASC`) so "after" means the same
    /// thing here as it does when the conversation is read.
    ///
    /// The anchor is fetched separately rather than as a subquery because a
    /// missing anchor must be distinguishable from an anchor with nothing
    /// after it — as a subquery both answer `0`.
    async fn messages_after(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<Option<u64>, SessionStorageError> {
        let anchor: Option<(String, i64)> = sqlx::query_as(
            "SELECT created_at, rowid FROM session_messages WHERE id = ? AND session_id = ?",
        )
        .bind(message_id)
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        let Some((created_at, rowid)) = anchor else {
            return Ok(None);
        };

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM session_messages \
             WHERE session_id = ? AND (created_at > ? OR (created_at = ? AND rowid > ?))",
        )
        .bind(session_id)
        .bind(&created_at)
        .bind(&created_at)
        .bind(rowid)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(Some(count.max(0) as u64))
    }

    async fn get_title_provenance(
        &self,
        session_id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionStorageError> {
        let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT title_source, title_through_message_id FROM sessions WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(row.unwrap_or((None, None)))
    }

    /// Write the deterministic fallback title, marked as such so the
    /// re-titling job knows it may improve on it.
    ///
    /// Does NOT touch `updated_at`: this runs on the first turn of a session,
    /// and the activity clock that gates every background job reads that
    /// column. Bumping it here would be the pond reporting its own
    /// bookkeeping as user activity.
    async fn set_derived_title(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<(), SessionStorageError> {
        self.get_session(session_id).await?; // guard: session must exist

        sqlx::query(
            "UPDATE sessions SET title = ?, title_source = 'derived', \
             title_through_message_id = NULL WHERE id = ?",
        )
        .bind(title)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(())
    }

    /// Write a model-generated title and record how far it reaches.
    ///
    /// Same reasoning on `updated_at` as [`set_derived_title`]: a background
    /// rename is not activity, and treating it as such would let the job
    /// reset the very idle clock that permitted it to run.
    async fn set_generated_title(
        &self,
        session_id: &str,
        title: &str,
        through_message_id: &str,
    ) -> Result<(), SessionStorageError> {
        self.get_session(session_id).await?; // guard: session must exist

        sqlx::query(
            "UPDATE sessions SET title = ?, title_source = 'model', \
             title_through_message_id = ? WHERE id = ?",
        )
        .bind(title)
        .bind(through_message_id)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(())
    }

    async fn extraction_cursor(
        &self,
        session_id: &str,
    ) -> Result<ExtractionCursor, SessionStorageError> {
        // Deliberately not guarded on session existence, like `count_messages`:
        // a session that is not there has not been examined, and that is the
        // answer the walk wants rather than an error it would have to decide
        // what to do with.
        let row: Option<(Option<String>, Option<String>, i64)> = sqlx::query_as(
            "SELECT extracted_through_id, extracted_at, extraction_attempts \
             FROM sessions WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(match row {
            Some((through_message_id, extracted_at, attempts)) => ExtractionCursor {
                through_message_id,
                extracted_at: parse_dt_opt(extracted_at),
                attempts: attempts.max(0) as u32,
            },
            None => ExtractionCursor::unstarted(),
        })
    }

    /// Move (or clear) the extraction watermark.
    ///
    /// Same `updated_at` reasoning as [`set_derived_title`] and
    /// [`set_generated_title`], and it matters more here: this writer runs on
    /// EVERY window of every conversation in the backlog. If it stamped
    /// `updated_at`, the pass's own activity watcher would read its own
    /// bookkeeping as somebody coming back, cancel the pass, and push the idle
    /// clock forward -- so the engine would cancel itself mid-run, forever, and
    /// the idle gate that admitted it would never open again.
    async fn set_extraction_cursor(
        &self,
        session_id: &str,
        through_message_id: Option<&str>,
    ) -> Result<(), SessionStorageError> {
        match through_message_id {
            // A watermark that moved is a watermark nothing has failed against
            // yet, so the attempt count goes with it.
            Some(id) => sqlx::query(
                "UPDATE sessions SET extracted_through_id = ?, \
                     extracted_at = datetime('now'), extraction_attempts = 0 WHERE id = ?",
            )
            .bind(id)
            .bind(session_id),
            // Back to unstarted, stamp included: a conversation that has to be
            // re-walked from message one has not been examined, and keeping the
            // stamp would sort it to the back of a backlog it has not started.
            None => sqlx::query(
                "UPDATE sessions SET extracted_through_id = NULL, \
                 extracted_at = NULL, extraction_attempts = 0 WHERE id = ?",
            )
            .bind(session_id),
        }
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(())
    }

    async fn note_extraction_attempt(&self, session_id: &str) -> Result<u32, SessionStorageError> {
        // Increment and read back in one statement. Two statements would race
        // nothing today -- one lane slot, one writer -- but the RETURNING form
        // is the same cost and does not depend on that staying true.
        let attempts: Option<i64> = sqlx::query_scalar(
            "UPDATE sessions SET extraction_attempts = extraction_attempts + 1 \
             WHERE id = ? RETURNING extraction_attempts",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(attempts.unwrap_or(0).max(0) as u32)
    }

    async fn delete_session(&self, session_id: &str) -> Result<(), SessionStorageError> {
        // Files first: the CASCADE below erases the index rows, and without them
        // the bytes on disk are unreachable garbage.
        self.remove_session_attachment_files(session_id).await;
        // ON DELETE CASCADE handles session_messages automatically
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<Session>, SessionStorageError> {
        let rows = sqlx::query_as::<_, SessionRow>(
            "SELECT id, title, profile_id, total_prompt_tokens, total_completion_tokens, model_name, created_at, updated_at FROM sessions ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        rows.into_iter().map(Session::try_from).collect()
    }

    async fn get_messages_paginated(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        self.get_session(session_id).await?; // guard: session must exist

        let rows = sqlx::query_as::<_, MessageRow>(
            "SELECT id, session_id, role, content, tool_call_id, tool_calls_json, created_at, \
             prompt_tokens, completion_tokens, reasoning_tokens, liked \
             FROM session_messages \
             WHERE session_id = ? \
             ORDER BY created_at ASC, rowid ASC \
             LIMIT ? OFFSET ?",
        )
        .bind(session_id)
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        rows.into_iter().map(SessionMessage::try_from).collect()
    }

    async fn get_recent_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        self.get_session(session_id).await?; // guard: session must exist

        // Fetch newest-first, then reverse to return chronological order.
        let rows = sqlx::query_as::<_, MessageRow>(
            "SELECT id, session_id, role, content, tool_call_id, tool_calls_json, created_at, \
             prompt_tokens, completion_tokens, reasoning_tokens, liked \
             FROM session_messages \
             WHERE session_id = ? \
             ORDER BY created_at DESC, rowid DESC \
             LIMIT ?",
        )
        .bind(session_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        let mut messages: Vec<SessionMessage> = rows
            .into_iter()
            .map(SessionMessage::try_from)
            .collect::<Result<_, _>>()?;
        messages.reverse();
        Ok(messages)
    }

    async fn increment_usage(
        &self,
        session_id: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        model_name: Option<&str>,
    ) -> Result<(), SessionStorageError> {
        sqlx::query(
            "UPDATE sessions SET \
                total_prompt_tokens = total_prompt_tokens + ?, \
                total_completion_tokens = total_completion_tokens + ?, \
                model_name = COALESCE(?, model_name), \
                updated_at = datetime('now') \
             WHERE id = ?",
        )
        .bind(prompt_tokens as i64)
        .bind(completion_tokens as i64)
        .bind(model_name)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        Ok(())
    }

    async fn count_messages(&self, session_id: &str) -> Result<u64, SessionStorageError> {
        // Indexed COUNT(*) — cheap even for long conversations. Unlike the
        // read methods, this deliberately does NOT guard on session existence:
        // a missing session simply has zero messages, which is the answer the
        // sidebar badge wants.
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_messages WHERE session_id = ?")
                .bind(session_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(count.max(0) as u64)
    }

    async fn recent_reasoning_samples(
        &self,
        scan_limit: usize,
    ) -> Result<Vec<u32>, SessionStorageError> {
        // Walks `idx_session_messages_created_at` backwards and stops after
        // `scan_limit` ROWS -- not after that many samples. See the port docs:
        // bounding by result count would make a pond with thinking switched off
        // scan its whole history every turn to find nothing.
        //
        // `IS NOT NULL` is applied in SQL rather than in Rust so a row nobody
        // counted cannot arrive here as a zero. The distinction is migration
        // 0039's entire reason for having no DEFAULT on that column.
        let rows: Vec<(i64,)> = sqlx::query_as(
            "SELECT reasoning_tokens FROM ( \
                 SELECT reasoning_tokens FROM session_messages \
                 ORDER BY created_at DESC LIMIT ? \
             ) WHERE reasoning_tokens IS NOT NULL",
        )
        .bind(scan_limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|(tokens,)| u32::try_from(tokens).unwrap_or(0))
            .collect())
    }

    async fn first_user_message(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        // Earliest user-authored message, used only as a read-time title
        // fallback. Ordered identically to get_messages so "first" is stable.
        let content: Option<String> = sqlx::query_scalar(
            "SELECT content FROM session_messages \
             WHERE session_id = ? AND role = 'user' \
             ORDER BY created_at ASC, rowid ASC \
             LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(content)
    }

    async fn first_assistant_message(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        // Earliest assistant-authored message — the history card's preview.
        // Ordered identically to get_messages so "first" is stable, and
        // filtered to non-empty content because a turn that produced only tool
        // calls stores an empty assistant row, which would render as a card
        // with a blank body rather than no body.
        let content: Option<String> = sqlx::query_scalar(
            "SELECT content FROM session_messages \
             WHERE session_id = ? AND role = 'assistant' AND TRIM(content) <> '' \
             ORDER BY created_at ASC, rowid ASC \
             LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(content)
    }

    // ── Image attachments (phase F2) ────────────────────────────────────────

    async fn list_session_attachments(
        &self,
        session_id: &str,
    ) -> Result<Vec<MessageAttachment>, SessionStorageError> {
        let rows = sqlx::query_as::<_, AttachmentRow>(
            "SELECT id, message_id, session_id, ordinal, mime_type, byte_size, created_at \
             FROM message_attachments \
             WHERE session_id = ? \
             ORDER BY created_at ASC, ordinal ASC, rowid ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(rows.into_iter().map(MessageAttachment::from).collect())
    }

    async fn load_message_images(
        &self,
        message_ids: &[String],
    ) -> Result<HashMap<String, Vec<ImageAttachment>>, SessionStorageError> {
        if message_ids.is_empty() {
            return Ok(HashMap::new());
        }

        // Runtime sqlx has no array binding for SQLite, so the IN list is built
        // from placeholders — never from the ids themselves.
        let placeholders = std::iter::repeat_n("?", message_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT message_id, mime_type, file_path FROM message_attachments \
             WHERE message_id IN ({placeholders}) \
             ORDER BY message_id, ordinal ASC"
        );
        let mut query = sqlx::query(&sql);
        for id in message_ids {
            query = query.bind(id);
        }
        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        let mut out: HashMap<String, Vec<ImageAttachment>> = HashMap::new();
        for row in rows {
            let message_id: String = row.get("message_id");
            let mime_type: String = row.get("mime_type");
            let file_path: String = row.get("file_path");
            // A missing file degrades to "this image is gone" rather than an
            // error: the caller's fallback is a text placeholder, which is a
            // better outcome than failing the whole turn.
            let Some(bytes) = self.read_attachment_file(&file_path).await else {
                tracing::debug!(
                    message_id = %message_id,
                    path = %file_path,
                    "attachment bytes unavailable; will fall back to a text placeholder"
                );
                continue;
            };
            out.entry(message_id).or_default().push(ImageAttachment {
                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                mime_type,
            });
        }
        Ok(out)
    }

    async fn read_attachment(
        &self,
        attachment_id: &str,
    ) -> Result<Option<(String, Vec<u8>)>, SessionStorageError> {
        let row = sqlx::query("SELECT mime_type, file_path FROM message_attachments WHERE id = ?")
            .bind(attachment_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        let Some(row) = row else { return Ok(None) };
        let mime_type: String = row.get("mime_type");
        let file_path: String = row.get("file_path");
        Ok(self
            .read_attachment_file(&file_path)
            .await
            .map(|bytes| (mime_type, bytes)))
    }

    // ── Reasoning text (PAI-5 P6) ───────────────────────────────────────────

    async fn add_thinking(
        &self,
        session_id: &str,
        message_id: &str,
        blocks: &[String],
    ) -> Result<(), SessionStorageError> {
        if blocks.is_empty() {
            return Ok(());
        }
        for (idx, content) in blocks.iter().enumerate() {
            sqlx::query(
                "INSERT INTO session_thinking \
                     (id, message_id, session_id, block_index, content, created_at) \
                 VALUES (?, ?, ?, ?, ?, datetime('now'))",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(message_id)
            .bind(session_id)
            .bind(idx as i64)
            .bind(content)
            .execute(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;
        }
        Ok(())
    }

    async fn set_message_feedback(
        &self,
        session_id: &str,
        message_id: &str,
        liked: Option<bool>,
    ) -> Result<(), SessionStorageError> {
        let result =
            sqlx::query("UPDATE session_messages SET liked = ? WHERE id = ? AND session_id = ?")
                .bind(liked.map(|v| v as i64))
                .bind(message_id)
                .bind(session_id)
                .execute(&self.pool)
                .await
                .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(SessionStorageError::MessageNotFound(message_id.to_string()));
        }
        Ok(())
    }

    async fn get_thinking_for_session(
        &self,
        session_id: &str,
    ) -> Result<HashMap<String, Vec<String>>, SessionStorageError> {
        // `block_index` and not `created_at`: every block of one turn is
        // written inside the same `datetime('now')` second, so ordering by time
        // would shuffle the passages of a fast turn into an arbitrary order.
        let rows = sqlx::query(
            "SELECT message_id, content FROM session_thinking \
             WHERE session_id = ? \
             ORDER BY message_id, block_index ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            let message_id: String = row.get("message_id");
            let content: String = row.get("content");
            out.entry(message_id).or_default().push(content);
        }
        Ok(out)
    }

    async fn delete_messages_from(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<(), SessionStorageError> {
        // The attachments of the messages about to go, gathered BEFORE the
        // delete because afterwards there is no way to find them: the rows are
        // keyed to `message_id`, and nothing cascades. Without this an edited
        // turn leaves its `message_attachments` rows pointing at files that
        // belong to no message, and the files themselves on disk forever —
        // `remove_session_attachment_files` only runs when the whole session is
        // deleted, and it removes the entire directory, which is far too broad
        // here.
        let orphaned: Vec<String> = sqlx::query_scalar(
            "SELECT a.file_path FROM message_attachments a \
             JOIN session_messages m ON m.id = a.message_id \
             WHERE m.session_id = ?1 \
               AND m.rowid >= (SELECT rowid FROM session_messages WHERE id = ?2 AND session_id = ?1)",
        )
        .bind(session_id)
        .bind(message_id)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();

        sqlx::query(
            "DELETE FROM message_attachments \
             WHERE message_id IN ( \
                 SELECT id FROM session_messages \
                 WHERE session_id = ?1 \
                   AND rowid >= (SELECT rowid FROM session_messages WHERE id = ?2 AND session_id = ?1) \
             )",
        )
        .bind(session_id)
        .bind(message_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        // rowid, not created_at: two messages in the same turn can share a
        // second-resolution timestamp, and `>=` on created_at alone could
        // sweep up an earlier sibling row. rowid is SQLite's own insertion
        // order, so it is a stable tiebreaker — the same one get_messages()
        // and friends already use as `ORDER BY created_at ASC, rowid ASC`.
        let result = sqlx::query(
            "DELETE FROM session_messages \
             WHERE session_id = ?1 \
               AND rowid >= (SELECT rowid FROM session_messages WHERE id = ?2 AND session_id = ?1)",
        )
        .bind(session_id)
        .bind(message_id)
        .execute(&self.pool)
        .await
        .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        // Files last, and best-effort: a file that fails to unlink is wasted
        // disk, whereas a row that outlives its message is a broken reference
        // the attachment route can still be asked for.
        for path in orphaned {
            if let Err(e) = tokio::fs::remove_file(&path).await {
                tracing::warn!(
                    path = %path,
                    error = %e,
                    "could not remove the attachment file of a truncated message"
                );
            }
        }

        if result.rows_affected() == 0 {
            return Err(SessionStorageError::MessageNotFound(message_id.to_string()));
        }

        sqlx::query("UPDATE sessions SET updated_at = datetime('now') WHERE id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| SessionStorageError::StorageError(e.to_string()))?;

        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use pond_core::models::domain::message::ChatMessage;
    use tempfile::tempdir;

    async fn make_storage() -> (SqliteSessionStorage, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        // Attachment bytes must land in the temp dir, never in the developer's
        // real profile (the production default resolves the OS data dir).
        let storage = SqliteSessionStorage::new(db.system)
            .with_attachment_dir(tmp.path().join("attachments"));
        (storage, tmp)
    }

    #[tokio::test]
    async fn create_and_get_session() {
        let (s, _tmp) = make_storage().await;
        let session = s.create_session("sess-1".to_string()).await.unwrap();
        assert_eq!(session.id, "sess-1");
        let fetched = s.get_session("sess-1").await.unwrap();
        assert_eq!(fetched.id, "sess-1");
    }

    /// C1: the engine-session pairing must survive a process restart, which is
    /// what a fresh storage handle over the same file simulates.
    #[tokio::test]
    async fn engine_session_pairing_persists_and_upserts() {
        let tmp = tempdir().unwrap();
        {
            let db = Database::init(tmp.path()).await.unwrap();
            let s = SqliteSessionStorage::new(db.system);
            assert_eq!(s.get_engine_session_id("sess-1").await.unwrap(), None);
            // No sessions row on purpose — the pairing must not require one.
            s.set_engine_session_id("sess-1", "20260728_1")
                .await
                .unwrap();
            assert_eq!(
                s.get_engine_session_id("sess-1").await.unwrap().as_deref(),
                Some("20260728_1")
            );
            // Re-pairing (engine store wiped) overwrites rather than erroring.
            s.set_engine_session_id("sess-1", "20260728_9")
                .await
                .unwrap();
            assert_eq!(
                s.get_engine_session_id("sess-1").await.unwrap().as_deref(),
                Some("20260728_9")
            );
        }
        let db = Database::init(tmp.path()).await.unwrap();
        let reopened = SqliteSessionStorage::new(db.system);
        assert_eq!(
            reopened
                .get_engine_session_id("sess-1")
                .await
                .unwrap()
                .as_deref(),
            Some("20260728_9"),
            "pairing must survive a restart"
        );
        assert_eq!(reopened.get_engine_session_id("other").await.unwrap(), None);
    }

    /// PAI-2 P1: a builtin MCP tool call carries the ENGINE's session id, so a
    /// draft decision has to walk this map backwards to find a speaker.
    #[tokio::test]
    async fn the_engine_session_reverse_lookup_finds_the_giap_session() {
        let (s, _tmp) = make_storage().await;
        s.set_engine_session_id("giap-1", "20260805_4")
            .await
            .unwrap();
        assert_eq!(
            s.get_session_id_for_engine("20260805_4").await.unwrap(),
            Some("giap-1".to_string())
        );
        assert_eq!(s.get_session_id_for_engine("nope").await.unwrap(), None);
        assert_eq!(
            s.get_session_id_for_engine("").await.unwrap(),
            None,
            "a blank engine id must not match a row"
        );
    }

    #[tokio::test]
    async fn get_nonexistent_session_returns_error() {
        let (s, _tmp) = make_storage().await;
        let result = s.get_session("missing").await;
        assert!(matches!(
            result,
            Err(SessionStorageError::SessionNotFound(_))
        ));
    }

    #[tokio::test]
    async fn add_and_retrieve_messages() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        let msg = SessionMessage::new(
            "m1".to_string(),
            "sess-1".to_string(),
            ChatMessage::user("Hello"),
        );
        s.add_message("sess-1".to_string(), msg).await.unwrap();
        let msgs = s.get_messages("sess-1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].message.content, "Hello");
        assert_eq!(msgs[0].message.role, Role::User);
    }

    #[tokio::test]
    async fn messages_preserve_insertion_order() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m1".to_string(),
                "sess-1".to_string(),
                ChatMessage::user("First"),
            ),
        )
        .await
        .unwrap();
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m2".to_string(),
                "sess-1".to_string(),
                ChatMessage::assistant("Second"),
            ),
        )
        .await
        .unwrap();
        let msgs = s.get_messages("sess-1").await.unwrap();
        assert_eq!(msgs[0].message.content, "First");
        assert_eq!(msgs[1].message.content, "Second");
    }

    #[tokio::test]
    async fn delete_session_cascades_to_messages() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m1".to_string(),
                "sess-1".to_string(),
                ChatMessage::user("Hi"),
            ),
        )
        .await
        .unwrap();
        s.delete_session("sess-1").await.unwrap();
        assert!(matches!(
            s.get_session("sess-1").await,
            Err(SessionStorageError::SessionNotFound(_))
        ));
    }

    #[tokio::test]
    async fn new_messages_default_to_no_feedback() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m1".to_string(),
                "sess-1".to_string(),
                ChatMessage::assistant("Hi"),
            ),
        )
        .await
        .unwrap();
        let msgs = s.get_messages("sess-1").await.unwrap();
        assert_eq!(msgs[0].liked, None);
    }

    #[tokio::test]
    async fn feedback_round_trips_like_dislike_and_clear() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m1".to_string(),
                "sess-1".to_string(),
                ChatMessage::assistant("Hi"),
            ),
        )
        .await
        .unwrap();

        s.set_message_feedback("sess-1", "m1", Some(true))
            .await
            .unwrap();
        assert_eq!(s.get_messages("sess-1").await.unwrap()[0].liked, Some(true));

        s.set_message_feedback("sess-1", "m1", Some(false))
            .await
            .unwrap();
        assert_eq!(
            s.get_messages("sess-1").await.unwrap()[0].liked,
            Some(false)
        );

        s.set_message_feedback("sess-1", "m1", None).await.unwrap();
        assert_eq!(s.get_messages("sess-1").await.unwrap()[0].liked, None);
    }

    #[tokio::test]
    async fn feedback_on_missing_message_errors() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        assert!(matches!(
            s.set_message_feedback("sess-1", "missing", Some(true))
                .await,
            Err(SessionStorageError::MessageNotFound(_))
        ));
    }

    #[tokio::test]
    async fn delete_messages_from_removes_the_target_and_everything_after() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        for (id, text) in [("m1", "First"), ("m2", "Second"), ("m3", "Third")] {
            s.add_message(
                "sess-1".to_string(),
                SessionMessage::new(
                    id.to_string(),
                    "sess-1".to_string(),
                    ChatMessage::user(text),
                ),
            )
            .await
            .unwrap();
        }

        s.delete_messages_from("sess-1", "m2").await.unwrap();

        let msgs = s.get_messages("sess-1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, "m1");
    }

    /// "Refresh" keeps the user's message and only drops the reply after it —
    /// this is what makes that possible: truncating from the assistant
    /// message's id leaves every earlier message, including its own user
    /// prompt, untouched.
    #[tokio::test]
    async fn delete_messages_from_the_assistant_reply_keeps_the_user_prompt() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m1".to_string(),
                "sess-1".to_string(),
                ChatMessage::user("Question"),
            ),
        )
        .await
        .unwrap();
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m2".to_string(),
                "sess-1".to_string(),
                ChatMessage::assistant("Answer"),
            ),
        )
        .await
        .unwrap();

        s.delete_messages_from("sess-1", "m2").await.unwrap();

        let msgs = s.get_messages("sess-1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, "m1");
    }

    #[tokio::test]
    async fn delete_messages_from_missing_message_errors() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        assert!(matches!(
            s.delete_messages_from("sess-1", "missing").await,
            Err(SessionStorageError::MessageNotFound(_))
        ));
    }

    #[tokio::test]
    async fn add_message_to_missing_session_errors() {
        let (s, _tmp) = make_storage().await;
        let msg = SessionMessage::new(
            "m1".to_string(),
            "no-session".to_string(),
            ChatMessage::user("Hi"),
        );
        let result = s.add_message("no-session".to_string(), msg).await;
        assert!(matches!(
            result,
            Err(SessionStorageError::SessionNotFound(_))
        ));
    }

    #[tokio::test]
    async fn list_sessions_returns_all_ordered() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-a".to_string()).await.unwrap();
        s.create_session("sess-b".to_string()).await.unwrap();
        // Add a message to sess-a to update its updated_at
        s.add_message(
            "sess-a".to_string(),
            SessionMessage::new(
                "m1".to_string(),
                "sess-a".to_string(),
                ChatMessage::user("Hi"),
            ),
        )
        .await
        .unwrap();

        let sessions = s.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 2);
        // sess-a was updated more recently, so it should be first
        assert_eq!(sessions[0].id, "sess-a");
        assert_eq!(sessions[1].id, "sess-b");
    }

    #[tokio::test]
    async fn get_messages_paginated_works() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        for i in 0..10 {
            s.add_message(
                "sess-1".to_string(),
                SessionMessage::new(
                    format!("m{}", i),
                    "sess-1".to_string(),
                    ChatMessage::user(format!("Msg {}", i)),
                ),
            )
            .await
            .unwrap();
        }

        let page = s.get_messages_paginated("sess-1", 3, 0).await.unwrap();
        assert_eq!(page.len(), 3);
        assert_eq!(page[0].message.content, "Msg 0");

        let page2 = s.get_messages_paginated("sess-1", 3, 7).await.unwrap();
        assert_eq!(page2.len(), 3);
        assert_eq!(page2[0].message.content, "Msg 7");

        let past_end = s.get_messages_paginated("sess-1", 5, 100).await.unwrap();
        assert!(past_end.is_empty());
    }

    #[tokio::test]
    async fn session_title_defaults_to_none() {
        let (s, _tmp) = make_storage().await;
        let session = s.create_session("sess-1".to_string()).await.unwrap();
        assert_eq!(session.title, None);
    }

    #[tokio::test]
    async fn update_title_sets_and_persists() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();

        s.update_title("sess-1", "Weather Chat".to_string())
            .await
            .unwrap();
        let session = s.get_session("sess-1").await.unwrap();
        assert_eq!(session.title, Some("Weather Chat".to_string()));
    }

    // ── Title provenance (migration 0049) ───────────────────────────────────

    #[tokio::test]
    async fn each_title_writer_stamps_its_own_provenance() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();

        // A brand new row knows nothing about who named it.
        assert_eq!(
            s.get_title_provenance("sess-1").await.unwrap(),
            (None, None)
        );

        s.set_derived_title("sess-1", "so i was wondering whether")
            .await
            .unwrap();
        assert_eq!(
            s.get_title_provenance("sess-1").await.unwrap(),
            (Some("derived".to_string()), None)
        );

        s.set_generated_title("sess-1", "Wake word fires twice", "msg-9")
            .await
            .unwrap();
        assert_eq!(
            s.get_title_provenance("sess-1").await.unwrap(),
            (Some("model".to_string()), Some("msg-9".to_string()))
        );
        assert_eq!(
            s.get_session("sess-1").await.unwrap().title.as_deref(),
            Some("Wake word fires twice")
        );

        // A human rename outranks everything, and clears the reach marker so a
        // stale one can never be read as covering the new name.
        s.update_title("sess-1", "Jetson deploy notes".to_string())
            .await
            .unwrap();
        assert_eq!(
            s.get_title_provenance("sess-1").await.unwrap(),
            (Some("user".to_string()), None)
        );
    }

    /// The invariant that keeps the background job from sabotaging itself.
    ///
    /// `sessions.updated_at` is one of the two activity sources the idle gate
    /// reads. If a background rename stamped it, the job would look exactly
    /// like a person coming back: its own watcher would cancel the sweep
    /// partway through, and every pass would shove the idle clock forward.
    /// A human rename is real activity and *should* bump it.
    #[tokio::test]
    async fn background_title_writes_are_not_mistaken_for_user_activity() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        let before = s.get_session("sess-1").await.unwrap().updated_at;

        // SQLite's datetime('now') has one-second resolution, so without this
        // a bump inside the same second would be invisible and the test would
        // pass against code that does bump.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        s.set_derived_title("sess-1", "so i was wondering whether")
            .await
            .unwrap();
        assert_eq!(
            s.get_session("sess-1").await.unwrap().updated_at,
            before,
            "the deterministic fallback must not read as user activity"
        );

        s.set_generated_title("sess-1", "Wake word fires twice", "msg-9")
            .await
            .unwrap();
        assert_eq!(
            s.get_session("sess-1").await.unwrap().updated_at,
            before,
            "a background rename must not read as user activity"
        );

        s.update_title("sess-1", "Jetson deploy notes".to_string())
            .await
            .unwrap();
        assert!(
            s.get_session("sess-1").await.unwrap().updated_at > before,
            "a person renaming a conversation IS activity"
        );
    }

    /// The extraction cursor round-trips, and clearing it really does mean
    /// unstarted rather than "examined, found nothing".
    ///
    /// The two states are told apart by the STAMP, not by the id: the backlog
    /// is ordered `extracted_at IS NULL` first, so a cleared cursor that kept
    /// its stamp would sort a conversation that has to be re-walked from
    /// message one to the back of a queue it has not started.
    #[tokio::test]
    async fn the_extraction_cursor_round_trips_and_clears_to_unstarted() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();

        let fresh = s.extraction_cursor("sess-1").await.unwrap();
        assert_eq!(fresh, ExtractionCursor::unstarted());

        s.set_extraction_cursor("sess-1", Some("msg-7"))
            .await
            .unwrap();
        let moved = s.extraction_cursor("sess-1").await.unwrap();
        assert_eq!(moved.through_message_id.as_deref(), Some("msg-7"));
        assert!(
            moved.extracted_at.is_some(),
            "a watermark that moved carries the time it moved"
        );
        assert_eq!(moved.attempts, 0);

        s.set_extraction_cursor("sess-1", None).await.unwrap();
        assert_eq!(
            s.extraction_cursor("sess-1").await.unwrap(),
            ExtractionCursor::unstarted(),
            "clearing the cursor must clear the stamp with it"
        );

        // A session that does not exist has not been examined. That is a real
        // answer, not a missing one, and the walk depends on getting it rather
        // than an error it would have to decide what to do with.
        assert_eq!(
            s.extraction_cursor("never-existed").await.unwrap(),
            ExtractionCursor::unstarted()
        );
    }

    /// Attempts count against the watermark, and moving the watermark clears
    /// them.
    ///
    /// The give-up rung depends on both halves. Without the increment a model
    /// that never emits parseable JSON re-reads one window forever; without the
    /// reset, three failures anywhere in a conversation's past would
    /// permanently disqualify it.
    #[tokio::test]
    async fn extraction_attempts_count_against_the_watermark_and_reset_when_it_moves() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();

        assert_eq!(s.note_extraction_attempt("sess-1").await.unwrap(), 1);
        assert_eq!(s.note_extraction_attempt("sess-1").await.unwrap(), 2);
        assert_eq!(s.extraction_cursor("sess-1").await.unwrap().attempts, 2);

        // An attempt must NOT move the watermark: the window was read, not
        // examined, and advancing past it would lose a real conversation to a
        // parser failure.
        assert_eq!(
            s.extraction_cursor("sess-1")
                .await
                .unwrap()
                .through_message_id,
            None
        );

        s.set_extraction_cursor("sess-1", Some("msg-3"))
            .await
            .unwrap();
        assert_eq!(s.extraction_cursor("sess-1").await.unwrap().attempts, 0);

        // A missing session reports zero rather than erroring, so the give-up
        // rung reads "no failures here" for a conversation that has been
        // deleted underneath the walk.
        assert_eq!(s.note_extraction_attempt("gone").await.unwrap(), 0);
    }

    /// The same invariant the title writers carry, on the writer that runs most
    /// often.
    ///
    /// `sessions.updated_at` is one of the two activity sources the idle gate
    /// reads. The extraction cursor is written once per window of every
    /// conversation in the backlog, so if it stamped that column the pass's own
    /// watcher would read its bookkeeping as somebody coming back, cancel the
    /// pass mid-run, and shove the idle clock forward -- every time, forever.
    #[tokio::test]
    async fn extraction_cursor_writes_are_not_mistaken_for_user_activity() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        let before = s.get_session("sess-1").await.unwrap().updated_at;

        // SQLite's datetime('now') has one-second resolution, so without this
        // a bump inside the same second would be invisible and the test would
        // pass against code that does bump.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        s.set_extraction_cursor("sess-1", Some("msg-4"))
            .await
            .unwrap();
        assert_eq!(
            s.get_session("sess-1").await.unwrap().updated_at,
            before,
            "advancing the extraction watermark must not read as user activity"
        );

        s.note_extraction_attempt("sess-1").await.unwrap();
        assert_eq!(
            s.get_session("sess-1").await.unwrap().updated_at,
            before,
            "a failed extraction attempt must not read as user activity either"
        );

        s.set_extraction_cursor("sess-1", None).await.unwrap();
        assert_eq!(
            s.get_session("sess-1").await.unwrap().updated_at,
            before,
            "clearing the cursor must not read as user activity either"
        );

        // Vacuity control: this storage really does bump `updated_at` when
        // something real happens, so the three assertions above are decisions
        // and not a column nobody writes.
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m1".to_string(),
                "sess-1".to_string(),
                ChatMessage::user("hello".to_string()),
            ),
        )
        .await
        .unwrap();
        assert!(
            s.get_session("sess-1").await.unwrap().updated_at > before,
            "somebody sending a message IS activity"
        );
    }

    /// The cheap answer must agree with the expensive one, because the idle
    /// re-titling pass trusts it to decide whether a conversation has outgrown
    /// its name — and a wrong answer either freezes a stale title forever or
    /// burns the inference slot renaming something that has not changed.
    #[tokio::test]
    async fn messages_after_agrees_with_walking_the_history() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        for i in 0..10 {
            s.add_message(
                "sess-1".to_string(),
                SessionMessage::new(
                    format!("m{i}"),
                    "sess-1".to_string(),
                    ChatMessage::user(format!("message {i}")),
                ),
            )
            .await
            .unwrap();
        }

        let history = s.get_messages("sess-1").await.unwrap();
        for (i, msg) in history.iter().enumerate() {
            let expected = (history.len() - i - 1) as u64;
            assert_eq!(
                s.messages_after("sess-1", &msg.id).await.unwrap(),
                Some(expected),
                "disagreed at index {i}"
            );
        }
        // The newest message has nothing after it — which is NOT the same
        // answer as an anchor that no longer exists.
        assert_eq!(s.messages_after("sess-1", "m9").await.unwrap(), Some(0));
        assert_eq!(s.messages_after("sess-1", "gone").await.unwrap(), None);
        // An anchor belonging to a different conversation is not this one's.
        s.create_session("sess-2".to_string()).await.unwrap();
        assert_eq!(s.messages_after("sess-2", "m0").await.unwrap(), None);
    }

    /// The card preview is the pond's answer, so an assistant row that carries
    /// no words — a turn that only called tools — must not win the slot and
    /// render a card with a blank body.
    #[tokio::test]
    async fn first_assistant_message_skips_a_wordless_turn() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();

        let rows = [
            ("m0", ChatMessage::user("what time is it")),
            ("m1", ChatMessage::assistant("")),
            ("m2", ChatMessage::assistant("   ")),
            ("m3", ChatMessage::assistant("It is just past nine.")),
        ];
        for (id, msg) in rows {
            s.add_message(
                "sess-1".to_string(),
                SessionMessage::new(id.to_string(), "sess-1".to_string(), msg),
            )
            .await
            .unwrap();
        }

        assert_eq!(
            s.first_assistant_message("sess-1")
                .await
                .unwrap()
                .as_deref(),
            Some("It is just past nine."),
        );
        // The user's opening line still belongs to the title fallback.
        assert_eq!(
            s.first_user_message("sess-1").await.unwrap().as_deref(),
            Some("what time is it"),
        );
    }

    #[tokio::test]
    async fn first_assistant_message_is_none_before_a_reply_exists() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new("m0".into(), "sess-1".into(), ChatMessage::user("hello")),
        )
        .await
        .unwrap();

        assert_eq!(s.first_assistant_message("sess-1").await.unwrap(), None);
        assert_eq!(s.first_assistant_message("missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn provenance_writers_guard_against_a_missing_session() {
        let (s, _tmp) = make_storage().await;
        assert!(matches!(
            s.set_derived_title("missing", "x").await,
            Err(SessionStorageError::SessionNotFound(_))
        ));
        assert!(matches!(
            s.set_generated_title("missing", "x", "msg-1").await,
            Err(SessionStorageError::SessionNotFound(_))
        ));
        // Reading provenance for a session that is not there is a question with
        // a sensible answer, not an error.
        assert_eq!(
            s.get_title_provenance("missing").await.unwrap(),
            (None, None)
        );
    }

    #[tokio::test]
    async fn update_title_on_missing_session_errors() {
        let (s, _tmp) = make_storage().await;
        let result = s.update_title("missing", "Nope".to_string()).await;
        assert!(matches!(
            result,
            Err(SessionStorageError::SessionNotFound(_))
        ));
    }

    #[tokio::test]
    async fn get_recent_messages_returns_newest_in_order() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        for i in 0..10 {
            s.add_message(
                "sess-1".to_string(),
                SessionMessage::new(
                    format!("m{}", i),
                    "sess-1".to_string(),
                    ChatMessage::user(format!("Msg {}", i)),
                ),
            )
            .await
            .unwrap();
        }

        // Ask for 3 most recent → should get Msg 7, 8, 9 in chronological order
        let recent = s.get_recent_messages("sess-1", 3).await.unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].message.content, "Msg 7");
        assert_eq!(recent[1].message.content, "Msg 8");
        assert_eq!(recent[2].message.content, "Msg 9");
    }

    #[tokio::test]
    async fn get_recent_messages_limit_exceeds_count_returns_all() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        for i in 0..3 {
            s.add_message(
                "sess-1".to_string(),
                SessionMessage::new(
                    format!("m{}", i),
                    "sess-1".to_string(),
                    ChatMessage::user(format!("Msg {}", i)),
                ),
            )
            .await
            .unwrap();
        }

        let recent = s.get_recent_messages("sess-1", 100).await.unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].message.content, "Msg 0");
    }

    #[tokio::test]
    async fn tool_call_metadata_round_trips() {
        use pond_core::models::domain::message::ToolCallRecord;
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();

        let asst = SessionMessage::new(
            "m-asst".to_string(),
            "sess-1".to_string(),
            ChatMessage::assistant_with_tool_calls(
                "calling weather",
                vec![ToolCallRecord {
                    id: "call-42".to_string(),
                    name: "get_weather".to_string(),
                    arguments: "{\"city\":\"Nairobi\"}".to_string(),
                }],
            ),
        );
        s.add_message("sess-1".to_string(), asst).await.unwrap();

        let tool = SessionMessage::new(
            "m-tool".to_string(),
            "sess-1".to_string(),
            ChatMessage::tool_result("sunny, 24C", "call-42"),
        );
        s.add_message("sess-1".to_string(), tool).await.unwrap();

        let msgs = s.get_messages("sess-1").await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].message.tool_calls.len(), 1);
        assert_eq!(msgs[0].message.tool_calls[0].id, "call-42");
        assert_eq!(msgs[0].message.tool_calls[0].name, "get_weather");
        assert_eq!(msgs[1].message.role, Role::Tool);
        assert_eq!(msgs[1].message.tool_call_id.as_deref(), Some("call-42"));
    }

    #[tokio::test]
    async fn count_messages_reflects_stored_rows() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        assert_eq!(s.count_messages("sess-1").await.unwrap(), 0);

        for i in 0..5 {
            s.add_message(
                "sess-1".to_string(),
                SessionMessage::new(
                    format!("m{}", i),
                    "sess-1".to_string(),
                    ChatMessage::user(format!("Msg {}", i)),
                ),
            )
            .await
            .unwrap();
        }
        assert_eq!(s.count_messages("sess-1").await.unwrap(), 5);
    }

    #[tokio::test]
    async fn count_messages_missing_session_is_zero() {
        let (s, _tmp) = make_storage().await;
        // No error, no session — just zero rows.
        assert_eq!(s.count_messages("nope").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn first_user_message_returns_earliest_user_row() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        // No user messages yet.
        assert_eq!(s.first_user_message("sess-1").await.unwrap(), None);

        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m0".to_string(),
                "sess-1".to_string(),
                ChatMessage::assistant("greeting"),
            ),
        )
        .await
        .unwrap();
        // Still no *user* message.
        assert_eq!(s.first_user_message("sess-1").await.unwrap(), None);

        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m1".to_string(),
                "sess-1".to_string(),
                ChatMessage::user("What is the weather in Nairobi today?"),
            ),
        )
        .await
        .unwrap();
        s.add_message(
            "sess-1".to_string(),
            SessionMessage::new(
                "m2".to_string(),
                "sess-1".to_string(),
                ChatMessage::user("second question"),
            ),
        )
        .await
        .unwrap();

        assert_eq!(
            s.first_user_message("sess-1").await.unwrap(),
            Some("What is the weather in Nairobi today?".to_string())
        );
    }

    #[tokio::test]
    async fn messages_survive_restart() {
        let tmp = tempdir().unwrap();
        // First run: write
        {
            let db = Database::init(tmp.path()).await.unwrap();
            let s = SqliteSessionStorage::new(db.system);
            s.create_session("persistent".to_string()).await.unwrap();
            s.add_message(
                "persistent".to_string(),
                SessionMessage::new(
                    "m1".to_string(),
                    "persistent".to_string(),
                    ChatMessage::user("Remember me"),
                ),
            )
            .await
            .unwrap();
        }
        // Second run: read back
        {
            let db = Database::init(tmp.path()).await.unwrap();
            let s = SqliteSessionStorage::new(db.system);
            let msgs = s.get_messages("persistent").await.unwrap();
            assert_eq!(msgs.len(), 1);
            assert_eq!(msgs[0].message.content, "Remember me");
        }
    }

    // ── Image attachments (phase F2) ────────────────────────────────────────

    /// A 1x1 red PNG, base64. Small enough to inline, real enough that a decode
    /// failure would show up.
    const TINY_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==";

    async fn seed_image_message(
        s: &SqliteSessionStorage,
        session: &str,
        message_id: &str,
        images: Vec<ImageAttachment>,
    ) {
        s.add_message(
            session.to_string(),
            SessionMessage::new(
                message_id.to_string(),
                session.to_string(),
                ChatMessage::user_with_images("what is this?", images),
            ),
        )
        .await
        .unwrap();
    }

    /// Truncating a turn takes its attachments with it -- rows AND files.
    ///
    /// Nothing cascades from `session_messages` to `message_attachments`, and
    /// the only file cleanup in this module removes the whole session
    /// directory, which runs only on a full session delete. So an edited turn
    /// used to leave rows pointing at a message that no longer exists, and the
    /// image bytes on disk forever -- on a Jetson, where the disk is the thing
    /// that runs out.
    #[tokio::test]
    async fn truncating_a_message_removes_its_attachments_and_their_files() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-img".to_string()).await.unwrap();
        seed_image_message(&s, "sess-img", "keep", vec![png(TINY_PNG)]).await;
        seed_image_message(&s, "sess-img", "drop", vec![png(TINY_PNG)]).await;

        let before = s.list_session_attachments("sess-img").await.unwrap();
        assert_eq!(before.len(), 2, "both turns have an attachment to start");
        let doomed: Vec<String> =
            sqlx::query_scalar("SELECT file_path FROM message_attachments WHERE message_id = ?")
                .bind("drop")
                .fetch_all(&s.pool)
                .await
                .unwrap();
        assert_eq!(doomed.len(), 1);
        assert!(
            std::path::Path::new(&doomed[0]).exists(),
            "the fixture never wrote the file, so this test would pass vacuously"
        );

        s.delete_messages_from("sess-img", "drop").await.unwrap();

        let after = s.list_session_attachments("sess-img").await.unwrap();
        assert_eq!(
            after.len(),
            1,
            "the truncated message left its attachment row behind, pointing at \
             a message that no longer exists"
        );
        assert!(
            !std::path::Path::new(&doomed[0]).exists(),
            "the attachment row went but its file is still on disk"
        );

        // The surviving turn is untouched: this must delete forward, not all.
        let kept: Vec<String> =
            sqlx::query_scalar("SELECT file_path FROM message_attachments WHERE message_id = ?")
                .bind("keep")
                .fetch_all(&s.pool)
                .await
                .unwrap();
        assert_eq!(kept.len(), 1);
        assert!(std::path::Path::new(&kept[0]).exists());
    }

    fn png(data: &str) -> ImageAttachment {
        ImageAttachment {
            data: data.to_string(),
            mime_type: "image/png".to_string(),
        }
    }

    #[tokio::test]
    async fn attachments_round_trip_bytes_and_metadata() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-img".to_string()).await.unwrap();
        seed_image_message(&s, "sess-img", "m1", vec![png(TINY_PNG)]).await;

        let meta = s.list_session_attachments("sess-img").await.unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].message_id, "m1");
        assert_eq!(meta[0].session_id, "sess-img");
        assert_eq!(meta[0].ordinal, 0);
        assert_eq!(meta[0].mime_type, "image/png");
        // 1x1 PNG is 70 bytes; assert it is the decoded length, not the base64.
        assert!(meta[0].byte_size > 0 && meta[0].byte_size < TINY_PNG.len() as u64);

        let loaded = s.load_message_images(&["m1".to_string()]).await.unwrap();
        let images = loaded.get("m1").expect("m1 has images");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].mime_type, "image/png");
        assert_eq!(images[0].data, TINY_PNG, "base64 must round-trip exactly");

        let (mime, bytes) = s.read_attachment(&meta[0].id).await.unwrap().unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes.len() as u64, meta[0].byte_size);
    }

    /// Ordinal order is the user's pick order, and must survive the round trip —
    /// "the first picture" has to mean the same thing on a follow-up turn.
    #[tokio::test]
    async fn attachment_order_is_preserved() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-order".to_string()).await.unwrap();
        // Three distinguishable payloads (differing base64 lengths).
        let a = png(TINY_PNG);
        let b = ImageAttachment {
            data: "aGVsbG8=".to_string(),
            mime_type: "image/jpeg".to_string(),
        };
        let c = ImageAttachment {
            data: "aGVsbG8gd29ybGQ=".to_string(),
            mime_type: "image/webp".to_string(),
        };
        seed_image_message(&s, "sess-order", "m1", vec![a, b, c]).await;

        let meta = s.list_session_attachments("sess-order").await.unwrap();
        assert_eq!(
            meta.iter().map(|m| m.ordinal).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            meta.iter()
                .map(|m| m.mime_type.as_str())
                .collect::<Vec<_>>(),
            vec!["image/png", "image/jpeg", "image/webp"]
        );
        let loaded = s.load_message_images(&["m1".to_string()]).await.unwrap();
        assert_eq!(
            loaded["m1"]
                .iter()
                .map(|i| i.mime_type.as_str())
                .collect::<Vec<_>>(),
            vec!["image/png", "image/jpeg", "image/webp"]
        );
    }

    #[tokio::test]
    async fn attachments_survive_restart() {
        let tmp = tempdir().unwrap();
        let attach_dir = tmp.path().join("attachments");
        {
            let db = Database::init(tmp.path()).await.unwrap();
            let s = SqliteSessionStorage::new(db.system).with_attachment_dir(attach_dir.clone());
            s.create_session("sess-restart".to_string()).await.unwrap();
            seed_image_message(&s, "sess-restart", "m1", vec![png(TINY_PNG)]).await;
        }
        {
            let db = Database::init(tmp.path()).await.unwrap();
            let s = SqliteSessionStorage::new(db.system).with_attachment_dir(attach_dir);
            let loaded = s.load_message_images(&["m1".to_string()]).await.unwrap();
            assert_eq!(loaded["m1"][0].data, TINY_PNG);
        }
    }

    /// Loading images must never fail a turn just because a file went missing —
    /// the caller's fallback is a text placeholder, which is strictly better than
    /// erroring out the whole conversation.
    #[tokio::test]
    async fn a_missing_attachment_file_degrades_instead_of_erroring() {
        let (s, tmp) = make_storage().await;
        s.create_session("sess-gone".to_string()).await.unwrap();
        seed_image_message(&s, "sess-gone", "m1", vec![png(TINY_PNG)]).await;

        // Simulate the bytes disappearing under us (disk cleanup, restore).
        let dir = tmp.path().join("attachments").join("sess-gone");
        for entry in std::fs::read_dir(&dir).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }

        let loaded = s.load_message_images(&["m1".to_string()]).await.unwrap();
        assert!(!loaded.contains_key("m1"), "no pixels, no entry");
        // The index row is still there, so the UI can still say an image existed.
        assert_eq!(
            s.list_session_attachments("sess-gone").await.unwrap().len(),
            1
        );
        let meta = s.list_session_attachments("sess-gone").await.unwrap();
        assert!(s.read_attachment(&meta[0].id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn undecodable_base64_is_skipped_without_failing_the_message() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-bad".to_string()).await.unwrap();
        seed_image_message(
            &s,
            "sess-bad",
            "m1",
            vec![
                ImageAttachment {
                    data: "!!!not base64!!!".to_string(),
                    mime_type: "image/png".to_string(),
                },
                png(TINY_PNG),
            ],
        )
        .await;

        // The message itself persisted.
        assert_eq!(s.get_messages("sess-bad").await.unwrap().len(), 1);
        // Only the decodable image was stored, and it kept its ordinal.
        let meta = s.list_session_attachments("sess-bad").await.unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].ordinal, 1);
    }

    #[tokio::test]
    async fn a_data_url_prefix_is_tolerated() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-dataurl".to_string()).await.unwrap();
        seed_image_message(
            &s,
            "sess-dataurl",
            "m1",
            vec![ImageAttachment {
                data: format!("data:image/png;base64,{TINY_PNG}"),
                mime_type: "image/png".to_string(),
            }],
        )
        .await;
        let loaded = s.load_message_images(&["m1".to_string()]).await.unwrap();
        assert_eq!(loaded["m1"][0].data, TINY_PNG);
    }

    #[tokio::test]
    async fn deleting_a_session_removes_its_attachment_files() {
        let (s, tmp) = make_storage().await;
        s.create_session("sess-del".to_string()).await.unwrap();
        seed_image_message(&s, "sess-del", "m1", vec![png(TINY_PNG)]).await;
        let dir = tmp.path().join("attachments").join("sess-del");
        assert!(dir.exists());

        s.delete_session("sess-del").await.unwrap();
        assert!(!dir.exists(), "orphaned bytes must not survive the session");
    }

    /// A caller-supplied session id must never become a path.
    #[tokio::test]
    async fn a_traversal_session_id_cannot_escape_the_attachment_root() {
        let (s, tmp) = make_storage().await;
        let nasty = "../../escaped";
        s.create_session(nasty.to_string()).await.unwrap();
        seed_image_message(&s, nasty, "m1", vec![png(TINY_PNG)]).await;

        let root = tmp.path().join("attachments");
        let meta = s.list_session_attachments(nasty).await.unwrap();
        assert_eq!(meta.len(), 1);
        // Whatever path was chosen, it is inside the root.
        let stored: String =
            sqlx::query_scalar("SELECT file_path FROM message_attachments WHERE id = ?")
                .bind(&meta[0].id)
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert!(
            Path::new(&stored).starts_with(&root),
            "attachment escaped the root: {stored}"
        );
    }

    #[tokio::test]
    async fn a_text_only_message_writes_no_attachment_rows_or_files() {
        let (s, tmp) = make_storage().await;
        s.create_session("sess-text".to_string()).await.unwrap();
        s.add_message(
            "sess-text".to_string(),
            SessionMessage::new(
                "m1".to_string(),
                "sess-text".to_string(),
                ChatMessage::user("no pictures here"),
            ),
        )
        .await
        .unwrap();
        assert!(s
            .list_session_attachments("sess-text")
            .await
            .unwrap()
            .is_empty());
        // Not even the per-session directory is created.
        assert!(!tmp.path().join("attachments").join("sess-text").exists());
    }

    #[tokio::test]
    async fn loading_images_for_no_messages_is_a_no_op() {
        let (s, _tmp) = make_storage().await;
        assert!(s.load_message_images(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn token_counts_round_trip_and_default_null() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let s = SqliteSessionStorage::new(db.system);
        s.create_session("tok".to_string()).await.unwrap();

        s.add_message(
            "tok".to_string(),
            SessionMessage::new(
                "u1".to_string(),
                "tok".to_string(),
                ChatMessage::user("question"),
            ),
        )
        .await
        .unwrap();
        s.add_message(
            "tok".to_string(),
            SessionMessage::new(
                "a1".to_string(),
                "tok".to_string(),
                ChatMessage::assistant("answer"),
            )
            .with_token_counts(Some(1930), Some(87)),
        )
        .await
        .unwrap();

        let msgs = s.get_messages("tok").await.unwrap();
        assert_eq!(msgs[0].prompt_tokens, None, "user rows carry no counts");
        assert_eq!(msgs[1].prompt_tokens, Some(1930));
        assert_eq!(msgs[1].completion_tokens, Some(87));
    }

    /// PAI-5 P2, migration 0039. Two claims in one, and the second is the one
    /// that would rot silently: reasoning is stored ALONGSIDE the provider's
    /// completion count and does not disturb it, and a row written without a
    /// reasoning count reads back `None` rather than `Some(0)`. PAI-5 P5 sizes
    /// an output reserve from this column, and "nobody counted" read as "no
    /// thinking happened" would bias every reserve downwards.
    /// PAI-5 P5's read, and the guard on the defaulted port method.
    ///
    /// The port defaults `recent_reasoning_samples` to an empty vec so mocks
    /// keep compiling, and a defaulted trait method is a recorded vacuity shape
    /// in this programme: deleting a real override leaves the tree green while
    /// the feature quietly stops working. Here it would stop by keeping the
    /// anchor forever, which is silent by construction. So the real adapter is
    /// asserted to answer with real numbers.
    #[tokio::test]
    async fn sqlite_reads_real_reasoning_samples_rather_than_the_default() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let s = SqliteSessionStorage::new(db.system);
        s.create_session("samples".to_string()).await.unwrap();

        for (id, reasoning) in [
            ("m1", Some(120u32)),
            ("m2", None),
            ("m3", Some(340)),
            ("m4", None),
            ("m5", Some(90)),
        ] {
            s.add_message(
                "samples".to_string(),
                SessionMessage::new(
                    id.to_string(),
                    "samples".to_string(),
                    ChatMessage::assistant("turn"),
                )
                .with_token_counts(Some(100), Some(10))
                .with_reasoning_tokens(reasoning),
            )
            .await
            .unwrap();
        }

        let mut got = s.recent_reasoning_samples(100).await.unwrap();
        got.sort_unstable();
        assert_eq!(
            got,
            vec![90, 120, 340],
            "an unmeasured turn must not arrive as a zero. Migration 0039 left that column \
             nullable with no DEFAULT for exactly this reason, and a zero here is a vote for a \
             smaller output reserve cast by a turn that never reasoned"
        );

        // `scan_limit` bounds ROWS READ, not samples returned. Reading one row
        // can therefore yield no samples at all -- which is the point: a pond
        // with thinking off must not walk its whole history every turn.
        let scanned_one = s.recent_reasoning_samples(1).await.unwrap();
        assert!(
            scanned_one.len() <= 1,
            "scan_limit is being applied to the sample count rather than to the rows scanned; \
             on a pond with no reasoning that makes this a full history scan per turn"
        );
    }

    #[tokio::test]
    async fn reasoning_tokens_round_trip_beside_the_provider_counts() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let pool = db.system.clone();
        let s = SqliteSessionStorage::new(db.system);
        s.create_session("reason".to_string()).await.unwrap();

        s.add_message(
            "reason".to_string(),
            SessionMessage::new(
                "a1".to_string(),
                "reason".to_string(),
                ChatMessage::assistant("thought about it"),
            )
            .with_token_counts(Some(1930), Some(87))
            .with_reasoning_tokens(Some(412)),
        )
        .await
        .unwrap();
        // A row from a path that carries no reasoning: NULL, not zero.
        s.add_message(
            "reason".to_string(),
            SessionMessage::new(
                "a2".to_string(),
                "reason".to_string(),
                ChatMessage::assistant("did not think about it"),
            )
            .with_token_counts(Some(20), Some(4)),
        )
        .await
        .unwrap();

        // The row shape every pre-0039 message has: written by an INSERT that
        // never mentions the column at all. This is the case the column's
        // absent DEFAULT is FOR, and the only way to reach it from a unit test
        // — the adapter always binds the column, so binding NULL through it
        // would pass just as happily against `DEFAULT 0`.
        sqlx::query(
            "INSERT INTO session_messages (id, session_id, role, content, created_at) \
             VALUES ('a3', 'reason', 'assistant', 'written before 0039', datetime('now'))",
        )
        .execute(&pool)
        .await
        .unwrap();

        let msgs = s.get_messages("reason").await.unwrap();
        assert_eq!(msgs[0].reasoning_tokens, Some(412));
        assert_eq!(
            msgs[0].completion_tokens,
            Some(87),
            "reasoning was folded into the completion count instead of riding beside it"
        );
        assert_eq!(
            msgs[1].reasoning_tokens, None,
            "an uncounted row must stay NULL; Some(0) would claim the turn did no thinking"
        );
        assert_eq!(
            msgs[2].reasoning_tokens, None,
            "a row written without the column read back as a counted zero — migration 0039 \
             has grown a DEFAULT, and every message written before this phase now claims \
             its turn did no thinking. PAI-5 P5 sizes an output reserve from that."
        );
    }

    // ── Session identity (PAI-1 P2) ───────────────────────────────────────

    /// `sessions.profile_id` has existed since migration 0003 and nothing ever
    /// wrote it. This is the test that stops it being a dead column again.
    #[tokio::test]
    async fn a_new_session_is_unattributed_and_reads_back_that_way() {
        let (s, _tmp) = make_storage().await;
        let session = s.create_session("sess-1".to_string()).await.unwrap();
        assert_eq!(session.profile_id, None);
        assert_eq!(s.get_session("sess-1").await.unwrap().profile_id, None);
        assert_eq!(
            s.get_session_identity("sess-1").await.unwrap(),
            SessionIdentity::unknown()
        );
    }

    #[tokio::test]
    async fn identity_round_trips_including_the_face_confidence() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        insert_profile(&s, "jerry").await;
        let before = s.get_session("sess-1").await.unwrap().updated_at;

        s.set_session_identity(
            "sess-1",
            &SessionIdentity {
                profile_id: Some("jerry".to_string()),
                source: IdentificationSource::Face,
                confidence: Some(0.62),
            },
        )
        .await
        .unwrap();

        let read = s.get_session_identity("sess-1").await.unwrap();
        assert_eq!(read.profile_id.as_deref(), Some("jerry"));
        assert_eq!(
            s.get_session("sess-1").await.unwrap().updated_at,
            before,
            "identifying a session must not reorder the user's chat history"
        );
        assert_eq!(read.source, IdentificationSource::Face);
        assert!((read.confidence.unwrap() - 0.62).abs() < 1e-6);

        // and the same fact is visible on the session itself, which is what
        // the sessions list and every later scope decision will read.
        assert_eq!(
            s.get_session("sess-1").await.unwrap().profile_id.as_deref(),
            Some("jerry")
        );
        assert_eq!(
            s.list_sessions().await.unwrap()[0].profile_id.as_deref(),
            Some("jerry")
        );
    }

    /// An attribution that is accepted and then quietly dropped is exactly the
    /// bug this phase exists to end, so a write against a session that does not
    /// exist has to fail loudly.
    #[tokio::test]
    async fn identifying_a_session_that_does_not_exist_is_an_error() {
        let (s, _tmp) = make_storage().await;
        let err = s
            .set_session_identity(
                "no-such-session",
                &SessionIdentity {
                    profile_id: Some("jerry".to_string()),
                    source: IdentificationSource::Explicit,
                    confidence: None,
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, SessionStorageError::SessionNotFound(id) if id == "no-such-session"),
            "expected SessionNotFound"
        );
    }

    /// Reading identity for an unknown session is NOT an error -- "nobody" is a
    /// correct answer to "whose session is this".
    #[tokio::test]
    async fn reading_identity_for_an_unknown_session_says_nobody() {
        let (s, _tmp) = make_storage().await;
        assert_eq!(
            s.get_session_identity("no-such-session").await.unwrap(),
            SessionIdentity::unknown()
        );
    }

    /// Migration 0003 declared `profile_id REFERENCES profiles(id)` with no ON
    /// DELETE action, and `Database::init` turns foreign keys on. That was
    /// harmless only while the column stayed NULL. Now that it is written,
    /// deleting a member who has ever spoken to the pond would fail the FK
    /// check -- so 0037 carries a BEFORE DELETE trigger standing in for the ON
    /// DELETE SET NULL that SQLite will not let us add in place.
    #[tokio::test]
    async fn deleting_a_profile_releases_their_sessions_instead_of_failing() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        insert_profile(&s, "jerry").await;
        s.set_session_identity(
            "sess-1",
            &SessionIdentity {
                profile_id: Some("jerry".to_string()),
                source: IdentificationSource::Face,
                confidence: Some(0.91),
            },
        )
        .await
        .unwrap();

        sqlx::query("DELETE FROM profiles WHERE id = ?")
            .bind("jerry")
            .execute(&s.pool)
            .await
            .expect("deleting a member must not be blocked by their sessions");

        // The conversation survives; only the attribution is gone. Erasing the
        // content is a separate, deliberate cascade (PAI-1 P7).
        let session = s.get_session("sess-1").await.unwrap();
        assert_eq!(session.id, "sess-1");
        assert_eq!(session.profile_id, None);
        assert_eq!(
            s.get_session_identity("sess-1").await.unwrap(),
            SessionIdentity::unknown(),
            "a released session must not keep a dangling source or confidence"
        );
    }

    /// `profile_id` is a real foreign key, so identity cannot name a member who
    /// does not exist. Worth pinning: it is the cheapest guard against a typo'd
    /// or stale id becoming a permanent, unmatchable attribution.
    #[tokio::test]
    async fn identity_cannot_name_a_profile_that_does_not_exist() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        let err = s
            .set_session_identity(
                "sess-1",
                &SessionIdentity {
                    profile_id: Some("ghost".to_string()),
                    source: IdentificationSource::Explicit,
                    confidence: None,
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, SessionStorageError::StorageError(_)),
            "expected the foreign key to reject an unknown profile, got {err:?}"
        );
    }

    async fn insert_profile(s: &SqliteSessionStorage, id: &str) {
        sqlx::query(
            "INSERT INTO profiles (id, display_name, avatar_emoji, preferences) \
             VALUES (?, ?, 'duck', '{}')",
        )
        .bind(id)
        .bind(id)
        .execute(&s.pool)
        .await
        .expect("profiles row is required by the sessions.profile_id foreign key");
    }

    // ── Race-free identity writes (PAI-1 P4) ─────────────────────────────

    /// The read-compare-write this replaced could lose: two requests both read
    /// `Unknown`, both passed `supersedes`, and the later write won whatever
    /// its rank. Here the comparison is inside the UPDATE, so the stale caller
    /// simply does not match.
    #[tokio::test]
    async fn a_weaker_source_cannot_win_even_from_a_stale_read() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        insert_profile(&s, "jerry").await;
        insert_profile(&s, "liz").await;

        // Somebody taps "this is Jerry".
        assert!(s
            .set_session_identity_if_stronger(
                "sess-1",
                &SessionIdentity {
                    profile_id: Some("jerry".into()),
                    source: IdentificationSource::Explicit,
                    confidence: None,
                },
            )
            .await
            .unwrap());

        // A camera frame, decided against the state BEFORE that write, tries
        // to bind a different person on weaker evidence.
        assert!(
            !s.set_session_identity_if_stronger(
                "sess-1",
                &SessionIdentity {
                    profile_id: Some("liz".into()),
                    source: IdentificationSource::Face,
                    confidence: Some(0.62),
                },
            )
            .await
            .unwrap(),
            "a face match must not take a session an explicit claim holds"
        );

        let held = s.get_session_identity("sess-1").await.unwrap();
        assert_eq!(held.profile_id.as_deref(), Some("jerry"));
        assert_eq!(held.source, IdentificationSource::Explicit);
    }

    #[tokio::test]
    async fn an_equal_or_stronger_source_still_wins() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        insert_profile(&s, "jerry").await;
        insert_profile(&s, "liz").await;

        for (source, who) in [
            (IdentificationSource::Face, "jerry"),
            // equal strength: re-identification, the endpoint's normal case
            (IdentificationSource::Face, "liz"),
            // stronger
            (IdentificationSource::Explicit, "jerry"),
            (IdentificationSource::PairedDevice, "liz"),
        ] {
            assert!(
                s.set_session_identity_if_stronger(
                    "sess-1",
                    &SessionIdentity {
                        profile_id: Some(who.into()),
                        source,
                        confidence: None,
                    },
                )
                .await
                .unwrap(),
                "{:?} should have been accepted",
                source
            );
            assert_eq!(
                s.get_session_identity("sess-1").await.unwrap().source,
                source
            );
        }
    }

    fn who(profile: &str, source: IdentificationSource) -> SessionIdentity {
        SessionIdentity {
            profile_id: Some(profile.into()),
            source,
            confidence: None,
        }
    }

    /// An implicit device claim never moves a session to a different member.
    ///
    /// THE DEFECT: `resolve_turn_scope` persisted the paired-device rung with
    /// the strength-only write, and resolves scope for read routes too. So
    /// Liz's phone opening the proposals for Jerry's session -- bound to him
    /// by face, or by his own "this is Jerry" -- took it, because
    /// `PairedDevice` outranks both. This is the one write that must not.
    #[tokio::test]
    async fn a_device_claim_never_takes_a_session_bound_to_another_member() {
        for held in [IdentificationSource::Face, IdentificationSource::Explicit] {
            let (s, _tmp) = make_storage().await;
            s.create_session("sess-1".to_string()).await.unwrap();
            insert_profile(&s, "jerry").await;
            insert_profile(&s, "liz").await;
            assert!(s
                .set_session_identity_if_stronger("sess-1", &who("jerry", held))
                .await
                .unwrap());

            assert!(
                !s.claim_session_identity(
                    "sess-1",
                    &who("liz", IdentificationSource::PairedDevice)
                )
                .await
                .unwrap(),
                "Liz's device took a session bound to Jerry by {held:?}"
            );
            let kept = s.get_session_identity("sess-1").await.unwrap();
            assert_eq!(kept.profile_id.as_deref(), Some("jerry"));
            assert_eq!(kept.source, held);
        }
    }

    /// The control: a device claim binds a session nobody holds. Without this,
    /// a claim that refused everything would pass the test above -- and would
    /// quietly stop the batch extractor attributing any paired-device chat.
    #[tokio::test]
    async fn a_device_claim_binds_an_unattributed_session() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        insert_profile(&s, "liz").await;
        assert!(s
            .claim_session_identity("sess-1", &who("liz", IdentificationSource::PairedDevice))
            .await
            .unwrap());
        assert_eq!(
            s.get_session_identity("sess-1")
                .await
                .unwrap()
                .profile_id
                .as_deref(),
            Some("liz")
        );
    }

    /// And it still strengthens the SAME member's binding: a face match
    /// upgraded to the device proof, which is the case the claim exists for.
    #[tokio::test]
    async fn a_device_claim_strengthens_the_same_members_binding() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        insert_profile(&s, "jerry").await;
        assert!(s
            .set_session_identity_if_stronger("sess-1", &who("jerry", IdentificationSource::Face))
            .await
            .unwrap());
        assert!(s
            .claim_session_identity("sess-1", &who("jerry", IdentificationSource::PairedDevice))
            .await
            .unwrap());
        assert_eq!(
            s.get_session_identity("sess-1").await.unwrap().source,
            IdentificationSource::PairedDevice
        );
    }

    #[tokio::test]
    async fn a_claim_on_a_missing_session_is_still_not_found() {
        let (s, _tmp) = make_storage().await;
        let err = s
            .claim_session_identity(
                "no-such-session",
                &who("liz", IdentificationSource::PairedDevice),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SessionStorageError::SessionNotFound(id) if id == "no-such-session"));
    }

    /// A legacy row has a NULL source and is unattributed, so anything binds it.
    #[tokio::test]
    async fn anything_binds_a_legacy_row_with_no_source() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-1".to_string()).await.unwrap();
        insert_profile(&s, "jerry").await;
        assert!(s
            .set_session_identity_if_stronger(
                "sess-1",
                &SessionIdentity {
                    profile_id: Some("jerry".into()),
                    source: IdentificationSource::Face,
                    confidence: Some(0.5),
                },
            )
            .await
            .unwrap());
    }

    /// Zero rows updated is ambiguous between "no such session" and "not
    /// superseded". The caller needs them apart -- one is a 404, the other a
    /// normal refusal -- so the adapter disambiguates rather than guessing.
    #[tokio::test]
    async fn a_conditional_write_to_a_missing_session_is_still_not_found() {
        let (s, _tmp) = make_storage().await;
        let err = s
            .set_session_identity_if_stronger(
                "no-such-session",
                &SessionIdentity {
                    profile_id: None,
                    source: IdentificationSource::Explicit,
                    confidence: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SessionStorageError::SessionNotFound(id) if id == "no-such-session"));
    }

    // ── Reasoning text (PAI-5 P6) ───────────────────────────────────────────
    //
    // `add_thinking` / `get_thinking_for_session` are DEFAULTED on the port so
    // the four non-SQLite implementors need no change. The cost of a default is
    // that deleting the override below leaves the whole workspace green while
    // the feature silently stops working -- the exact vacuity shape this
    // programme keeps recording. These tests are the counterweight: they run
    // against the real adapter, and `pond-infra` is in ci.yml's test list.

    async fn seed_assistant_row(s: &SqliteSessionStorage, session: &str, msg: &str) {
        s.add_message(
            session.to_string(),
            SessionMessage::new(
                msg.to_string(),
                session.to_string(),
                ChatMessage::assistant("the answer"),
            ),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn thinking_blocks_round_trip_keyed_to_their_message() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-think".to_string()).await.unwrap();
        seed_assistant_row(&s, "sess-think", "assistant-1").await;
        seed_assistant_row(&s, "sess-think", "assistant-2").await;

        s.add_thinking(
            "sess-think",
            "assistant-1",
            &["first".to_string(), "second".to_string()],
        )
        .await
        .unwrap();
        s.add_thinking("sess-think", "assistant-2", &["only".to_string()])
            .await
            .unwrap();

        let out = s.get_thinking_for_session("sess-think").await.unwrap();

        // Order within a turn is the whole readability of the panel, and both
        // blocks of turn one are written inside the same `datetime('now')`
        // second -- so an adapter that ordered by created_at would shuffle them
        // and this assertion is what notices.
        assert_eq!(
            out.get("assistant-1").map(Vec::as_slice),
            Some(["first".to_string(), "second".to_string()].as_slice()),
            "turn one's passages must come back in emission order; got {:?}",
            out.get("assistant-1")
        );
        assert_eq!(
            out.get("assistant-2").map(Vec::as_slice),
            Some(["only".to_string()].as_slice()),
            "turn two's passage must be keyed to turn two, not merged into the \
             session; got {:?}",
            out.get("assistant-2")
        );
        assert_eq!(out.len(), 2, "one entry per message that has reasoning");
    }

    #[tokio::test]
    async fn thinking_is_scoped_to_its_own_session() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-a".to_string()).await.unwrap();
        s.create_session("sess-b".to_string()).await.unwrap();
        seed_assistant_row(&s, "sess-a", "a-1").await;
        seed_assistant_row(&s, "sess-b", "b-1").await;
        s.add_thinking("sess-a", "a-1", &["private to A".to_string()])
            .await
            .unwrap();
        s.add_thinking("sess-b", "b-1", &["private to B".to_string()])
            .await
            .unwrap();

        // Invariant 6. A session's scope is `sessions.profile_id`, and the read
        // is per-session; a Guest session must not be able to reach a household
        // member's reasoning simply because both rows live in one table.
        let a = s.get_thinking_for_session("sess-a").await.unwrap();
        assert_eq!(a.len(), 1, "session A sees only its own reasoning: {a:?}");
        assert!(
            !a.contains_key("b-1"),
            "session A can read session B's reasoning -- the session filter is \
             missing from the query: {a:?}"
        );
    }

    #[tokio::test]
    async fn a_session_with_no_reasoning_reads_back_empty_not_missing() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-quiet".to_string()).await.unwrap();
        seed_assistant_row(&s, "sess-quiet", "q-1").await;

        // Every session recorded before `persist_thinking` was turned on is in
        // this state, which is the overwhelming majority of them. Reading one
        // must be an empty map, never an error -- `get_session_messages`
        // swallows the error, so an adapter that failed here would turn every
        // historical page load into a page with no thinking AND no signal.
        let out = s.get_thinking_for_session("sess-quiet").await.unwrap();
        assert!(out.is_empty(), "expected no reasoning rows, got {out:?}");
    }

    #[tokio::test]
    async fn deleting_a_session_takes_its_reasoning_with_it() {
        let (s, _tmp) = make_storage().await;
        s.create_session("sess-gone-think".to_string())
            .await
            .unwrap();
        seed_assistant_row(&s, "sess-gone-think", "g-1").await;
        s.add_thinking("sess-gone-think", "g-1", &["candid".to_string()])
            .await
            .unwrap();

        s.delete_session("sess-gone-think").await.unwrap();

        // The erasure path that exists today. Reasoning text is the least
        // reviewed thing the model produces; it must not be the one artefact
        // that outlives the conversation a user asked to forget.
        let left: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_thinking WHERE session_id = ?")
                .bind("sess-gone-think")
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(
            left, 0,
            "{left} reasoning row(s) survived the deletion of their session -- \
             the ON DELETE CASCADE in migration 0040 is not being enforced \
             (check `PRAGMA foreign_keys` is on for this pool)"
        );
    }
}
