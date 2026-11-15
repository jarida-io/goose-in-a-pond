use crate::models::domain::message::ImageAttachment;
use crate::user_data::domain::message_note::MessageNote;
use crate::user_data::domain::session::{
    CutTurn, ExtractionCursor, MessageAttachment, Session, SessionIdentity, SessionMessage,
    SessionSurface,
};
use chrono::{DateTime, Utc};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SessionStorageError {
    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Message not found: {0}")]
    MessageNotFound(String),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("General error: {0}")]
    General(String),
}

/// Driven port for persisting conversation sessions and their messages.
#[async_trait::async_trait]
pub trait SessionStorage: Send + Sync {
    async fn create_session(&self, session_id: String) -> Result<Session, SessionStorageError>;

    /// Records where a conversation was started, once: a later turn from another screen does
    /// not move it. `device_id` is the paired device that began it, when there was one.
    async fn mark_surface(
        &self,
        _session_id: &str,
        _surface: SessionSurface,
        _device_id: Option<&str>,
    ) -> Result<(), SessionStorageError> {
        Ok(())
    }

    async fn get_session(&self, session_id: &str) -> Result<Session, SessionStorageError>;

    async fn add_message(
        &self,
        session_id: String,
        message: SessionMessage,
    ) -> Result<SessionMessage, SessionStorageError>;

    async fn get_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionMessage>, SessionStorageError>;

    /// Conversations whose newest message is a person's, sent since `since`, with nothing
    /// attached: the turns a stop cut off that can be answered again from their words alone.
    async fn cut_turns(&self, _since: DateTime<Utc>) -> Result<Vec<CutTurn>, SessionStorageError> {
        Ok(Vec::new())
    }

    /// Replaces the note under one stored message; an empty note clears it.
    async fn set_message_note(
        &self,
        _session_id: &str,
        _message_id: &str,
        _note: &MessageNote,
    ) -> Result<(), SessionStorageError> {
        Ok(())
    }

    async fn update_title(
        &self,
        session_id: &str,
        title: String,
    ) -> Result<(), SessionStorageError>;

    /// Delete a session and all its messages.
    async fn delete_session(&self, session_id: &str) -> Result<(), SessionStorageError>;

    /// List all sessions, ordered by most recently updated first.
    async fn list_sessions(&self) -> Result<Vec<Session>, SessionStorageError>;

