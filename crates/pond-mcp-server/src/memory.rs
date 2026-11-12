//! Memory MCP server: recall, save and forget memory fragments.

use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::shared::domain::agent_note::{
    dropped_for_room, same_note, tidy, AgentNote, NoteKind,
};
use pond_core::shared::ports::agent_notes::AgentNotesRepository;
use pond_core::shared::services::child_sessions::{process_child_sessions, ChildSessionRegistry};
use pond_core::shared::services::turn_authority::TurnAuthorityRegistry;
use pond_core::user_data::domain::member_profile::{
    admit, closed_by, file_under, profile_order, render_profile, same_sentence, About,
};
use pond_core::user_data::domain::memory::{
    cosine_similarity, MemoryEventKind, MemoryFragment, MemoryLifecycle,
};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ErrorCode, ErrorData, Implementation, InitializeResult, Meta,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

// ── Parameter structs ──────────────────────────────────────────────────────

/// Rows a whole-profile recall returns when no limit is asked for.
const PROFILE_ROWS: usize = 40;
/// Rows a recall with a query returns when no limit is asked for.
const RECALLED_ROWS: usize = 10;
/// Rows of a member's that `forget_memory` looks through for the words it was given.
const FORGET_SEARCHED: usize = 200;
/// Two sentences this close say one fact. High on purpose: a lower bar drops "likes hiking"
/// as a copy of "likes biking".
const SAME_FACT_SIMILARITY: f32 = 0.97;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RecallMemoriesParams {
    /// What to look for, in the person's words. Omit for their whole profile.
    pub query: Option<String>,
    /// Max results.
    pub limit: Option<u32>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SaveMemoryParams {
    /// One whole sentence in the third person that names the person and states the fact.
    #[serde(default)]
    pub content: String,
    /// What the fact is about, one of: name birthday work home language partner child pet
    /// family friend diet allergy health routine likes dislikes style goal place date
    /// instruction now ("now": true for days or weeks only).
    pub about: Option<String>,
    /// Catch-all for unexpected fields the model sends; an old `segment` arrives here.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ForgetMemoryParams {
    /// The memory to forget: words from it, as the person said them.
    pub what: Option<String>,
    /// Catch-all for unexpected fields the model sends; an old `id` or `content` arrives here.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NoteToSelfParams {
    /// The note, as one sentence.
    #[serde(default)]
    pub text: String,
    /// "lesson" for how to work better; "note" for something to come back to. Default "note".
    pub kind: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Why a call touched no memory. Rendered as a successful result: small models retry errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No live turn or helper stands behind the call, so nobody can be named.
    Unresolved,
    /// An unidentified speaker in a household of several.
    Guest,
}

impl Refusal {
    /// Model-facing text; each ends with what to do instead, or a small model retries verbatim.
    pub fn message(&self) -> &'static str {
        match self {
            Refusal::Unresolved => {
                "Memory is not available in this conversation, so nothing was read, saved or \
                 deleted. Answer from this conversation and say you could not use memory."
            }
            Refusal::Guest => {
                "Memory is only available once this conversation is identified as a member of \
                 the household, so nothing was read, saved or deleted. Answer from this \
                 conversation instead."
            }
        }
    }

    fn trace(&self) -> &'static str {
        match self {
            Refusal::Unresolved => "unresolved",
            Refusal::Guest => "guest",
        }
    }
}

