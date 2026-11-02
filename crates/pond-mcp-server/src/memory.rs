//! Memory MCP server: recall, save and forget memory fragments.

use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::shared::services::child_sessions::{process_child_sessions, ChildSessionRegistry};
use pond_core::shared::services::turn_authority::TurnAuthorityRegistry;
use pond_core::user_data::domain::memory::{
    MemoryEventKind, MemoryFragment, MemoryLifecycle, MemorySegment, MemoryTier,
};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
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

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RecallMemoriesParams {
    pub query: Option<String>,
    /// Max results, default 10.
    pub limit: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SaveMemoryParams {
    #[serde(default)]
    pub content: String,
    /// Comma-separated tags.
    pub tags: Option<String>,
    /// identity|preference|correction|relationship|project|knowledge|context; auto if omitted.
    pub segment: Option<String>,
    /// 0.0-1.0; defaults by segment.
    pub importance: Option<f32>,
    /// short|long|permanent; defaults by segment.
    pub tier: Option<String>,
    /// IDs of memories this replaces (archived).
    #[serde(default)]
    pub supersedes: Option<Vec<String>>,
    /// Wrong claim this corrects (correction segment).
    pub corrects: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ForgetMemoryParams {
    pub id: Option<String>,
    /// Exact content match, used when id is absent.
    pub content: Option<String>,
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
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Recall memories, optionally filtered by keyword (semantic when available). \
        Returns segment, importance, and IDs."
    )]
    async fn recall_memories(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<RecallMemoriesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_recall(&ctx.meta, params.0).await
    }

    #[tool(
        description = "Save a memory. Pass supersedes=[ids] to replace and archive old memories."
    )]
    async fn save_memory(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<SaveMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_save(&ctx.meta, params.0).await
    }

    #[tool(description = "Delete a specific memory by ID or by exact content match.")]
    async fn forget_memory(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<ForgetMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.run_forget(&ctx.meta, params.0).await
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
        let limit = params.limit.unwrap_or(10) as usize;

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

        let text = if filtered.is_empty() {
            crate::format::format_dead_end(
                "stored memories matching this",
                "Nothing is stored about it. Answer from this conversation or ask \
                 the user — do not search the web for a fact about them.",
            )
        } else {
            filtered
                .iter()
                .map(|f| {
                    let seg = f
                        .segment
                        .as_ref()
                        .map(|s| format!("{:?}", s).to_lowercase())
                        .unwrap_or_else(|| "—".to_string());
                    let imp = f
                        .importance
                        .map(|i| format!("{:.1}", i))
                        .unwrap_or_else(|| "—".to_string());
                    format!(
                        "[{}] [id:{}] [{}, {}] {}",
                        f.created_at.format("%Y-%m-%d"),
                        f.id,
                        seg,
                        imp,
                        f.content
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        if !filtered.is_empty() {
            let ui_memories: Vec<serde_json::Value> = filtered
                .iter()
                .map(|f| {
                    serde_json::json!({
                        "content": f.content,
                        "segment": f.segment.as_ref()
                            .map(|s| format!("{:?}", s).to_lowercase())
                            .unwrap_or_else(|| "unknown".to_string()),
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
        let id = uuid::Uuid::new_v4().to_string();

        let supersedes = params.supersedes.clone();

        let content = if !params.content.is_empty() {
            params.content.clone()
        } else {
            let user_msg = crate::last_user_message();
            if user_msg.is_empty() {
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "No content provided to save. Tell me what you'd like me to remember.",
                )]));
            }
            eprintln!(
                "[memory] empty content param, using user message: {:?}",
                user_msg
            );
            user_msg
        };
        let tag_list: Vec<String> = params
            .tags
            .unwrap_or_default()
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();

        let segment = params
            .segment
            .as_deref()
            .and_then(parse_memory_segment)
            .unwrap_or_else(|| auto_classify_segment(&content));

        let importance = params
            .importance
            .map(|i| i.clamp(0.0, 1.0))
            .unwrap_or_else(|| segment.default_importance());

        let tier = params
            .tier
            .as_deref()
            .and_then(parse_memory_tier)
            .unwrap_or_else(|| segment.default_tier());

        let decay_rate = tier.default_decay_rate();

        let embedder = self.embedding_provider.as_ref().filter(|e| e.available());
        let embedding = if let Some(emb) = embedder {
            match emb.embed(&content).await {
                Ok(vec) => {
                    tracing::debug!(dims = vec.len(), "save_memory: embedding generated");
                    Some(vec)
                }
                Err(e) => {
                    tracing::warn!("save_memory: embedding failed ({e}), saving without");
                    None
                }
            }
        } else {
            None
        };

        let corrects = params.corrects.clone().filter(|s| !s.is_empty());

        let new_id = id.clone();
        let fragment = MemoryFragment {
            id,
            // A member's save is theirs; a household turn's stays shared, as extraction files it.
            profile_id: scope.owner_id().map(str::to_string),
            session_id: None,
            content: content.clone(),
            embedding,
            source: "mcp_tool".to_string(),
            tags: tag_list,
            created_at: chrono::Utc::now(),
            segment: Some(segment.clone()),
            importance: Some(importance),
            tier: Some(tier.clone()),
            decay_rate: Some(decay_rate),
            access_count: 0,
            last_accessed_at: None,
            lifecycle: Some(MemoryLifecycle::Active),
            superseded_by: None,
            corrects,
        };

        self.memory_repo.add(fragment).await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to save memory: {}", e),
                None,
            )
        })?;

        let _ = self
            .memory_repo
            .log_event(MemoryEventKind::Written, &new_id, None, None)
            .await;

        if let Some(ref superseded_ids) = supersedes {
            for old_id in superseded_ids {
                // Only a row this caller could recall may be archived, never another member's.
                if let Ok(Some(_)) = self.memory_repo.get_in_scope(old_id, &scope).await {
                    let _ = self.memory_repo.mark_superseded(old_id, &new_id).await;
                }
            }
        }

        let seg_label = format!("{:?}", segment).to_lowercase();
        let tier_label = format!("{:?}", tier).to_lowercase();
        let plain_text = format!(
            "Memory saved ({seg_label}, importance={importance:.1}, tier={tier_label}): {content}"
        );
        let ui_data = serde_json::json!({
            "content": content,
            "segment": seg_label,
            "saved": true,
        });
        let hint = format!("[[[mcp-ui:memory_saved:{}]]]\n", ui_data);
        let full_result = format!("{}{}", hint, plain_text);
        Ok(CallToolResult::success(vec![ContentBlock::text(
            full_result,
        )]))
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
        if let Some(id) = &params.id {
            // Out of scope reads as missing, so another member's memory can't be deleted or probed.
            let found = self
                .memory_repo
                .get_in_scope(id, &scope)
                .await
                .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;
            if found.is_none() {
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    crate::format::format_dead_end(
                        "a memory with that id",
                        "Nothing was deleted. Call recall_memories to find the memory's id \
                         before trying to forget it again.",
                    ),
                )]));
            }
            self.memory_repo.delete(id).await.map_err(|e| {
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to delete: {}", e),
                    None,
                )
            })?;
            let _ = self
                .memory_repo
                .log_event(MemoryEventKind::Deleted, id, None, None)
                .await;
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Memory {id} deleted."
            ))]));
        }

        if let Some(content) = &params.content {
            let memories = self
                .memory_repo
                .search_recent(&scope, 100)
                .await
                .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))?;

            let lower = content.to_lowercase();
            if let Some(found) = memories.iter().find(|m| m.content.to_lowercase() == lower) {
                let id = found.id.clone();
                self.memory_repo.delete(&id).await.map_err(|e| {
                    ErrorData::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("Failed to delete: {}", e),
                        None,
                    )
                })?;
                let _ = self
                    .memory_repo
                    .log_event(MemoryEventKind::Deleted, &id, None, None)
                    .await;
                return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Memory deleted: {}",
                    found.content
                ))]));
            }

            return Ok(CallToolResult::success(vec![ContentBlock::text(
                crate::format::format_dead_end(
                    "a memory with that exact content",
                    "Nothing was deleted. Call recall_memories to find the exact \
                     wording before trying to forget it again.",
                ),
            )]));
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(
            "Provide either an 'id' or 'content' to identify the memory to forget.".to_string(),
        )]))
    }
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
                "GIAP Memory MCP server — save, recall, and forget memory fragments.\n\n\
                 Tools: recall_memories (keyword-filtered recall with segment metadata and IDs), \
                 save_memory (with optional segment/importance/tier; use 'supersedes' to replace \
                 old memories by ID), forget_memory (by ID or exact content match).\n\n\
                 When correcting a fact, recall the old memory first, then save the correction \
                 with supersedes=[old_id] to replace it.\n\n\
                 Memories are categorized by segment (identity, preference, correction, \
                 relationship, project, knowledge, context) with importance scoring and \
                 decay-based lifecycle management.",
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