    /// Up to `limit` messages starting at `offset`, in chronological order.
    async fn get_messages_paginated(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError>;

    /// The newest `limit` messages, oldest-first; use over `get_messages()` to cap context load.
    async fn get_recent_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError>;

    async fn increment_usage(
        &self,
        _session_id: &str,
        _prompt_tokens: u32,
        _completion_tokens: u32,
        _model_name: Option<&str>,
    ) -> Result<(), SessionStorageError> {
        Ok(())
    }

    /// Message count for the sidebar badge; the default `0` is only a stub for mocks.
    async fn count_messages(&self, _session_id: &str) -> Result<u64, SessionStorageError> {
        Ok(0)
    }

    /// Recent `reasoning_tokens`, newest first; unmeasured (`NULL`) turns are skipped, not zeroed.
    /// `scan_limit` caps rows read, not samples. The empty default leaves the output reserve as is.
    async fn recent_reasoning_samples(
        &self,
        _scan_limit: usize,
    ) -> Result<Vec<u32>, SessionStorageError> {
        Ok(Vec::new())
    }

    /// Earliest user message's content: the label fallback for a session with no `title`.
    async fn first_user_message(
        &self,
        _session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(None)
    }

    /// `(summary, id of the last message it covers)`; `(None, None)` when there is none yet.
    async fn get_rolling_summary(
        &self,
        _session_id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionStorageError> {
        Ok((None, None))
    }

    /// The rolling summary and its `rolling_summary_updated_at` revision, read together: reading
    /// them separately races the summary service and stamps a stale vector as current.
    async fn get_rolling_summary_with_revision(
        &self,
        _session_id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionStorageError> {
        Ok((None, None))
    }

    /// Store a rolling summary covering messages up to `through_message_id`.
    async fn set_rolling_summary(
        &self,
        _session_id: &str,
        _summary: &str,
        _through_message_id: &str,
    ) -> Result<(), SessionStorageError> {
        Ok(())
    }

    /// `(title_source, title_through_id)`: who wrote the title and, if the model, what it covers.
    /// `(None, None)` (a pre-provenance row) makes the re-titling gate treat it as user-chosen.
    async fn get_title_provenance(
        &self,
        _session_id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionStorageError> {
        Ok((None, None))
    }

    /// Earliest assistant reply, the history card preview (the title already covers the ask).
    async fn first_assistant_message(
        &self,
        _session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(None)
    }

    /// The earliest answer that cites a web page, or tool result that names its page (a read's
    /// address, a lookup's source): where a conversation card finds its picture.
    async fn first_web_message(
        &self,
        _session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(None)
    }

    /// Messages added since `message_id`; `None` (not in this session) means the marker covers
    /// nothing, not "no change". Worth overriding: the idle re-titler calls it for every session.
    async fn messages_after(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<Option<u64>, SessionStorageError> {
        let messages = self.get_messages(session_id).await?;
        Ok(messages
            .iter()
            .position(|m| m.id == message_id)
            .map(|i| (messages.len() - i - 1) as u64))
    }

    /// Store the six-word fallback title, stamped `derived` so the re-titling job may replace it.
    async fn set_derived_title(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<(), SessionStorageError> {
        self.update_title(session_id, title.to_string()).await
    }

    /// Store a model-written title and the newest message it covers, stamped `model`.
    async fn set_generated_title(
        &self,
        session_id: &str,
        title: &str,
        _through_message_id: &str,
    ) -> Result<(), SessionStorageError> {
        self.update_title(session_id, title.to_string()).await
    }

    /// The engine's own session id paired with this GIAP session, persisted to survive restarts.
    /// Opaque: re-validate it with the engine, whose store can be wiped independently of ours.
    async fn get_engine_session_id(
        &self,
        _session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(None)
    }

    /// The GIAP session paired to an engine session id; callers must refuse on `None`.
    async fn get_session_id_for_engine(
        &self,
        _engine_session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(None)
    }

    /// Upsert the engine pairing; no `sessions` row needed, as some paths pair before it exists.
    async fn set_engine_session_id(
        &self,
        _session_id: &str,
        _engine_session_id: &str,
    ) -> Result<(), SessionStorageError> {
        Ok(())
    }

    /// Who this session is attributed to, and on what evidence.
    /// Unidentified sessions and unknown ids get [`SessionIdentity::unknown`], not an error.
    async fn get_session_identity(
        &self,
        _session_id: &str,
    ) -> Result<SessionIdentity, SessionStorageError> {
        Ok(SessionIdentity::unknown())
    }

    /// Record who a session belongs to; precedence is [`SessionIdentity::supersedes`]'s job.
    /// Needs the `sessions` row: a missing one is [`SessionStorageError::SessionNotFound`].
    async fn set_session_identity(
        &self,
        _session_id: &str,
        _identity: &SessionIdentity,
    ) -> Result<(), SessionStorageError> {
        Ok(())
    }

    /// Write `identity` only if it supersedes the stored one; `false` if a stronger one held.
    /// Compare inside the write, since read-then-write races; the default is not race-free.
    async fn set_session_identity_if_stronger(
        &self,
        session_id: &str,
        identity: &SessionIdentity,
    ) -> Result<bool, SessionStorageError> {
        let existing = self.get_session_identity(session_id).await?;
        if !identity.supersedes(&existing) {
            return Ok(false);
        }
        self.set_session_identity(session_id, identity).await?;
        Ok(true)
    }

    /// Like [`set_session_identity_if_stronger`], but never moves a session to another member.
    /// For inferred identity (a paired phone opening a chat); the default is not race-free.
    async fn claim_session_identity(
        &self,
        session_id: &str,
        identity: &SessionIdentity,
    ) -> Result<bool, SessionStorageError> {
        let existing = self.get_session_identity(session_id).await?;
        let someone_else =
            existing.profile_id.is_some() && existing.profile_id != identity.profile_id;
        if someone_else || !identity.supersedes(&existing) {
            return Ok(false);
        }
        self.set_session_identity(session_id, identity).await?;
        Ok(true)
    }

    /// Tool groups (MCP extension names) fixed per session so the KV prompt prefix stays reusable.
    /// Persisted so `enable_tool_group` widening survives restarts; `None` = not chosen yet.
    async fn get_session_tool_groups(
        &self,
        _session_id: &str,
    ) -> Result<Option<Vec<String>>, SessionStorageError> {
        Ok(None)
    }

    /// Upsert this session's tool groups (replacing the list); no `sessions` row needed.
    async fn set_session_tool_groups(
        &self,
        _session_id: &str,
        _groups: &[String],
    ) -> Result<(), SessionStorageError> {
        Ok(())
    }

    // ── Image attachments ───────────────────────────────────────────────────
    // `add_message` writes them; only these methods read them, so `get_messages` stays cheap.

    /// Metadata for every attachment in a session, chronological then by ordinal; reads no bytes.
    async fn list_session_attachments(
        &self,
        _session_id: &str,
    ) -> Result<Vec<MessageAttachment>, SessionStorageError> {
        Ok(Vec::new())
    }

    /// Base64 images for the given messages, keyed by message id, each in ordinal order.
    async fn load_message_images(
        &self,
        _message_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<ImageAttachment>>, SessionStorageError> {
        Ok(std::collections::HashMap::new())
    }

    /// One attachment's `(MIME type, decoded bytes)`, raw because it serves `<img>` requests.
    async fn read_attachment(
        &self,
        _attachment_id: &str,
    ) -> Result<Option<(String, Vec<u8>)>, SessionStorageError> {
        Ok(None)
    }

    // ── Reasoning text ──────────────────────────────────────────────────────
    // The only way a turn's `<thinking>` text enters or leaves the pond.

    /// Persist a turn's reasoning passages in order; `ChatService` gates it on `persist_thinking`.
    async fn add_thinking(
        &self,
        _session_id: &str,
        _message_id: &str,
        _blocks: &[String],
    ) -> Result<(), SessionStorageError> {
        Ok(())
    }

    /// A session's reasoning passages by message id, in emission order. UI only: never feed it to
    /// a prompt (`tests/thinking_is_never_replayed.rs` pins the permitted callers).
    async fn get_thinking_for_session(
        &self,
        _session_id: &str,
    ) -> Result<std::collections::HashMap<String, Vec<String>>, SessionStorageError> {
        Ok(std::collections::HashMap::new())
    }

    /// Set a message's training vote: `Some(true)` accept, `Some(false)` exclude, `None` clear.
    async fn set_message_feedback(
        &self,
        _session_id: &str,
        _message_id: &str,
        _liked: Option<bool>,
    ) -> Result<(), SessionStorageError> {
        Err(SessionStorageError::General(
            "message feedback is not supported by this SessionStorage adapter".to_string(),
        ))
    }

    // ── Batch memory extraction cursor ──────────────────────────────────────
    // Defaults read "never examined" and drop writes, so walks repeat but never skip a chat.

    /// How far batch memory extraction has read into this conversation.
    async fn extraction_cursor(
        &self,
        _session_id: &str,
    ) -> Result<ExtractionCursor, SessionStorageError> {
        Ok(ExtractionCursor::unstarted())
    }

    /// Move the watermark through `Some(id)`, resetting attempts; `None` clears it and its stamp.
    /// Must not touch `sessions.updated_at`: the idle gate would read that as someone returning.
    async fn set_extraction_cursor(
        &self,
        _session_id: &str,
        _through_message_id: Option<&str>,
    ) -> Result<(), SessionStorageError> {
        Ok(())
    }

    /// Count an unparseable read without moving the watermark; returns consecutive attempts.
    /// Same `updated_at` rule as [`set_extraction_cursor`](Self::set_extraction_cursor).
    async fn note_extraction_attempt(&self, _session_id: &str) -> Result<u32, SessionStorageError> {
        Ok(0)
    }

    /// Delete `message_id` and every later message (insertion order), for edit/refresh resubmits.
    async fn delete_messages_from(
        &self,
        _session_id: &str,
        _message_id: &str,
    ) -> Result<(), SessionStorageError> {
        Err(SessionStorageError::General(
            "message truncation is not supported by this SessionStorage adapter".to_string(),
        ))
    }
}
