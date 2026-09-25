//! Memory MCP server: recall, save and forget memory fragments.

use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::user_data::domain::memory::{
    MemoryEventKind, MemoryFragment, MemoryLifecycle, MemorySegment, MemoryTier,
};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorCode, ErrorData, Implementation, InitializeResult,
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

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct MemoryMcpServer {
    memory_repo: Arc<dyn MemoryRepository + Send + Sync>,
    embedding_provider: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
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
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Recall memories, optionally filtered by keyword (semantic when available). \
        Returns segment, importance, and IDs."
    )]
    async fn recall_memories(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<RecallMemoriesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.0.limit.unwrap_or(10) as usize;

        let filtered: Vec<_> = if let Some(ref query) = params.0.query {
            if let Some(ref emb) = self.embedding_provider {
                // A query, not a document -- see EmbeddingProvider::embed_query.
                match emb.embed_query(query).await {
                    Ok(query_vec) => {
                        match self
                            .memory_repo
                            .search_similar(&query_vec, &ProfileScope::Household, limit)
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
                                keyword_search(&self.memory_repo, query, limit).await?
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "recall_memories: embedding failed ({e}), falling back to keyword"
                        );
                        keyword_search(&self.memory_repo, query, limit).await?
                    }
                }
            } else {
                keyword_search(&self.memory_repo, query, limit).await?
            }
        } else {
            self.memory_repo
                .search_recent(&ProfileScope::Household, limit)
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
            return Ok(CallToolResult::success(vec![Content::text(full_result)]));
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "Save a memory. Pass supersedes=[ids] to replace and archive old memories."
    )]
    async fn save_memory(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SaveMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = uuid::Uuid::new_v4().to_string();

        let supersedes = params.0.supersedes.clone();

        let content = if !params.0.content.is_empty() {
            params.0.content.clone()
        } else {
            let user_msg = crate::last_user_message();
            if user_msg.is_empty() {
                return Ok(CallToolResult::success(vec![Content::text(
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
            .0
            .tags
            .unwrap_or_default()
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();

        let segment = params
            .0
            .segment
            .as_deref()
            .and_then(parse_memory_segment)
            .unwrap_or_else(|| auto_classify_segment(&content));

        let importance = params
            .0
            .importance
            .map(|i| i.clamp(0.0, 1.0))
            .unwrap_or_else(|| segment.default_importance());

        let tier = params
            .0
            .tier
            .as_deref()
            .and_then(parse_memory_tier)
            .unwrap_or_else(|| segment.default_tier());

        let decay_rate = tier.default_decay_rate();

        let embedding = if let Some(ref emb) = self.embedding_provider {
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

        let corrects = params.0.corrects.clone().filter(|s| !s.is_empty());

        let new_id = id.clone();
        let fragment = MemoryFragment {
            id,
            profile_id: None,
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
                let _ = self.memory_repo.mark_superseded(old_id, &new_id).await;
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
        Ok(CallToolResult::success(vec![Content::text(full_result)]))
    }

    #[tool(description = "Delete a specific memory by ID or by exact content match.")]
    async fn forget_memory(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ForgetMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(id) = &params.0.id {
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
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Memory {id} deleted."
            ))]));
        }

        if let Some(content) = &params.0.content {
            let memories = self
                .memory_repo
                .search_recent(&ProfileScope::Household, 100)
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
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "Memory deleted: {}",
                    found.content
                ))]));
            }

            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_dead_end(
                    "a memory with that exact content",
                    "Nothing was deleted. Call recall_memories to find the exact \
                     wording before trying to forget it again.",
                ),
            )]));
        }

        Ok(CallToolResult::success(vec![Content::text(
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
    query: &str,
    limit: usize,
) -> Result<Vec<MemoryFragment>, ErrorData> {
    let fragments = memory_repo
        .search_recent(&ProfileScope::Household, limit * 2) // fetch more to allow for filtering
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
}