// ── Memory helpers (public for use by other crates) ────────────────────────

pub fn parse_memory_segment(s: &str) -> Option<MemorySegment> {
    match s.to_lowercase().as_str() {
        "identity" => Some(MemorySegment::Identity),
        "preference" => Some(MemorySegment::Preference),
        "correction" => Some(MemorySegment::Correction),
        "relationship" => Some(MemorySegment::Relationship),
        "project" => Some(MemorySegment::Project),
        // The batch engine's habit bin. Accepted here so a routine row the
        // engine wrote can be read back by the same name the tool takes.
        "routine" => Some(MemorySegment::Routine),
        "knowledge" => Some(MemorySegment::Knowledge),
        "context" => Some(MemorySegment::Context),
        _ => None,
    }
}

pub fn parse_memory_tier(s: &str) -> Option<MemoryTier> {
    match s.to_lowercase().as_str() {
        "short" => Some(MemoryTier::Short),
        "long" => Some(MemoryTier::Long),
        "permanent" => Some(MemoryTier::Permanent),
        _ => None,
    }
}

/// Classify a memory's segment by keyword heuristics (no LLM: fast, deterministic).
pub fn auto_classify_segment(content: &str) -> MemorySegment {
    let lower = content.to_lowercase();

    // Correction indicators (highest priority)
    if lower.starts_with("actually")
        || lower.starts_with("no, ")
        || lower.starts_with("correction:")
        || lower.contains("that's wrong")
        || lower.contains("that's not right")
        || lower.contains("not correct")
    {
        return MemorySegment::Correction;
    }

    if lower.starts_with("my name is")
        || lower.starts_with("i am a ")
        || lower.starts_with("i'm a ")
        || lower.contains("i live in")
        || lower.contains("i work at")
        || lower.contains("i work as")
        || lower.contains("my job is")
        || lower.contains("my role is")
    {
        return MemorySegment::Identity;
    }

    if lower.contains("my wife")
        || lower.contains("my husband")
        || lower.contains("my partner")
        || lower.contains("my friend")
        || lower.contains("my boss")
        || lower.contains("my colleague")
        || lower.contains("my sister")
        || lower.contains("my brother")
        || lower.contains("my mother")
        || lower.contains("my father")
        || lower.contains("my son")
        || lower.contains("my daughter")
    {
        return MemorySegment::Relationship;
    }

    if lower.starts_with("i prefer")
        || lower.starts_with("i like")
        || lower.starts_with("i love")
        || lower.starts_with("i hate")
        || lower.starts_with("i don't like")
        || lower.contains("my favorite")
        || lower.contains("my favourite")
    {
        return MemorySegment::Preference;
    }

    if lower.contains("working on")
        || lower.contains("my project")
        || lower.contains("my goal")
        || lower.contains("deadline")
        || lower.contains("i'm building")
        || lower.contains("i'm developing")
    {
        return MemorySegment::Project;
    }

    // Context indicators (transient)
    if lower.starts_with("right now")
        || lower.starts_with("currently")
        || lower.starts_with("today ")
        || lower.contains("at the moment")
    {
        return MemorySegment::Context;
    }

    MemorySegment::Knowledge
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
    fn auto_classify_correction() {
        assert_eq!(
            auto_classify_segment("Actually, my name is Jerry"),
            MemorySegment::Correction
        );
        assert_eq!(
            auto_classify_segment("No, that's not right"),
            MemorySegment::Correction
        );
    }

    #[test]
    fn auto_classify_identity() {
        assert_eq!(
            auto_classify_segment("My name is Jerry"),
            MemorySegment::Identity
        );
        assert_eq!(
            auto_classify_segment("I work at Jarida"),
            MemorySegment::Identity
        );
    }

    #[test]
    fn auto_classify_relationship() {
        assert_eq!(
            auto_classify_segment("My wife loves gardening"),
            MemorySegment::Relationship
        );
    }

    #[test]
    fn auto_classify_preference() {
        assert_eq!(
            auto_classify_segment("I prefer dark mode"),
            MemorySegment::Preference
        );
        assert_eq!(
            auto_classify_segment("My favorite color is blue"),
            MemorySegment::Preference
        );
    }

    #[test]
    fn auto_classify_project() {
        assert_eq!(
            auto_classify_segment("I'm building a home automation system"),
            MemorySegment::Project
        );
    }

    #[test]
    fn auto_classify_context() {
        assert_eq!(
            auto_classify_segment("Right now I'm at the office"),
            MemorySegment::Context
        );
    }

    #[test]
    fn auto_classify_defaults_to_knowledge() {
        assert_eq!(
            auto_classify_segment("The capital of France is Paris"),
            MemorySegment::Knowledge
        );
    }

    #[test]
    fn parse_segment_valid() {
        assert_eq!(
            parse_memory_segment("identity"),
            Some(MemorySegment::Identity)
        );
        assert_eq!(
            parse_memory_segment("CORRECTION"),
            Some(MemorySegment::Correction)
        );
        assert_eq!(parse_memory_segment("unknown"), None);
    }

    #[test]
    fn parse_tier_valid() {
        assert_eq!(parse_memory_tier("short"), Some(MemoryTier::Short));
        assert_eq!(parse_memory_tier("PERMANENT"), Some(MemoryTier::Permanent));
        assert_eq!(parse_memory_tier("invalid"), None);
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
            limit: None,
        }
    }

    fn save(content: &str) -> SaveMemoryParams {
        SaveMemoryParams {
            content: content.into(),
            ..Default::default()
        }
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

            let got = text(&server.run_save(&meta, save("I like jazz")).await.unwrap());
            assert!(got.contains(expected.message()), "{got}");

            let forget = ForgetMemoryParams {
                id: Some("shared-1".into()),
                content: None,
            };
            let got = text(&server.run_forget(&meta, forget).await.unwrap());
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
            .run_save(&meta_for("engine-amina"), save("Amina prefers tea"))
            .await
            .unwrap();
        server
            .run_save(
                &meta_for("engine-household"),
                save("The wifi password is on the fridge"),
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

        let forget = |id: &str| ForgetMemoryParams {
            id: Some(id.into()),
            content: None,
        };
        server.run_forget(&meta, forget("otieno-1")).await.unwrap();
        assert!(
            pond.owner_of("otieno-1").await.is_some(),
            "Amina deleted Otieno's memory"
        );
        let by_content = ForgetMemoryParams {
            id: None,
            content: Some("Otieno is allergic to shellfish".into()),
        };
        server.run_forget(&meta, by_content).await.unwrap();
        assert!(
            pond.owner_of("otieno-1").await.is_some(),
            "Amina deleted it by its words"
        );

        let replacing = SaveMemoryParams {
            supersedes: Some(vec!["otieno-1".into(), "amina-1".into()]),
            ..save("Amina is allergic to peanuts and sesame")
        };
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
