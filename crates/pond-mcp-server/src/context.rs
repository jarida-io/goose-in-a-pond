//! Read-only context tools, scoped via `DraftAuthority` (the one who-is-speaking port) from the
//! `_meta` engine session. No `_meta`, an unresolved session or a Guest is refused, never widened.

use std::sync::Arc;

use pond_core::context::domain::ContextItem;
use pond_core::context::ports::ContextRepository;
use pond_core::context::retrieval;
use pond_core::context::retrieval_service::PersonalContextRetrieval;
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::security::ports::draft_authority::DraftAuthority;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::services::memory_relevance::keyword_terms;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorData, Implementation, InitializeResult, Meta,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;

/// Extension name; must match `TOOL_GROUPS`, the guest denylist and Goose's registration.
/// A name the catalog lacks is treated as a user MCP server, which widens rather than fails.
pub const CONTEXT_EXTENSION: &str = "giap-context";

/// Per-call item cap whatever the model asks: results are re-prefilled into a small window.
const MAX_LIMIT: usize = 20;
const DEFAULT_LIMIT: usize = 5;

// ── Parameter structs ───────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SearchContextParams {
    /// What to look for, in the user's own words.
    pub query: Option<String>,
    /// Max results, default 5, capped at 20.
    pub limit: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RecentContextParams {
    /// Max results, default 5, capped at 20.
    pub limit: Option<u32>,
}

/// Why a call was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No caller resolved; one message for every cause, so none can be treated as benign.
    Unresolved,
    /// The caller is an unidentified speaker.
    Guest,
}

impl Refusal {
    /// Model-facing text; each ends with what to do instead, or a small model retries verbatim.
    pub fn message(&self) -> &'static str {
        match self {
            Refusal::Unresolved => {
                "Personal context is not available in this conversation. Answer from what you \
                 already know and say you could not check."
            }
            Refusal::Guest => {
                "Personal context is only available once this conversation is identified as a \
                 member of the household. Answer directly instead."
            }
        }
    }
}

