use crate::models::domain::message::ImageAttachment;
use crate::user_data::domain::session::{
    ExtractionCursor, MessageAttachment, Session, SessionIdentity, SessionMessage,
};
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

/// Driven Port: SessionStorage
///
/// This trait defines the interface for persisting conversation sessions and messages.
/// Implementations can range from in-memory storage to database-backed persistence.
#[async_trait::async_trait]
pub trait SessionStorage: Send + Sync {
    /// Create a new session.
    async fn create_session(&self, session_id: String) -> Result<Session, SessionStorageError>;

    /// Get a session by ID.
    async fn get_session(&self, session_id: &str) -> Result<Session, SessionStorageError>;

    /// Add a message to a session.
    async fn add_message(
        &self,
        session_id: String,
        message: SessionMessage,
    ) -> Result<SessionMessage, SessionStorageError>;

    /// Get all messages for a session.
    async fn get_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionMessage>, SessionStorageError>;

    /// Update the title of a session.
    async fn update_title(
        &self,
        session_id: &str,
        title: String,
    ) -> Result<(), SessionStorageError>;

    /// Delete a session and all its messages.
    async fn delete_session(&self, session_id: &str) -> Result<(), SessionStorageError>;

    /// List all sessions, ordered by most recently updated first.
    async fn list_sessions(&self) -> Result<Vec<Session>, SessionStorageError>;