fn refused(tool: &str, refusal: Refusal) -> CallToolResult {
    tracing::info!(
        target: "giap::trace",
        kind = "memory_tool_refused",
        tool,
        reason = refusal.trace(),
        "a memory tool call had no member to act for"
    );
    CallToolResult::success(vec![ContentBlock::text(refusal.message())])
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct MemoryMcpServer {
    memory_repo: Arc<dyn MemoryRepository + Send + Sync>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    /// Live turns by engine session; `None` reads the registry installed with the orchestrator.
    turns: Option<Arc<TurnAuthorityRegistry>>,
    children: Arc<ChildSessionRegistry>,
    /// Who the members are, so a saved fact can be checked to name its member; `None` reads
    /// the store installed with [`init_memory_profiles`].
    profiles: Option<Arc<dyn ProfileRepository + Send + Sync>>,
    /// Goose's own notes; `None` reads the store installed with [`init_agent_notes`].
    notes: Option<Arc<dyn AgentNotesRepository>>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl MemoryMcpServer {
    /// All tools, without constructing the server; the generated `tool_router()` is private.
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new(
        memory_repo: Arc<dyn MemoryRepository + Send + Sync>,
        embedding_provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    ) -> Self {
        Self {
            memory_repo,
            embedding_provider,
            turns: None,
            children: process_child_sessions(),
            profiles: None,
            notes: None,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
What the Pond remembers about the person, as their profile: with `query`, the facts nearest it; \
without, the whole profile. Check it before answering about them.")]
    async fn recall_memories(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<RecallMemoriesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_recall(&ctx.meta, params.0).await
    }

    #[tool(description = "\
Remember one lasting fact the person said about themselves: who they are, their people, what \
they eat, their routines, likes, goals, or a standing instruction to you. Not a question they \
asked, a task, a one-off plan (that is add_reminder), or anything you or a web page said. A new \
name, home, work, partner, diet or style replaces the old one by itself.")]
    async fn save_memory(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<SaveMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_save(&ctx.meta, params.0).await
    }

    #[tool(description = "Forget a memory when the person asks, by words from it.")]
    async fn forget_memory(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<ForgetMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_forget(&ctx.meta, params.0).await
    }

    #[tool(description = "\
Write a note for yourself, not a fact about the person: a lesson (how to work better, learnt \
from a mistake or a correction) or a follow-up to come back to.")]
    async fn note_to_self(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<NoteToSelfParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_note(&ctx.meta, params.0).await
    }
}

impl MemoryMcpServer {
    /// Resolves callers from these registries instead of the process's own.
    pub fn with_registries(
        mut self,
        turns: Arc<TurnAuthorityRegistry>,
        children: Arc<ChildSessionRegistry>,
    ) -> Self {
        self.turns = Some(turns);
        self.children = children;
        self
    }

    /// Checks saved facts against these members' names instead of the process's own store.
    pub fn with_profiles(mut self, profiles: Arc<dyn ProfileRepository + Send + Sync>) -> Self {
        self.profiles = Some(profiles);
        self
    }

    /// Keeps Goose's notes in this store instead of the process's own.
    pub fn with_notes(mut self, notes: Arc<dyn AgentNotesRepository>) -> Self {
        self.notes = Some(notes);
        self
    }

    /// The body of `note_to_self`, apart from the rmcp wrapper so tests can call it.
    pub async fn run_note(
        &self,
        meta: &Meta,
        params: NoteToSelfParams,
    ) -> Result<CallToolResult, ErrorData> {
        let scope = match self.scope_for(meta) {
            Ok(scope) => scope,
            Err(refusal) => return Ok(refused("note_to_self", refusal)),
        };
        let Some(notes) = self.notes.clone().or_else(|| AGENT_NOTES.get().cloned()) else {
            return Ok(said(
                "Nothing was noted: this Pond keeps no notes for you yet. Carry on without.",
            ));
        };
        let Ok(text) = tidy(&params.text) else {
            return Ok(said(
                "Nothing was noted. Send `text`: the note, as one sentence.",
            ));
        };
        let kind = NoteKind::parse(params.kind.as_deref().unwrap_or_default());
        let owner = scope.owner_id().map(str::to_string);
        let internal = |e: anyhow::Error| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to keep the note: {e}"),
                None,
            )
        };
        let existing = notes
            .owned(owner.as_deref(), kind)
            .await
            .map_err(internal)?;
        if existing.iter().any(|note| same_note(&note.text, &text)) {
            return Ok(said("Already noted."));
        }
        // Few are kept, so the list stays read: the oldest makes room.
        for old in dropped_for_room(kind, &existing) {
            let _ = notes.remove(&old, &scope).await;
        }
        notes
            .add(AgentNote {
                id: uuid::Uuid::new_v4().to_string(),
                profile_id: owner,
                kind,
                text: text.clone(),
                created_at: chrono::Utc::now(),
            })
            .await
            .map_err(internal)?;
        tracing::info!(
            target: "giap::trace",
            kind = "agent_note_written",
            note_kind = kind.as_str(),
            chars = text.chars().count(),
            "the agent wrote itself a note"
        );
        Ok(said(match kind {
            NoteKind::Lesson => format!("Noted for yourself as a lesson: {text}"),
            NoteKind::Note => format!("Noted for yourself to come back to: {text}"),
        }))
    }

    /// The member a scope files under: the name to use in advice, and every name a fact about
    /// them may use. Empty for the household, and when the member cannot be read.
    async fn names_for(&self, scope: &ProfileScope) -> (String, Vec<String>) {
        // The process's store is read per call, so it may be installed after this server starts.
        let profiles = self
            .profiles
            .clone()
            .or_else(|| MEMORY_PROFILES.get().cloned());
        let (Some(id), Some(profiles)) = (scope.owner_id(), profiles) else {
            return (String::new(), Vec::new());
        };
        let Ok(Some(profile)) = profiles.get(id).await else {
            return (String::new(), Vec::new());
        };
        let mut names = vec![profile.display_name.trim().to_string()];
        if let Some(preferred) = profile.preferences.get("preferred_name") {
            names.push(preferred.trim().to_string());
        }
        names.retain(|n| !n.is_empty());
        (names.first().cloned().unwrap_or_default(), names)
    }

    /// Whose memories this call may reach: the live turn's scope, which its injected memories were
    /// read at, else a helper's clamped one. Keyed by `_meta`, which the model cannot write.
    fn scope_for(&self, meta: &Meta) -> Result<ProfileScope, Refusal> {
        let session = crate::session_from_meta(meta).ok_or(Refusal::Unresolved)?;
        // The adapter's registry, installed with the orchestrator's deps once the adapter exists.
        let turns = self
            .turns
            .clone()
            .or_else(|| crate::installed_orchestrator_deps().map(|deps| deps.authorities()));
        let scope = turns
            .and_then(|turns| turns.authority_for_engine_session(&session))
            .map(|authority| authority.profile_scope().clone())
            .or_else(|| self.children.scope_of(&session))
            .ok_or(Refusal::Unresolved)?;
        if scope.excludes_everything() {
            return Err(Refusal::Guest);
        }
        Ok(scope)
    }

    /// The body of `recall_memories`, apart from the rmcp wrapper so tests can call it.
    pub async fn run_recall(
        &self,
        meta: &Meta,
        params: RecallMemoriesParams,
    ) -> Result<CallToolResult, ErrorData> {
        let scope = match self.scope_for(meta) {
            Ok(scope) => scope,
            Err(refusal) => return Ok(refused("recall_memories", refusal)),
        };
        let asked = params
            .query
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty());
        let default_rows = if asked.is_some() {
            RECALLED_ROWS
        } else {
            PROFILE_ROWS
        };
        let limit = params
            .limit
            .map_or(default_rows, |l| (l as usize).clamp(1, PROFILE_ROWS));
        let params = RecallMemoriesParams {
            query: asked.map(str::to_string),
            ..params
        };

        let filtered: Vec<_> = if let Some(ref query) = params.query {
            if let Some(emb) = self.embedding_provider.as_ref().filter(|e| e.available()) {
                // A query, not a document -- see EmbeddingProvider::embed_query.
                match emb.embed_query(query).await {
                    Ok(query_vec) => {
                        match self
                            .memory_repo
                            .search_similar(&query_vec, &scope, limit)
                            .await
                        {
                            Ok(results) if !results.is_empty() => {
                                tracing::debug!(
                                    count = results.len(),
                                    "recall_memories: vector search returned results"
                                );
                                results
                            }
                            _ => {
                                tracing::debug!(
                                    "recall_memories: vector search empty, falling back to keyword"
                                );
                                keyword_search(&self.memory_repo, &scope, query, limit).await?
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "recall_memories: embedding failed ({e}), falling back to keyword"
                        );
                        keyword_search(&self.memory_repo, &scope, query, limit).await?
                    }
                }
            } else {
                keyword_search(&self.memory_repo, &scope, query, limit).await?
            }
        } else {
            self.memory_repo
                .search_recent(&scope, limit)
                .await
                .map_err(|e| {
                    ErrorData::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("Memory error: {}", e),
                        None,
                    )
                })?
        };

        // Record access for decay tracking + audit log
        for f in &filtered {
            let _ = self.memory_repo.record_access(&f.id).await;
            let _ = self
                .memory_repo
                .log_event(MemoryEventKind::Recalled, &f.id, None, None)
                .await;
        }

        let now = chrono::Utc::now();
        let mut filtered: Vec<MemoryFragment> =
            filtered.into_iter().filter(|f| f.is_shown(now)).collect();
        // A whole profile reads in profile order; an answer to a query keeps its ranking.
        if params.query.is_none() {
            profile_order(&mut filtered);
        }
        let text = if filtered.is_empty() {
            crate::format::format_dead_end(
                "stored memories matching this",
                "Nothing is stored about it. Answer from this conversation or ask \
                 the user — do not search the web for a fact about them.",
            )
        } else {
            render_profile(&filtered)
        };

        if !filtered.is_empty() {
            let ui_memories: Vec<serde_json::Value> = filtered
                .iter()
                .map(|f| {
                    serde_json::json!({
                        "content": f.content,
                        "segment": f.segment.as_ref().map_or("unknown", |s| s.as_str()),
                        "about": f.about.map(About::as_str),
                        "importance": f.importance.unwrap_or(0.0),
                        "created_at": f.created_at.format("%Y-%m-%d").to_string(),
                    })
                })
                .collect();
            let ui_data = serde_json::json!({ "memories": ui_memories });
            let hint = format!("[[[mcp-ui:memory:{}]]]\n", ui_data);
            let full_result = format!("{}{}", hint, text);
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                full_result,
            )]));
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    /// The body of `save_memory`, apart from the rmcp wrapper so tests can call it.
    pub async fn run_save(
        &self,
        meta: &Meta,
        params: SaveMemoryParams,
    ) -> Result<CallToolResult, ErrorData> {
        let scope = match self.scope_for(meta) {
            Ok(scope) => scope,
            Err(refusal) => return Ok(refused("save_memory", refusal)),
        };
        let internal = |e: anyhow::Error| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to save memory: {e}"),
                None,
            )
        };
        let (name, aliases) = self.names_for(&scope).await;
        let about = params
            .about
            .as_deref()
            .or_else(|| params.extra.get("segment").and_then(|v| v.as_str()))
            .and_then(About::parse);
        let content = match admit(&params.content, about, &aliases) {
            Ok(content) => content,
            Err(why) => {
                tracing::info!(
                    target: "giap::trace",
                    kind = "memory_not_kept",
                    reason = why.as_str(),
                    "save_memory was offered something the write policy does not keep"
                );
                return Ok(said(format!(
                    "Nothing was saved: it {}. {}",
                    why.as_str(),
                    why.advice(&name)
                )));
            }
        };
        let Some(about) = about else {
            return Ok(said("Nothing was saved."));
        };

        let owner = scope.owner_id().map(str::to_string);
        let existing: Vec<MemoryFragment> = self
            .memory_repo
            .live_about(&scope, about)
            .await
            .map_err(internal)?
            .into_iter()
            .filter(|row| row.profile_id == owner)
            .collect();
        if let Some(known) = existing
            .iter()
            .find(|row| same_sentence(&row.content, &content))
        {
            let _ = self.memory_repo.record_access(&known.id).await;
            return Ok(said(format!("Already remembered: {}", known.content)));
        }

        let embedder = self.embedding_provider.as_ref().filter(|e| e.available());
        let embedding = match embedder {
            Some(embedder) => match embedder.embed(&content).await {
                Ok(vector) => Some(vector),
                Err(e) => {
                    tracing::warn!("save_memory: embedding failed ({e}), saving without");
                    None
                }
            },
            None => None,
        };
        // A list fact said again in other words is not a second fact. A single-valued one is
        // let through: the new wording replaces the old.
        if !about.single_valued() {
            let again = embedding.as_deref().and_then(|new| {
                existing.iter().find(|row| {
                    row.embedding.as_deref().is_some_and(|old| {
                        old.len() == new.len()
                            && cosine_similarity(old, new) >= SAME_FACT_SIMILARITY
                    })
                })
            });
            if let Some(known) = again {
                let _ = self.memory_repo.record_access(&known.id).await;
                return Ok(said(format!("Already remembered: {}", known.content)));
            }
        }

        let id = uuid::Uuid::new_v4().to_string();
        let mut fragment = MemoryFragment::from_chat(id.clone(), owner, None, content.clone());
        fragment.source = "mcp_tool".to_string();
        fragment.embedding = embedding;
        fragment.lifecycle = Some(MemoryLifecycle::Active);
        file_under(&mut fragment, about);
        self.memory_repo.add(fragment).await.map_err(internal)?;
        let _ = self
            .memory_repo
            .log_event(MemoryEventKind::Written, &id, None, None)
            .await;

        let mut replaced: Vec<String> = Vec::new();
        for old in closed_by(about, &content, &existing) {
            if self.memory_repo.mark_superseded(&old.id, &id).await.is_ok() {
                let _ = self
                    .memory_repo
                    .log_event(MemoryEventKind::Superseded, &old.id, None, None)
                    .await;
                replaced.push(old.content.clone());
            }
        }
        // An older caller's explicit list; only a row this caller could recall is retired.
        let listed = params
            .extra
            .get("supersedes")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str());
        for old_id in listed {
            if let Ok(Some(old)) = self.memory_repo.get_in_scope(old_id, &scope).await {
                if self.memory_repo.mark_superseded(old_id, &id).await.is_ok() {
                    replaced.push(old.content);
                }
            }
        }

        let heading = about.segment().heading();
        let mut plain_text = format!("Remembered under {heading}: {content}");
        if !replaced.is_empty() {
            plain_text.push_str(&format!(" It replaces: {}", replaced.join(" ")));
        }
        let ui_data = serde_json::json!({
            "content": content,
            "segment": about.segment().as_str(),
            "about": about.as_str(),
            "saved": true,
        });
        Ok(said(format!(
            "[[[mcp-ui:memory_saved:{ui_data}]]]\n{plain_text}"
        )))
    }

    /// The body of `forget_memory`, apart from the rmcp wrapper so tests can call it.
    pub async fn run_forget(
        &self,
        meta: &Meta,
        params: ForgetMemoryParams,
    ) -> Result<CallToolResult, ErrorData> {
        let scope = match self.scope_for(meta) {
            Ok(scope) => scope,
            Err(refusal) => return Ok(refused("forget_memory", refusal)),
        };
        let internal = |e: anyhow::Error| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to forget: {e}"),
                None,
            )
        };
        let extra = |key: &str| params.extra.get(key).and_then(|v| v.as_str());
        let Some(what) = params
            .what
            .as_deref()
            .or_else(|| extra("id"))
            .or_else(|| extra("content"))
            .map(str::trim)
            .filter(|w| !w.is_empty())
        else {
            return Ok(said(
                "Nothing was forgotten. Send `what`: words from the memory to forget.",
            ));
        };

        // Out of scope reads as missing, so another member's memory can't be deleted or probed.
        let by_id = self
            .memory_repo
            .get_in_scope(what, &scope)
            .await
            .map_err(internal)?;
        let found = match by_id {
            Some(row) => vec![row],
            None => {
                let rows = self
                    .memory_repo
                    .search_recent(&scope, FORGET_SEARCHED)
                    .await
                    .map_err(internal)?;
                matching_words(rows, what)
            }
        };
        match found.as_slice() {
            [] => Ok(said(crate::format::format_dead_end(
                "a memory with those words",
                "Nothing was forgotten. Call recall_memories to see how it is worded, then \
                 send words from it.",
            ))),
            [one] => {
                self.memory_repo.delete(&one.id).await.map_err(internal)?;
                let _ = self
                    .memory_repo
                    .log_event(MemoryEventKind::Deleted, &one.id, None, None)
                    .await;
                Ok(said(format!("Forgotten: {}", one.content)))
            }
            several => Ok(said(format!(
                "Nothing was forgotten: {} memories fit those words. Ask which they mean, \
                 then send more of its words:\n{}",
                several.len(),
                several
                    .iter()
                    .take(5)
                    .map(|row| format!("- {}", row.content))
                    .collect::<Vec<_>>()
                    .join("\n")
            ))),
        }
    }
}