// ── MCP server ──────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ContextMcpServer {
    repo: Arc<dyn ContextRepository>,
    embedder: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    retrieval: Option<Arc<PersonalContextRetrieval>>,
    authority: Option<Arc<dyn DraftAuthority>>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ContextMcpServer {
    /// Every tool this server exposes, without constructing it; `tool_router()` is module-private.
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new(repo: Arc<dyn ContextRepository>) -> Self {
        Self {
            repo,
            embedder: None,
            retrieval: None,
            authority: None,
            tool_router: Self::tool_router(),
        }
    }

    /// Attaches unified retrieval; without it `recall` answers nothing, never context-only.
    pub fn with_retrieval(mut self, retrieval: Option<Arc<PersonalContextRetrieval>>) -> Self {
        self.retrieval = retrieval;
        self
    }

    pub fn with_embedder(
        mut self,
        embedder: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    ) -> Self {
        self.embedder = embedder;
        self
    }

    /// Installs the caller resolution; `None` refuses every call (there is no policy mode here).
    pub fn with_authority(mut self, authority: Option<Arc<dyn DraftAuthority>>) -> Self {
        self.authority = authority;
        self
    }

    /// Resolve the caller's scope, or say why not.
    async fn scope_for(&self, meta: &Meta) -> Result<ProfileScope, Refusal> {
        let authority = self.authority.as_ref().ok_or(Refusal::Unresolved)?;
        let session = crate::session_meta::session_from_meta(meta).ok_or(Refusal::Unresolved)?;
        let (scope, _source) = authority
            .actor_for_engine_session(&session)
            .await
            .ok_or(Refusal::Unresolved)?;
        if scope.excludes_everything() {
            return Err(Refusal::Guest);
        }
        Ok(scope)
    }

    /// The body of `search_context`, apart from the rmcp wrapper so tests can call it.
    pub async fn run_search(
        &self,
        meta: &Meta,
        params: SearchContextParams,
    ) -> Result<Vec<ContextItem>, Refusal> {
        let scope = self.scope_for(meta).await?;
        let limit = clamp_limit(params.limit);
        let query = params.query.unwrap_or_default();
        if query.trim().is_empty() {
            return Ok(self.recent(&scope, limit).await);
        }

        // Semantic hits rank on the context blend, not raw cosine, so recency can beat similarity.
        if let Some(embedder) = &self.embedder {
            // A query, not a document -- see EmbeddingProvider::embed_query.
            if let Ok(vector) = embedder.embed_query(&query).await {
                match self.repo.search_similar(&vector, &scope, limit).await {
                    Ok(hits) if !hits.is_empty() => {
                        let mut ranked: Vec<(ContextItem, Option<f32>)> = hits
                            .into_iter()
                            .map(|(item, score)| (item, Some(score)))
                            .collect();
                        retrieval::rank_by_relevance(&mut ranked, chrono::Utc::now());
                        return Ok(ranked.into_iter().map(|(item, _)| item).collect());
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "context semantic search failed"),
                }
            }
        }

        // Keyword fallback, over the same stopword filter memory recall uses.
        let terms = keyword_terms(&query);
        match self.repo.search_items(&terms, &scope, limit).await {
            Ok(items) if !items.is_empty() => Ok(items),
            Ok(_) => Ok(self.recent(&scope, limit).await),
            Err(e) => {
                tracing::warn!(error = %e, "context keyword search failed");
                Ok(vec![])
            }
        }
    }

    /// The body of `recall`: answers across all sources, each line carrying its provenance.
    pub async fn run_recall(
        &self,
        meta: &Meta,
        query: &str,
        limit: usize,
    ) -> Result<Vec<String>, Refusal> {
        let scope = self.scope_for(meta).await?;
        let Some(retrieval) = &self.retrieval else {
            return Ok(vec![]);
        };
        Ok(retrieval
            .recall(query, &scope, clamp_limit(Some(limit as u32)))
            .await
            .into_iter()
            .map(|r| r.labelled())
            .collect())
    }

    /// The body of `get_recent_context`.
    pub async fn run_recent(
        &self,
        meta: &Meta,
        params: RecentContextParams,
    ) -> Result<Vec<ContextItem>, Refusal> {
        let scope = self.scope_for(meta).await?;
        Ok(self.recent(&scope, clamp_limit(params.limit)).await)
    }

    async fn recent(&self, scope: &ProfileScope, limit: usize) -> Vec<ContextItem> {
        match self.repo.recent_items(scope, limit).await {
            Ok(items) => items,
            Err(e) => {
                tracing::warn!(error = %e, "context recent read failed");
                vec![]
            }
        }
    }

    #[tool(
        description = "Search the member's incoming personal context (messages, calendar, \
        documents, sensor observations from connected sources) -- what has happened or is \
        coming up. What the user told you directly lives in `recall_memories`."
    )]
    async fn search_context(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<SearchContextParams>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(to_result(self.run_search(&ctx.meta, params.0).await))
    }

    #[tool(
        description = "Recall anything this household knows about a question -- what the member \
        told you, what this pond observed, and summarised past conversations. Prefer it when \
        you do not know which would answer. Lines carry provenance; pass it on, never present \
        a summary as the member's words."
    )]
    async fn recall(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<SearchContextParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("recall");
        let query = params.0.query.clone().unwrap_or_default();
        let limit = params.0.limit.unwrap_or(5) as usize;
        match self.run_recall(&ctx.meta, &query, limit).await {
            Ok(lines) if lines.is_empty() => Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_no_results(
                    &format!("anything about '{query}'"),
                    &["giap-context__get_recent_context"],
                ),
            )])),
            Ok(lines) => Ok(CallToolResult::success(vec![Content::text(
                lines.join("\n"),
            )])),
            Err(refusal) => Ok(to_result(Err(refusal))),
        }
    }

    #[tool(
        description = "The most recent personal context items for this household member, newest \
        first. Use it to answer 'what have I missed' or 'what is coming up'."
    )]
    async fn get_recent_context(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<RecentContextParams>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(to_result(self.run_recent(&ctx.meta, params.0).await))
    }
}

fn clamp_limit(requested: Option<u32>) -> usize {
    requested
        .map(|l| l as usize)
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT)
}

/// Renders a refusal as a successful result, not a protocol error, which small models retry.
fn to_result(outcome: Result<Vec<ContextItem>, Refusal>) -> CallToolResult {
    let text = match outcome {
        Err(refusal) => refusal.message().to_string(),
        Ok(items) if items.is_empty() => {
            "No personal context matched. Nothing may be connected yet -- say so rather than \
             guessing."
                .to_string()
        }
        Ok(items) => {
            let lines: Vec<String> = items.iter().map(retrieval::render_line).collect();
            lines.join("\n")
        }
    };
    CallToolResult::success(vec![Content::text(text)])
}

#[tool_handler]
impl ServerHandler for ContextMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                CONTEXT_EXTENSION,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Personal Context MCP server — what has arrived for this household \
                 member.\n\n\
                 These are things that came IN: messages, calendar entries, documents, sensor \
                 observations. They are not things the user told you, which live in \
                 `recall_memories`. You can only read; nothing here writes.",
            )
    }
}