    /// Get messages for a session with pagination.
    ///
    /// Returns up to `limit` messages starting from `offset`, ordered chronologically.
    async fn get_messages_paginated(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError>;

    /// Fetch the most recent `limit` messages, returned in chronological order
    /// (oldest-first). Use this instead of `get_messages()` to cap context load.
    async fn get_recent_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError>;

    /// Increment the cumulative token usage for a session.
    async fn increment_usage(
        &self,
        _session_id: &str,
        _prompt_tokens: u32,
        _completion_tokens: u32,
        _model_name: Option<&str>,
    ) -> Result<(), SessionStorageError> {
        Ok(()) // default no-op for backward compat
    }

    /// Count the messages stored for a session.
    ///
    /// Used to render a per-conversation badge in the history sidebar.
    /// The default returns 0 so in-memory mocks and legacy adapters keep
    /// compiling; real adapters override with an indexed `COUNT(*)`.
    async fn count_messages(&self, _session_id: &str) -> Result<u64, SessionStorageError> {
        Ok(0) // default no-op for backward compat
    }

    /// What reasoning has actually cost on this pond, newest first — PAI-5 P5.
    ///
    /// Returns the `reasoning_tokens` of recent assistant messages that HAVE
    /// one. `None` rows are dropped rather than folded in as zero, and that is
    /// the whole correctness of this method: migration 0039 made the column
    /// nullable with no `DEFAULT` precisely so an unmeasured turn is
    /// distinguishable from a turn that did not reason, and a `0` from a turn
    /// nobody counted is a vote for a smaller output reserve cast by evidence
    /// that does not exist.
    ///
    /// `scan_limit` bounds the ROWS READ, not the samples returned. A pond with
    /// thinking switched off has no `reasoning_tokens` anywhere, and a query
    /// bounded only by a result count would walk the entire message history
    /// every turn finding nothing — on a Jetson, forever.
    ///
    /// **The default returns no samples, and that is the narrowing direction.**
    /// No samples means [`observed_output_reserve`] keeps the measured anchor,
    /// which is the value the curve already shipped. A defaulted trait method is
    /// one of this programme's recorded vacuity shapes, so it is worth being
    /// explicit about why this one is safe: forgetting to override it cannot
    /// produce a wrong reserve, only the previous one.
    /// `sqlite_reads_real_reasoning_samples_rather_than_the_default` guards the
    /// real adapter against exactly that.
    ///
    /// [`observed_output_reserve`]: crate::models::services::context::context_budget::observed_output_reserve
    async fn recent_reasoning_samples(
        &self,
        _scan_limit: usize,
    ) -> Result<Vec<u32>, SessionStorageError> {
        Ok(Vec::new())
    }

    /// Return the content of the earliest user message in a session, if any.
    ///
    /// Used as a read-time fallback to derive a human-readable label when a
    /// session has no stored `title`. The default returns `None` so mocks and
    /// legacy adapters keep compiling.
    async fn first_user_message(
        &self,
        _session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(None) // default no-op for backward compat
    }

    /// Read the session's rolling conversation summary and the id of the last
    /// message it covers. `(None, None)` when no summary exists yet.
    async fn get_rolling_summary(
        &self,
        _session_id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionStorageError> {
        Ok((None, None)) // default no-op for backward compat
    }

    /// Read the rolling summary together with the revision stamp written
    /// alongside it (`rolling_summary_updated_at`).
    ///
    /// Both in ONE call on purpose. The personal-context index stamps a
    /// summary's vector with the revision it was computed from, and reading the
    /// text and the revision separately races the summary service: the sweep
    /// would embed one version and stamp it with another, which makes a stale
    /// vector look current and therefore never get repaired.
    async fn get_rolling_summary_with_revision(
        &self,
        _session_id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionStorageError> {
        // Default: no summary. Inert rather than wrong — an implementation that
        // does not override this simply contributes no summaries to the index.
        Ok((None, None))
    }

    /// Store a refreshed rolling summary. `through_message_id` is the id of
    /// the newest message the summary covers.
    async fn set_rolling_summary(
        &self,
        _session_id: &str,
        _summary: &str,
        _through_message_id: &str,
    ) -> Result<(), SessionStorageError> {
        Ok(()) // default no-op for backward compat
    }

    /// Who last wrote `sessions.title`, and — for a model-written one — the id
    /// of the newest message it covers: `(title_source, title_through_id)`.
    ///
    /// `(None, None)` means the row predates the provenance columns. The
    /// re-titling gate reads that as "assume a person chose this name" unless
    /// the stored title is byte-identical to the six-word fallback, so a
    /// missing override here can only ever make the job *more* conservative.
    async fn get_title_provenance(
        &self,
        _session_id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionStorageError> {
        Ok((None, None)) // default: unknown provenance, treated as off-limits
    }

    /// Return the content of the earliest assistant message in a session.
    ///
    /// The history wall's card preview. It shows what the pond *answered*
    /// rather than what it was asked, because the title already says what was
    /// asked — a card whose heading and body paraphrase the same sentence
    /// reads as a rendering fault, and the answer is the half you would not
    /// have been able to reconstruct from memory anyway.
    ///
    /// The default returns `None` so mocks and legacy adapters keep compiling;
    /// a card with no preview simply shows its title.
    async fn first_assistant_message(
        &self,
        _session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(None) // default no-op for backward compat
    }

    /// How many messages a session has gained since `message_id`.
    ///
    /// `None` means that message is not in this session — deleted, or a
    /// rebuilt history. Callers should read that as "this marker covers
    /// nothing" rather than "nothing has changed", or a compacted conversation
    /// would freeze whatever it was last marked with.
    ///
    /// Exists to be cheap. The idle re-titling pass asks this of every
    /// conversation it considers, and its steady state is "already named,
    /// nothing new since" — so the obvious implementation, loading the history
    /// and finding the index, would re-read every message of every
    /// conversation every five minutes, forever, on the smallest device this
    /// runs on. That obvious implementation is exactly what the default below
    /// does, which is correct and is why overriding it is worth doing.
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

    /// Store the deterministic six-word fallback title, stamped `derived` so
    /// the re-titling job knows it is free to improve on it.
    ///
    /// The default delegates to [`update_title`](Self::update_title) rather
    /// than no-opping: this replaces a call that every adapter already
    /// implements, and a silent no-op would stop sessions getting titles at
    /// all. An adapter that does not override simply records no provenance.
    async fn set_derived_title(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<(), SessionStorageError> {
        self.update_title(session_id, title.to_string()).await
    }

    /// Store a model-written title together with the id of the newest message
    /// it covers, stamping its provenance as `model`.
    ///
    /// Same delegation reasoning as [`set_derived_title`](Self::set_derived_title).
    /// An adapter that does not override still gets the better title; it just
    /// cannot distinguish it later, so the gate leaves it alone from then on.
    async fn set_generated_title(
        &self,
        session_id: &str,
        title: &str,
        _through_message_id: &str,
    ) -> Result<(), SessionStorageError> {
        self.update_title(session_id, title.to_string()).await
    }

    /// The agent engine's own session id paired with this GIAP session, if one
    /// has been recorded.
    ///
    /// Engines keep their own conversation store under ids they generate
    /// themselves. Without a persisted pairing, a server restart makes an
    /// existing GIAP chat resolve to a brand-new empty engine session and the
    /// model loses the whole conversation even though every message is still in
    /// `pond_system.db`. The id is engine-generated and opaque here — callers
    /// must re-validate it against the engine before use, because the engine's
    /// store can be wiped independently of ours.
    async fn get_engine_session_id(
        &self,
        _session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(None) // default no-op for backward compat
    }

    /// The GIAP session paired to an engine session id, if any.
    ///
    /// The inverse of [`get_engine_session_id`](Self::get_engine_session_id).
    /// Needed because a builtin MCP tool call carries the ENGINE's session id
    /// in its request `_meta`, and a draft decision has to resolve that to a
    /// speaker.
    ///
    /// The default returns `Ok(None)` -- "unresolvable". That is the narrowing
    /// answer, so a mock or legacy adapter that does not override it causes a
    /// refusal, never a permission.
    async fn get_session_id_for_engine(
        &self,
        _engine_session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(None)
    }

    /// Record the engine session paired with this GIAP session (idempotent
    /// upsert). Deliberately not keyed to a `sessions` row: the pairing is also
    /// established on paths (direct tool calls, the voice child) that can run
    /// before a GIAP session row exists.
    async fn set_engine_session_id(
        &self,
        _session_id: &str,
        _engine_session_id: &str,
    ) -> Result<(), SessionStorageError> {
        Ok(()) // default no-op for backward compat
    }

    /// Who this session is attributed to, and on what evidence.
    ///
    /// Returns [`SessionIdentity::unknown`] for a session that has never been
    /// identified, which today is every session in every existing pond. That
    /// is a real answer, not a missing one: "nobody has been identified" is
    /// exactly what the caller needs to know, and returning it rather than an
    /// `Option` removes the temptation to treat absence as permission.
    ///
    /// An unknown session id also reads as unattributed rather than erroring.
    /// A read asking "whose session is this" has a correct answer for a session
    /// that does not exist, and it is "nobody".
    async fn get_session_identity(
        &self,
        _session_id: &str,
    ) -> Result<SessionIdentity, SessionStorageError> {
        Ok(SessionIdentity::unknown()) // default no-op for backward compat
    }

    /// Record who a session belongs to.
    ///
    /// This does NOT decide whether the new identification should win over
    /// whatever is already stored -- that is a policy question and it lives in
    /// [`SessionIdentity::supersedes`], in the domain. An adapter that made the
    /// choice itself would put the rule beyond the reach of a pond-core test.
    ///
    /// Unlike the tool-group and engine-session pairings, this one is stored on
    /// the `sessions` row itself, so it genuinely requires the row to exist.
    /// Implementations return [`SessionStorageError::SessionNotFound`] rather
    /// than succeeding silently -- an attribution that was accepted and then
    /// discarded is the failure mode this whole phase exists to end.
    async fn set_session_identity(
        &self,
        _session_id: &str,
        _identity: &SessionIdentity,
    ) -> Result<(), SessionStorageError> {
        Ok(()) // default no-op for backward compat
    }

    /// Write an identity **only if** it is at least as strong as what is stored.
    ///
    /// The read-compare-write in the handlers is not safe on its own. Two
    /// requests can both read `Unknown` and both pass
    /// [`SessionIdentity::supersedes`], after which the later write wins
    /// whatever its rank -- so a face match landing a millisecond after a
    /// member tapped "this is Liz" takes the session, for a different person,
    /// on weaker evidence. That is the exact downgrade `supersedes` exists to
    /// refuse, and it is reachable today.
    ///
    /// Implementations must do the comparison inside the write itself. Returns
    /// `true` when the write happened, `false` when a stronger identification
    /// already held the session -- which is a normal outcome, not an error.
    ///
    /// The default implementation is **not** race-free; it falls back to the
    /// unconditional write so mocks and legacy adapters keep compiling. Real
    /// adapters override it.
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

    /// The tool GROUPS (MCP extension names) selected for this session, if any.
    ///
    /// Phase D2 chooses a session's tool surface once, from its opening message,
    /// and keeps it stable so the local engine's KV prompt prefix stays reusable
    /// across turns. Persisting it matters for one specific reason: the model can
    /// widen its own surface mid-session via `enable_tool_group`, and a
    /// process-local record would silently drop that capability on the next
    /// restart, mid-conversation, with no way for the user to tell why.
    ///
    /// `None` means "not chosen yet" — the caller selects and stores.
    async fn get_session_tool_groups(
        &self,
        _session_id: &str,
    ) -> Result<Option<Vec<String>>, SessionStorageError> {
        Ok(None) // default no-op for backward compat
    }

    /// Record this session's tool groups (idempotent upsert, replaces the list).
    ///
    /// Deliberately not keyed to a `sessions` row, for the same reason as
    /// [`set_engine_session_id`]: selection also happens on paths that run before
    /// a GIAP `sessions` row exists.
    ///
    /// [`set_engine_session_id`]: SessionStorage::set_engine_session_id
    async fn set_session_tool_groups(
        &self,
        _session_id: &str,
        _groups: &[String],
    ) -> Result<(), SessionStorageError> {
        Ok(()) // default no-op for backward compat
    }

    // ── Image attachments (phase F2) ────────────────────────────────────────
    //
    // Attachments are written implicitly: `add_message` persists whatever is on
    // `SessionMessage.message.images`. They are read back EXPLICITLY, through
    // the three methods below, and never inflated into `get_messages` — a
    // session-history read happens on every turn and on every UI load, and
    // silently base64-inflating every image a conversation ever contained would
    // turn a cheap read into a multi-megabyte one.

    /// Metadata for every attachment in a session, chronological then by
    /// ordinal. Cheap: no bytes are read.
    ///
    /// Callers use this to decide WHICH images are worth loading (see
    /// `models::services::context::image_history::plan_image_replay`) before
    /// paying for any of them.
    async fn list_session_attachments(
        &self,
        _session_id: &str,
    ) -> Result<Vec<MessageAttachment>, SessionStorageError> {
        Ok(Vec::new()) // default no-op for backward compat
    }

    /// Load the base64 image payloads for specific messages, keyed by message
    /// id, with each message's images in ordinal order.
    ///
    /// Batched deliberately: history replay needs a handful of images chosen
    /// across a whole conversation, and one call per image would be one file
    /// read plus one query per image.
    async fn load_message_images(
        &self,
        _message_ids: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<ImageAttachment>>, SessionStorageError> {
        Ok(std::collections::HashMap::new()) // default no-op for backward compat
    }

    /// Read one attachment's raw (decoded) bytes and MIME type.
    ///
    /// Serves the desktop's `<img>` requests, so it returns bytes rather than
    /// base64 — re-encoding only to have the browser decode again is pure waste.
    async fn read_attachment(
        &self,
        _attachment_id: &str,
    ) -> Result<Option<(String, Vec<u8>)>, SessionStorageError> {
        Ok(None) // default no-op for backward compat
    }

    // ── Reasoning text (PAI-5 P6) ───────────────────────────────────────────
    //
    // The two methods below are the ONLY way the `<thinking>` text a turn
    // produced enters or leaves the pond. They live here rather than on a
    // dedicated port for one blunt reason: the store has to be reachable from
    // `ChatService` (which mints the assistant message id these rows are keyed
    // to) and from the history-read handler, and both of those already hold a
    // `SessionStorage`. A separate port would have meant a new `AppState`
    // field, and "the adapter exists but production never wired it" is a
    // failure this programme has recorded three times.
    //
    // Defaulted, like every method above, so the four non-SQLite implementors
    // (two mocks, a `pond-agent` test double, a capturing fake) need no change.
    // The cost of a default is that deleting the real override leaves the tree
    // green -- so `SqliteSessionStorage` carries its own behavioural test
    // (`thinking_blocks_round_trip_keyed_to_their_message`), not a grep.

    /// Persist the reasoning passages a turn produced, in order, keyed to the
    /// assistant message they produced.
    ///
    /// Called only when `settings.persist_thinking` is true; the gate lives in
    /// `ChatService`, which owns turn persistence, so no handler can hold the
    /// text and forget to ask.
    async fn add_thinking(
        &self,
        _session_id: &str,
        _message_id: &str,
        _blocks: &[String],
    ) -> Result<(), SessionStorageError> {
        Ok(()) // default no-op for backward compat
    }

    /// Every stored reasoning passage in a session, grouped by message id, each
    /// message's passages in emission order.
    ///
    /// **This is a UI read and nothing else.** It must never be called from
    /// anything that builds a prompt -- the trimmer, the rolling summariser,
    /// the prompt builder, the compactor. Replaying a model's own discarded
    /// scratch work back at it is the failure PAI-5's third invariant names, and
    /// `crates/pond-core/tests/thinking_is_never_replayed.rs` enumerates the
    /// permitted callers of this method by name.
    async fn get_thinking_for_session(
        &self,
        _session_id: &str,
    ) -> Result<std::collections::HashMap<String, Vec<String>>, SessionStorageError> {
        Ok(std::collections::HashMap::new()) // default no-op for backward compat
    }

    /// Set (or clear, with `None`) the liked/disliked training-feedback flag
    /// on one message. `Some(true)` marks the turn as accepted training data,
    /// `Some(false)` marks it excluded, `None` clears any prior vote.
    ///
    /// The default is a no-op error so adapters that predate the feedback UI
    /// keep compiling; the SQLite adapter is the real implementation.
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

    // ── Batch memory extraction cursor (migration 0056) ─────────────────────
    //
    // Three defaulted methods, for the same reason as every other default in
    // this trait: four non-SQLite implementors exist and none of them has a
    // conversation worth mining. The cost of a default is that deleting the
    // real override leaves the tree green, so the SQLite adapter carries its
    // own behavioural tests rather than a grep.
    //
    // The defaults are the narrowing direction. An adapter that does not
    // override reads as "never examined" and silently discards every write, so
    // the batch engine re-walks the same window forever rather than advancing
    // past conversations it never read. Wasteful, never wrong.

    /// How far batch memory extraction has read into this conversation.
    async fn extraction_cursor(
        &self,
        _session_id: &str,
    ) -> Result<ExtractionCursor, SessionStorageError> {
        Ok(ExtractionCursor::unstarted())
    }

    /// Move the watermark, or clear it.
    ///
    /// `Some(id)` records that the walk has covered everything up to and
    /// including that message, stamps the time, and resets the attempt count --
    /// a watermark that moved is a watermark nothing has failed against yet.
    ///
    /// `None` clears the cursor back to unstarted, which is what a walk does
    /// when its anchor has been deleted (see
    /// [`messages_after`](Self::messages_after) returning `None`). The stamp is
    /// cleared with it, deliberately: a conversation that must be re-walked
    /// from message one has not been examined, and leaving the stamp would sort
    /// it to the back of a backlog it has not started.
    ///
    /// **Implementations must not touch `sessions.updated_at`.** That column is
    /// one of the two activity sources the idle gate reads
    /// (`consolidation_schedule::saw_activity_since_start`), so a background
    /// writer stamping it looks exactly like a person coming back: the pass's
    /// own watcher would cancel it mid-run, and every pass would shove the idle
    /// clock forward. Both existing title writers already avoid this for the
    /// same reason, and a source-grep test pins it.
    async fn set_extraction_cursor(
        &self,
        _session_id: &str,
        _through_message_id: Option<&str>,
    ) -> Result<(), SessionStorageError> {
        Ok(())
    }

    /// Record that a window was read and came back unparseable, returning the
    /// new consecutive-attempt count.
    ///
    /// Separate from [`set_extraction_cursor`](Self::set_extraction_cursor)
    /// because the watermark must NOT move: the window has not been examined,
    /// only attempted. Same `updated_at` rule applies.
    async fn note_extraction_attempt(&self, _session_id: &str) -> Result<u32, SessionStorageError> {
        Ok(0)
    }

    /// Delete `message_id` and every later message in the same session (by
    /// insertion order).
    ///
    /// Backs "edit" and "refresh" on a user message: the caller truncates
    /// from that message onward, then resubmits the (same or edited) text as
    /// a normal new turn through the existing chat endpoint — no separate
    /// regenerate code path needed.
    ///
    /// The default is a no-op error so adapters that predate this feature
    /// keep compiling; the SQLite adapter is the real implementation.
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