fn said(text: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text.into())])
}

/// The rows `what` points at: the one that is that sentence, else every row holding all of its
/// words of three letters or more.
fn matching_words(rows: Vec<MemoryFragment>, what: &str) -> Vec<MemoryFragment> {
    if let Some(exact) = rows.iter().find(|row| same_sentence(&row.content, what)) {
        return vec![exact.clone()];
    }
    let words: Vec<String> = what
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_string)
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    rows.into_iter()
        .filter(|row| {
            let content = row.content.to_lowercase();
            words.iter().all(|w| content.contains(w.as_str()))
        })
        .collect()
}

#[tool_handler]
impl ServerHandler for MemoryMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-memory",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Memory MCP server — the profile the Pond keeps of each household member.\n\n\
                 Tools: recall_memories (their profile, or the facts nearest a query), \
                 save_memory (one lasting fact they said about themselves, with what it is \
                 `about`), forget_memory (by words from it), note_to_self (your own lessons \
                 and follow-ups, never facts about people).\n\n\
                 A new name, home, work, partner, diet or style replaces the old one in code; \
                 a correction is saved the same way. Facts are filed under headings (who they \
                 are, people, food and health, routines, preferences, goals, places, dates, \
                 standing instructions, right now).",
            )
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────

/// Keyword-based memory search: fetch recent, then filter by substring match.
async fn keyword_search(
    memory_repo: &Arc<dyn MemoryRepository + Send + Sync>,
    scope: &ProfileScope,
    query: &str,
    limit: usize,
) -> Result<Vec<MemoryFragment>, ErrorData> {
    let fragments = memory_repo
        .search_recent(scope, limit * 2) // fetch more to allow for filtering
        .await
        .map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Memory error: {}", e),
                None,
            )
        })?;
    let q_lower = query.to_lowercase();
    Ok(fragments
        .into_iter()
        .filter(|f| f.content.to_lowercase().contains(&q_lower))
        .take(limit)
        .collect())
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct MemoryDeps {
    memory_repo: Arc<dyn MemoryRepository + Send + Sync>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
}