// ── Static deps + spawn function for Goose's builtin registry ───────────────
// `SpawnServerFn` takes no captures, so deps arrive through globals installed at startup.

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct ContextDeps {
    repo: Arc<dyn ContextRepository>,
    embedder: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    /// Unified retrieval across memory, context and summaries; `None` without an embedder.
    retrieval: Option<Arc<PersonalContextRetrieval>>,
}

static CONTEXT_DEPS: OnceLock<ContextDeps> = OnceLock::new();
static CONTEXT_AUTHORITY: OnceLock<Option<Arc<dyn DraftAuthority>>> = OnceLock::new();

/// Install the repository. Call once at startup, before the first turn.
pub fn init_context_deps(
    repo: Arc<dyn ContextRepository>,
    embedder: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    retrieval: Option<Arc<PersonalContextRetrieval>>,
) {
    let _ = CONTEXT_DEPS.set(ContextDeps {
        repo,
        embedder,
        retrieval,
    });
}

/// Installs the caller resolution; `None` refuses every call.
pub fn init_context_authority(authority: Option<Arc<dyn DraftAuthority>>) {
    let _ = CONTEXT_AUTHORITY.set(authority);
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_context_server(reader: DuplexStream, writer: DuplexStream) {
    // Not every binary installs these deps; a panic here would take down every builtin server.
    let Some(deps) = CONTEXT_DEPS.get() else {
        tracing::error!(
            "spawn_context_server called before init_context_deps — extension will not start"
        );
        return;
    };
    let server = ContextMcpServer::new(deps.repo.clone())
        .with_embedder(deps.embedder.clone())
        .with_retrieval(deps.retrieval.clone())
        .with_authority(CONTEXT_AUTHORITY.get().cloned().flatten());
    crate::serve_builtin(CONTEXT_EXTENSION, server, reader, writer);
}

#[cfg(test)]
mod tests {
    //! Keep each `#[test]` right above its own `fn`; a stray one silently binds to the next fn.

    use super::*;
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use pond_core::context::domain::{ContextItem, ContextSource, ItemKind, ItemParts, SourceKind};
    use pond_core::context::retention::ContextRetention;
    use pond_core::context::scope::item_is_visible;
    use pond_core::security::domain::redaction::{Redacted, RedactionLevel};
    use pond_core::security::ports::policy::{PolicyDecision, PolicyMode};
    use pond_core::security::ports::redactor::Redactor;
    use pond_core::user_data::domain::profile::{EXEMPLAR_OWNER_ID, SECOND_EXEMPLAR_OWNER_ID};
    use pond_core::user_data::domain::session::IdentificationSource;
    use serde_json::Value;

    /// A redactor that finds nothing; redaction is tested in pond-core.
    struct NoopRedactor;
    impl Redactor for NoopRedactor {
        fn redact(&self, text: &str, _level: RedactionLevel) -> Redacted {
            Redacted::unchanged(text)
        }
    }

    /// One item per member, filtered by `item_is_visible`: an unscoped stub would pass vacuously.
    struct StubRepo {
        items: Vec<ContextItem>,
    }

    impl StubRepo {
        fn with_one_item_each() -> Self {
            let items = [EXEMPLAR_OWNER_ID, SECOND_EXEMPLAR_OWNER_ID]
                .iter()
                .enumerate()
                .map(|(n, owner)| {
                    ContextItem::from_parts(
                        &NoopRedactor,
                        ItemParts {
                            id: format!("i{n}"),
                            source_id: format!("s{n}"),
                            external_id: format!("e{n}"),
                            profile_id: (*owner).to_string(),
                            source_kind: SourceKind::Voice,
                            kind: ItemKind::Message,
                            occurred_at: Utc::now(),
                            ingested_at: Utc::now(),
                            title: format!("{owner} subject"),
                            body: format!("the dentist rang for {owner}"),
                            participants: vec![],
                            stored_sensitivity: None,
                            embedding: None,
                        },
                    )
                    .expect("valid item")
                })
                .collect();
            Self { items }
        }

        fn visible(&self, scope: &ProfileScope, limit: usize) -> Vec<ContextItem> {
            self.items
                .iter()
                .filter(|i| item_is_visible(i, scope))
                .take(limit)
                .cloned()
                .collect()
        }
    }

    #[async_trait]
    impl ContextRepository for StubRepo {
        async fn upsert_source(&self, _source: &ContextSource) -> anyhow::Result<()> {
            Ok(())
        }
        async fn get_source(
            &self,
            _id: &str,
            _scope: &ProfileScope,
        ) -> anyhow::Result<Option<ContextSource>> {
            Ok(None)
        }
        async fn list_sources(&self, _scope: &ProfileScope) -> anyhow::Result<Vec<ContextSource>> {
            Ok(vec![])
        }
        async fn disconnect_source(&self, _id: &str, _scope: &ProfileScope) -> anyhow::Result<u64> {
            Ok(0)
        }
        async fn save_item(&self, _item: &ContextItem) -> anyhow::Result<()> {
            Ok(())
        }
        async fn recent_items(
            &self,
            scope: &ProfileScope,
            limit: usize,
        ) -> anyhow::Result<Vec<ContextItem>> {
            Ok(self.visible(scope, limit))
        }
        async fn search_items(
            &self,
            keywords: &[String],
            scope: &ProfileScope,
            limit: usize,
        ) -> anyhow::Result<Vec<ContextItem>> {
            Ok(self
                .visible(scope, limit)
                .into_iter()
                .filter(|i| {
                    let hay = format!("{} {}", i.title(), i.body()).to_lowercase();
                    keywords.iter().any(|k| hay.contains(&k.to_lowercase()))
                })
                .collect())
        }
        async fn search_similar(
            &self,
            _query_embedding: &[f32],
            _scope: &ProfileScope,
            _limit: usize,
        ) -> anyhow::Result<Vec<(ContextItem, f32)>> {
            Ok(vec![])
        }
        async fn search_unembedded(&self, _limit: usize) -> anyhow::Result<Vec<ContextItem>> {
            Ok(vec![])
        }
        async fn update_embedding(&self, _id: &str, _embedding: &[f32]) -> anyhow::Result<()> {
            Ok(())
        }
        async fn count_for_profile(&self, _profile_id: &str) -> anyhow::Result<u64> {
            Ok(0)
        }
        async fn purge_expired(
            &self,
            _retention: &ContextRetention,
            _now: DateTime<Utc>,
        ) -> anyhow::Result<u64> {
            Ok(0)
        }
    }

    struct StubAuthority {
        actor: Option<ProfileScope>,
    }

    #[async_trait]
    impl DraftAuthority for StubAuthority {
        async fn policy_mode(&self) -> PolicyMode {
            PolicyMode::Enforce
        }
        async fn actor_for_engine_session(
            &self,
            _engine_session_id: &str,
        ) -> Option<(ProfileScope, IdentificationSource)> {
            self.actor
                .clone()
                .map(|s| (s, IdentificationSource::Explicit))
        }
        async fn audit(&self, _session: &str, _action: &str, _decision: &PolicyDecision) {}
    }

    fn meta_for(session: &str) -> Meta {
        let mut m = Meta::new();
        m.0.insert(
            crate::session_meta::SESSION_ID_META_KEY.to_string(),
            Value::String(session.to_string()),
        );
        m
    }

    fn server_for(actor: Option<ProfileScope>) -> ContextMcpServer {
        ContextMcpServer::new(Arc::new(StubRepo::with_one_item_each()))
            .with_authority(Some(Arc::new(StubAuthority { actor })))
    }

    #[tokio::test]
    async fn an_owner_sees_only_their_own_context() {
        let server = server_for(Some(ProfileScope::Owner(EXEMPLAR_OWNER_ID.into())));
        let items = server
            .run_recent(&meta_for("engine-1"), RecentContextParams::default())
            .await
            .expect("an identified member may read their own context");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].profile_id(), EXEMPLAR_OWNER_ID);

        let searched = server
            .run_search(
                &meta_for("engine-1"),
                SearchContextParams {
                    query: Some("dentist".into()),
                    limit: None,
                },
            )
            .await
            .expect("search is permitted");
        assert_eq!(searched.len(), 1, "search is not scoped");
        assert_eq!(searched[0].profile_id(), EXEMPLAR_OWNER_ID);

        // Vacuity control: the household sees both items.
        let household = server_for(Some(ProfileScope::Household));
        assert_eq!(
            household
                .run_recent(&meta_for("engine-1"), RecentContextParams::default())
                .await
                .expect("household read")
                .len(),
            2,
            "the fixture holds one item per member; if it does not, the owner assertion above \
             proves nothing"
        );
    }

    #[tokio::test]
    async fn a_guest_is_refused_by_both_tools() {
        let server = server_for(Some(ProfileScope::Guest));
        assert_eq!(
            server
                .run_recent(&meta_for("engine-1"), RecentContextParams::default())
                .await
                .expect_err("a guest reached the corpus"),
            Refusal::Guest
        );
        assert_eq!(
            server
                .run_search(
                    &meta_for("engine-1"),
                    SearchContextParams {
                        query: Some("dentist".into()),
                        limit: None,
                    }
                )
                .await
                .expect_err("a guest reached the corpus through search"),
            Refusal::Guest
        );
    }

    #[tokio::test]
    async fn an_unresolvable_caller_is_refused_rather_than_defaulted() {
        // (1) no `_meta` at all.
        let server = server_for(Some(ProfileScope::Household));
        assert_eq!(
            server
                .run_recent(&Meta::new(), RecentContextParams::default())
                .await
                .expect_err("a call with no engine session has no caller"),
            Refusal::Unresolved
        );

        // (2) a session the authority cannot resolve.
        let server = server_for(None);
        assert_eq!(
            server
                .run_recent(&meta_for("engine-1"), RecentContextParams::default())
                .await
                .expect_err("an unmapped session has no caller"),
            Refusal::Unresolved
        );

        // (3) no authority installed at all.
        let server = ContextMcpServer::new(Arc::new(StubRepo::with_one_item_each()));
        assert_eq!(
            server
                .run_recent(&meta_for("engine-1"), RecentContextParams::default())
                .await
                .expect_err("no authority must mean no read, not every read"),
            Refusal::Unresolved
        );
    }

    #[test]
    fn a_refusal_reads_as_an_instruction_and_carries_no_items() {
        for refusal in [Refusal::Unresolved, Refusal::Guest] {
            let rendered = to_result(Err(refusal.clone()));
            let text = format!("{:?}", rendered.content);
            assert!(
                text.contains(refusal.message()),
                "the rendered refusal is not the one the model was meant to read: {text}"
            );
            assert!(
                refusal.message().contains("Answer"),
                "{refusal:?} does not say what to do instead, so a small model will retry the \
                 identical call"
            );
            assert!(
                !text.contains("dentist"),
                "a refusal carried an item: {text}"
            );
        }
    }

    #[tokio::test]
    async fn the_limit_is_clamped_in_both_directions() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(1_000)), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(3)), 3);

        let server = server_for(Some(ProfileScope::Household));
        let items = server
            .run_recent(
                &meta_for("engine-1"),
                RecentContextParams {
                    limit: Some(10_000),
                },
            )
            .await
            .expect("household read");
        assert!(items.len() <= MAX_LIMIT);
    }

    /// An always-empty `recall` costs a schema per prompt and teaches the model not to ask.
    #[test]
    fn the_recall_tool_is_actually_handed_its_retrieval() {
        const SRC: &str = include_str!("context.rs");
        let spawn = SRC
            .split("pub fn spawn_context_server")
            .nth(1)
            .expect("spawn_context_server not found");
        let body = &spawn[..spawn.find("tokio::spawn").unwrap_or(spawn.len())];
        assert!(
            body.contains(".with_retrieval("),
            "spawn_context_server does not hand the server its retrieval, so the \
             recall tool is registered and permanently empty"
        );
    }

    /// A write tool would let a prompt injection plant "facts". Reads only the production half:
    /// the tests hold the same needle in string literals.
    #[test]
    fn the_extension_is_read_only() {
        const SRC: &str = include_str!("context.rs");
        let production = SRC
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields one");
        // Strip line comments so prose or commented-out tools can't count.
        let stripped: String = production
            .lines()
            .map(|line| match line.find("//") {
                Some(i) => &line[..i],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n");

        const NEEDLE: &str = "params: Parameters<";
        let handlers: Vec<&str> = stripped
            .match_indices(NEEDLE)
            .map(|(i, _)| {
                let before = &stripped[..i];
                let decl = before
                    .rfind("async fn ")
                    .expect("a Parameters argument outside any fn");
                before[decl + "async fn ".len()..]
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .trim()
            })
            .collect();
        assert!(
            !handlers.is_empty(),
            "the parser found no tool handlers at all, so this guard is asserting nothing. It \
             looks for `{NEEDLE}`, which is how an rmcp tool takes its arguments."
        );
        assert_eq!(
            handlers,
            vec!["search_context", "recall", "get_recent_context"],
            "the context extension's tool surface changed. It is read-only on purpose; a write \
             tool here is a prompt-injection path into a corpus the assistant treats as fact."
        );
    }
}