static MEMORY_DEPS: OnceLock<MemoryDeps> = OnceLock::new();

/// Install the deps once at startup; without `embedding_provider`, recall is keyword-only.
pub fn init_memory_deps(
    memory_repo: Arc<dyn MemoryRepository + Send + Sync>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
) {
    let _ = MEMORY_DEPS.set(MemoryDeps {
        memory_repo,
        embedding_provider,
    });
}

static AGENT_NOTES: OnceLock<Arc<dyn AgentNotesRepository>> = OnceLock::new();

/// Install the store for Goose's own notes once at startup; without it `note_to_self` says
/// nothing was noted.
pub fn init_agent_notes(notes: Arc<dyn AgentNotesRepository>) {
    let _ = AGENT_NOTES.set(notes);
}

/// The store installed with [`init_agent_notes`], for the turn that shows Goose its lessons.
pub fn agent_notes() -> Option<Arc<dyn AgentNotesRepository>> {
    AGENT_NOTES.get().cloned()
}

static MEMORY_PROFILES: OnceLock<Arc<dyn ProfileRepository + Send + Sync>> = OnceLock::new();

/// Install the member store once at startup, so a saved fact is checked to name its member.
/// Without it that check is skipped.
pub fn init_memory_profiles(profiles: Arc<dyn ProfileRepository + Send + Sync>) {
    let _ = MEMORY_PROFILES.set(profiles);
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_memory_server(reader: DuplexStream, writer: DuplexStream) {
    // No deps = this binary didn't install this family: skip, as a panic kills every builtin.
    let Some(deps) = MEMORY_DEPS.get() else {
        tracing::error!(
            "spawn_memory_server called before init_memory_deps — extension will not start"
        );
        return;
    };
    let server = MemoryMcpServer::new(deps.memory_repo.clone(), deps.embedding_provider.clone());
    crate::serve_builtin("giap-memory", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pond_core::user_data::domain::memory::MemoryFragment;
    use pond_core::user_data::ports::memory_repository::MemoryRepository;

    struct StubMemory;
    #[async_trait]
    impl MemoryRepository for StubMemory {
        async fn add(&self, _: MemoryFragment) -> anyhow::Result<()> {
            Ok(())
        }
        async fn search_recent(
            &self,
            _: &ProfileScope,
            _: usize,
        ) -> anyhow::Result<Vec<MemoryFragment>> {
            Ok(vec![])
        }
        async fn search_similar(
            &self,
            _: &[f32],
            _: &ProfileScope,
            _: usize,
        ) -> anyhow::Result<Vec<MemoryFragment>> {
            Ok(vec![])
        }
        async fn delete(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_server() -> MemoryMcpServer {
        MemoryMcpServer::new(Arc::new(StubMemory), None)
    }

    /// Stub embedding provider for testing vector search paths.
    struct StubEmbedding;
    #[async_trait]
    impl EmbeddingProvider for StubEmbedding {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![1.0, 0.0, 0.0, 0.0])
        }
        fn dimensions(&self) -> usize {
            4
        }
    }

    fn test_server_with_embeddings() -> MemoryMcpServer {
        MemoryMcpServer::new(Arc::new(StubMemory), Some(Arc::new(StubEmbedding)))
    }

    #[test]
    fn server_constructs_with_embeddings() {
        let _server = test_server_with_embeddings();
    }

    #[test]
    fn server_constructs() {
        let _server = test_server();
    }

    // ── Whose memories a call reaches ───────────────────────────────────────

    use pond_core::shared::domain::orchestration::DelegationAuthority;
    use pond_core::shared::services::turn_authority::TurnAuthorityLease;
    use pond_core::user_data::domain::profile::{EXEMPLAR_OWNER_ID, SECOND_EXEMPLAR_OWNER_ID};
    use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;

    const AMINA: &str = EXEMPLAR_OWNER_ID;
    const OTIENO: &str = SECOND_EXEMPLAR_OWNER_ID;

    /// One memory per member and one shared row, every one embedded so semantic recall is live.
    struct Pond {
        repo: Arc<MockMemoryRepository>,
        turns: Arc<TurnAuthorityRegistry>,
        children: Arc<ChildSessionRegistry>,
    }

    impl Pond {
        async fn new() -> Self {
            let repo = Arc::new(MockMemoryRepository::new());
            for (id, owner, content) in [
                ("amina-1", Some(AMINA), "Amina is allergic to peanuts"),
                ("otieno-1", Some(OTIENO), "Otieno is allergic to shellfish"),
                ("shared-1", None, "The house is allergic to loud alarms"),
            ] {
                let mut fragment = MemoryFragment::from_chat(
                    id.into(),
                    owner.map(str::to_string),
                    None,
                    content.into(),
                );
                fragment.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
                repo.add(fragment).await.unwrap();
            }
            Self {
                repo,
                turns: Arc::new(TurnAuthorityRegistry::new()),
                children: Arc::new(ChildSessionRegistry::new()),
            }
        }

        fn server(&self, embedded: bool) -> MemoryMcpServer {
            let embedder: Option<Arc<dyn EmbeddingProvider + Send + Sync>> = if embedded {
                Some(Arc::new(StubEmbedding))
            } else {
                None
            };
            MemoryMcpServer::new(self.repo.clone(), embedder)
                .with_registries(self.turns.clone(), self.children.clone())
        }

        /// A live turn; hold the lease, or the call is unresolved and every check passes vacuously.
        fn turn(&self, engine: &str, scope: ProfileScope) -> TurnAuthorityLease {
            self.turns.publish(
                engine,
                DelegationAuthority::for_turn(
                    format!("giap-{engine}"),
                    scope,
                    ["giap-memory__recall_memories"],
                ),
                Default::default(),
            )
        }

        async fn owner_of(&self, id: &str) -> Option<Option<String>> {
            self.repo
                .get_in_scope(id, &ProfileScope::Household)
                .await
                .unwrap()
                .map(|f| f.profile_id)
        }
    }

    fn meta_for(session: &str) -> Meta {
        let mut m = Meta::new();
        m.0.insert(
            crate::session_meta::SESSION_ID_META_KEY.to_string(),
            serde_json::Value::String(session.to_string()),
        );
        m
    }

    fn text(result: &CallToolResult) -> String {
        format!("{:?}", result.content)
    }

    fn recall(query: Option<&str>) -> RecallMemoriesParams {
        RecallMemoriesParams {
            query: query.map(str::to_string),
            ..Default::default()
        }
    }

    fn save(content: &str, about: &str) -> SaveMemoryParams {
        SaveMemoryParams {
            content: content.into(),
            about: Some(about.into()),
            ..Default::default()
        }
    }

    fn forget(what: &str) -> ForgetMemoryParams {
        ForgetMemoryParams {
            what: Some(what.into()),
            ..Default::default()
        }
    }

    /// Every live row in the pond, whoever owns it.
    async fn live(pond: &Pond) -> Vec<MemoryFragment> {
        let now = chrono::Utc::now();
        pond.repo
            .search_recent(&ProfileScope::Household, 50)
            .await
            .unwrap()
            .into_iter()
            .filter(|f| f.is_shown(now))
            .collect()
    }

    #[tokio::test]
    async fn a_member_recalls_their_own_and_shared_memories_never_anothers() {
        let pond = Pond::new().await;
        let _turn = pond.turn("engine-amina", ProfileScope::Owner(AMINA.into()));
        let meta = meta_for("engine-amina");

        // Recent, keyword and semantic recall each run their own query, so each is checked.
        for (embedded, query) in [
            (false, None),
            (false, Some("allergic")),
            (true, Some("allergic")),
        ] {
            let got = text(
                &pond
                    .server(embedded)
                    .run_recall(&meta, recall(query))
                    .await
                    .unwrap(),
            );
            assert!(
                got.contains("peanuts"),
                "{query:?}: Amina lost her own memory: {got}"
            );
            assert!(
                got.contains("loud alarms"),
                "{query:?}: Amina lost the shared memory: {got}"
            );
            assert!(
                !got.contains("shellfish"),
                "{query:?}: Amina's turn read Otieno's memory: {got}"
            );
        }

        // Vacuity control: a household turn reaches all three, so the miss above is the scope's.
        let _household = pond.turn("engine-household", ProfileScope::Household);
        let got = text(
            &pond
                .server(true)
                .run_recall(&meta_for("engine-household"), recall(Some("allergic")))
                .await
                .unwrap(),
        );
        assert!(
            got.contains("shellfish") && got.contains("peanuts"),
            "{got}"
        );
    }

    #[tokio::test]
    async fn a_helper_recalls_at_the_scope_it_was_bound_with() {
        let pond = Pond::new().await;
        let _child = pond.children.bind(
            "engine-helper",
            "giap-otieno-task",
            ProfileScope::Owner(OTIENO.into()),
        );
        let got = text(
            &pond
                .server(false)
                .run_recall(&meta_for("engine-helper"), recall(None))
                .await
                .unwrap(),
        );
        assert!(
            got.contains("shellfish") && got.contains("loud alarms"),
            "{got}"
        );
        assert!(
            !got.contains("peanuts"),
            "Otieno's helper read Amina's memory: {got}"
        );
    }

    #[tokio::test]
    async fn a_whole_profile_reads_as_headings_in_profile_order() {
        let pond = Pond::new().await;
        for (id, about, content) in [
            ("l1", About::Likes, "Amina likes mint tea."),
            ("n1", About::Name, "Amina goes by Mama Amina."),
            ("i1", About::Instruction, "Amina wants short answers."),
        ] {
            let mut row =
                MemoryFragment::from_chat(id.into(), Some(AMINA.into()), None, content.into());
            file_under(&mut row, about);
            pond.repo.add(row).await.unwrap();
        }

        let _turn = pond.turn("engine-test", ProfileScope::Owner(AMINA.into()));
        let res = pond
            .server(false)
            .run_recall(&meta_for("engine-test"), recall(None))
            .await
            .unwrap();
        let got = text(&res);

        let at = |needle: &str| {
            got.find(needle)
                .unwrap_or_else(|| panic!("{needle:?} is not in {got}"))
        };
        assert!(at("Standing instructions") < at("Who they are"));
        assert!(at("Who they are") < at("Preferences"));
        assert!(at("Amina wants short answers.") < at("Amina goes by Mama Amina."));
        assert!(!got.contains("[id:"), "a profile carries no ids: {got}");
    }

    #[tokio::test]
    async fn a_save_is_kept_only_when_it_is_a_fact_about_the_member() {
        let pond = Pond::new().await;
        let _amina = pond.turn("engine-amina", ProfileScope::Owner(AMINA.into()));
        let server = pond.server(false);
        let meta = meta_for("engine-amina");
        let before = live(&pond).await.len();

        for refused in [
            SaveMemoryParams {
                content: "Amina likes jazz.".into(),
                ..Default::default()
            },
            save("Amina likes jazz.", "music taste"),
            save("I like jazz.", "likes"),
            save("Amina asked about the weather in Kisumu.", "likes"),
            save("Amina sees the dentist on Tuesday.", "health"),
            save("", "likes"),
        ] {
            let got = text(&server.run_save(&meta, refused).await.unwrap());
            assert!(got.contains("Nothing was saved"), "{got}");
        }
        assert_eq!(live(&pond).await.len(), before, "a refused save was kept");

        let got = text(
            &server
                .run_save(&meta, save("Amina  likes jazz.", "likes"))
                .await
                .unwrap(),
        );
        assert!(got.contains("Remembered under Preferences"), "{got}");
        let kept = live(&pond).await;
        let row = kept
            .iter()
            .find(|f| f.content == "Amina likes jazz.")
            .expect("the fact, with its spacing tidied");
        assert_eq!(row.about, Some(About::Likes));
        assert_eq!(row.profile_id.as_deref(), Some(AMINA));
        assert_eq!(row.source, "mcp_tool");
    }

    #[tokio::test]
    async fn a_fact_must_name_the_member_it_is_filed_under() {
        use pond_core::user_data::domain::profile::CreateProfileRequest;
        use pond_core::user_data::mocks::mock_profile::MockProfileRepository;

        let pond = Pond::new().await;
        let profiles = Arc::new(MockProfileRepository::new());
        let member = profiles
            .create(CreateProfileRequest {
                display_name: "Wanjiru Kamau".into(),
                avatar_emoji: String::new(),
            })
            .await
            .unwrap();
        let _turn = pond.turn("engine-wanjiru", ProfileScope::Owner(member.id.clone()));
        let server = pond.server(false).with_profiles(profiles);
        let meta = meta_for("engine-wanjiru");

        let got = text(
            &server
                .run_save(&meta, save("The neighbour keeps bees.", "friend"))
                .await
                .unwrap(),
        );
        assert!(got.contains("Nothing was saved"), "{got}");
        assert!(got.contains("names Wanjiru Kamau"), "{got}");

        let got = text(
            &server
                .run_save(&meta, save("Wanjiru's neighbour keeps bees.", "friend"))
                .await
                .unwrap(),
        );
        assert!(got.contains("Remembered under People"), "{got}");
    }

    #[tokio::test]
    async fn a_new_single_valued_fact_replaces_the_old_and_a_repeat_is_not_saved_twice() {
        let pond = Pond::new().await;
        let _amina = pond.turn("engine-amina", ProfileScope::Owner(AMINA.into()));
        let server = pond.server(false);
        let meta = meta_for("engine-amina");

        server
            .run_save(&meta, save("Amina is vegetarian.", "diet"))
            .await
            .unwrap();
        let got = text(
            &server
                .run_save(&meta, save("amina is vegetarian", "diet"))
                .await
                .unwrap(),
        );
        assert!(got.contains("Already remembered"), "{got}");

        let got = text(
            &server
                .run_save(&meta, save("Amina eats fish but no other meat.", "diet"))
                .await
                .unwrap(),
        );
        assert!(got.contains("It replaces: Amina is vegetarian."), "{got}");
        let diets: Vec<String> = live(&pond)
            .await
            .into_iter()
            .filter(|f| f.about == Some(About::Diet))
            .map(|f| f.content)
            .collect();
        assert_eq!(
            diets,
            vec!["Amina eats fish but no other meat.".to_string()]
        );

        // A list fact closes nothing.
        for likes in ["Amina likes hiking.", "Amina likes bowling."] {
            server.run_save(&meta, save(likes, "likes")).await.unwrap();
        }
        let liked = live(&pond)
            .await
            .into_iter()
            .filter(|f| f.about == Some(About::Likes))
            .count();
        assert_eq!(liked, 2);
    }

    #[tokio::test]
    async fn a_note_to_self_is_the_members_capped_and_never_written_twice() {
        use pond_core::shared::domain::agent_note::LESSONS_KEPT;
        use pond_core::shared::mocks::mock_agent_notes::MockAgentNotes;

        let pond = Pond::new().await;
        let notes = Arc::new(MockAgentNotes::new());
        let _amina = pond.turn("engine-amina", ProfileScope::Owner(AMINA.into()));
        let _guest = pond.turn("engine-guest", ProfileScope::Guest);
        let server = pond.server(false).with_notes(notes.clone());
        let meta = meta_for("engine-amina");
        let note = |text: &str, kind: &str| NoteToSelfParams {
            text: text.into(),
            kind: Some(kind.into()),
            ..Default::default()
        };

        let got = text(
            &server
                .run_note(&meta, note(" Give prices in  shillings. ", "lesson"))
                .await
                .unwrap(),
        );
        assert!(
            got.contains("as a lesson: Give prices in shillings."),
            "{got}"
        );
        let got = text(
            &server
                .run_note(&meta, note("give prices in shillings", "lesson"))
                .await
                .unwrap(),
        );
        assert!(got.contains("Already noted"), "{got}");
        let got = text(&server.run_note(&meta, note("  ", "note")).await.unwrap());
        assert!(got.contains("Nothing was noted"), "{got}");
        let got = text(
            &server
                .run_note(&meta_for("engine-guest"), note("Check the pump.", "note"))
                .await
                .unwrap(),
        );
        assert!(got.contains(Refusal::Guest.message()), "{got}");

        for i in 0..LESSONS_KEPT + 2 {
            server
                .run_note(&meta, note(&format!("Lesson number {i}."), "lesson"))
                .await
                .unwrap();
        }
        let kept = notes.all().await;
        assert_eq!(kept.len(), LESSONS_KEPT);
        assert!(kept.iter().all(|n| n.profile_id.as_deref() == Some(AMINA)));
        assert!(
            !kept.iter().any(|n| n.text == "Give prices in shillings."),
            "the oldest made room"
        );
    }

    #[tokio::test]
    async fn forgetting_by_words_takes_the_one_row_they_fit_and_asks_when_several_fit() {
        let pond = Pond::new().await;
        let _amina = pond.turn("engine-amina", ProfileScope::Owner(AMINA.into()));
        let server = pond.server(false);
        let meta = meta_for("engine-amina");
        for likes in [
            "Amina likes hiking in Karura.",
            "Amina likes hiking up Longonot.",
        ] {
            server.run_save(&meta, save(likes, "likes")).await.unwrap();
        }

        let got = text(&server.run_forget(&meta, forget("hiking")).await.unwrap());
        assert!(got.contains("Nothing was forgotten: 2 memories"), "{got}");

        let got = text(
            &server
                .run_forget(&meta, forget("hiking Longonot"))
                .await
                .unwrap(),
        );
        assert!(
            got.contains("Forgotten: Amina likes hiking up Longonot."),
            "{got}"
        );

        let got = text(&server.run_forget(&meta, forget("sailing")).await.unwrap());
        assert!(got.contains("Nothing was forgotten"), "{got}");
        let got = text(
            &server
                .run_forget(&meta, ForgetMemoryParams::default())
                .await
                .unwrap(),
        );
        assert!(got.contains("Nothing was forgotten"), "{got}");
    }

    #[tokio::test]
    async fn a_call_with_no_member_behind_it_touches_nothing() {
        let pond = Pond::new().await;
        let _guest = pond.turn("engine-guest", ProfileScope::Guest);
        let ended = pond.turn("engine-ended", ProfileScope::Owner(AMINA.into()));
        drop(ended);
        let server = pond.server(true);

        for (meta, expected) in [
            (Meta::new(), Refusal::Unresolved),
            (meta_for("engine-unknown"), Refusal::Unresolved),
            (meta_for("engine-ended"), Refusal::Unresolved),
            (meta_for("engine-guest"), Refusal::Guest),
        ] {
            let got = text(
                &server
                    .run_recall(&meta, recall(Some("allergic")))
                    .await
                    .unwrap(),
            );
            assert!(got.contains(expected.message()), "{got}");
            assert!(
                !got.contains("allergic to"),
                "a refused recall carried a memory: {got}"
            );

            let got = text(
                &server
                    .run_save(&meta, save("Amina likes jazz.", "likes"))
                    .await
                    .unwrap(),
            );
            assert!(got.contains(expected.message()), "{got}");

            let got = text(&server.run_forget(&meta, forget("shared-1")).await.unwrap());
            assert!(got.contains(expected.message()), "{got}");
        }
        assert_eq!(
            pond.repo
                .search_recent(&ProfileScope::Household, 10)
                .await
                .unwrap()
                .len(),
            3,
            "a refused call saved or deleted a memory"
        );
    }

    #[tokio::test]
    async fn a_members_save_is_theirs_and_a_household_save_stays_shared() {
        let pond = Pond::new().await;
        let _amina = pond.turn("engine-amina", ProfileScope::Owner(AMINA.into()));
        let _household = pond.turn("engine-household", ProfileScope::Household);
        let server = pond.server(false);

        server
            .run_save(
                &meta_for("engine-amina"),
                save("Amina prefers tea.", "likes"),
            )
            .await
            .unwrap();
        server
            .run_save(
                &meta_for("engine-household"),
                save(
                    "The household keeps the wifi password on the fridge.",
                    "routine",
                ),
            )
            .await
            .unwrap();

        let saved = pond
            .repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        let owner = |content: &str| {
            saved
                .iter()
                .find(|f| f.content.contains(content))
                .unwrap_or_else(|| panic!("{content:?} was not saved"))
                .profile_id
                .clone()
        };
        assert_eq!(owner("prefers tea"), Some(AMINA.to_string()));
        assert_eq!(owner("wifi password"), None);
    }

    #[tokio::test]
    async fn a_member_cannot_forget_or_retire_anothers_memory_by_its_id() {
        let pond = Pond::new().await;
        let _amina = pond.turn("engine-amina", ProfileScope::Owner(AMINA.into()));
        let server = pond.server(false);
        let meta = meta_for("engine-amina");

        server.run_forget(&meta, forget("otieno-1")).await.unwrap();
        assert!(
            pond.owner_of("otieno-1").await.is_some(),
            "Amina deleted Otieno's memory"
        );
        server
            .run_forget(&meta, forget("Otieno is allergic to shellfish"))
            .await
            .unwrap();
        assert!(
            pond.owner_of("otieno-1").await.is_some(),
            "Amina deleted it by its words"
        );

        let mut replacing = save("Amina is allergic to peanuts and sesame.", "allergy");
        replacing.extra.insert(
            "supersedes".into(),
            serde_json::json!(["otieno-1", "amina-1"]),
        );
        server.run_save(&meta, replacing).await.unwrap();
        let retired: Vec<String> = pond
            .repo
            .superseded()
            .await
            .into_iter()
            .map(|(old, _)| old)
            .collect();
        assert_eq!(
            retired,
            vec!["amina-1".to_string()],
            "only her own row may be retired"
        );

        // Control: her own memory goes, so the refusals above are the scope's.
        server.run_forget(&meta, forget("amina-1")).await.unwrap();
        assert!(pond.owner_of("amina-1").await.is_none());
    }
}
