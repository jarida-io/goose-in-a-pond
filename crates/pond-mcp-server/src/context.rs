//! Context MCP Server — `search_context` and `get_recent_context` (PAI-8 P2).
//!
//! Read-only. There is no `ingest_context` tool and there will not be one: the
//! corpus is written by the ingest pipeline from sources the household
//! connected, and a tool that let the model write into it would let a prompt
//! injection plant a memo the assistant later quotes as fact.
//!
//! # The resolution, which is the whole substance
//!
//! An MCP tool handler has no access to the adapter's per-turn locals. What it
//! has is the caller's ENGINE session id, stamped into `_meta` by the engine and
//! un-forgeable by the model ([`crate::session_meta`]). That id resolves to a
//! [`ProfileScope`] through [`DraftAuthority::actor_for_engine_session`], and the
//! scope is what every read here is filtered by.
//!
//! Three inputs produce a refusal and they are the phase's boundary:
//!
//! 1. **No `_meta`.** No caller, so no scope, so nothing to show.
//! 2. **An unresolvable session** — one GIAP never chatted in, a subagent's own,
//!    or a turn that has ended. `actor_for_engine_session` answers `None` and
//!    `None` is a refusal, never a fallback to `Household`. A default of
//!    `Household` reached by ordering is the exact defect PAI-1 recorded: a
//!    `ChatService` built before the turn's scope resolved attributed a Guest's
//!    memory to the household.
//! 3. **A `Guest`.** PAI-8 invariant 2: an unidentified speaker sees no context
//!    items. None. Enforced here rather than only in the tool-group denylist,
//!    because a denylist is a list and this is the boundary.
//!
//! # Why the port is called `DraftAuthority`
//!
//! Because it is the only port that answers "who is speaking in this engine
//! session", `RepoDraftAuthority` already implements it against the identity
//! chain PAI-1 built, and a second implementation would be a second answer that
//! could disagree with the first — which is what
//! `identity_resolution::resolve_turn_scope`'s doc warns about. The name has
//! outgrown the type; renaming it is a tidy-up for whoever wires this, not a
//! reason to duplicate the resolution.

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

/// The MCP extension name.
///
/// **Three other places must spell this the same way**, and none of them is in
/// this crate: the catalog entry in `pond_core::mcp::domain::tool_group::TOOL_GROUPS`,
/// the guest denylist beside it, and the `register_builtin_extension` call in
/// `pond-adapters-goose`. An extension name the catalog does not carry is
/// treated as a user-added MCP server — never narrowed by selection, never
/// subtracted for a guest, never withheld from a subagent — so a typo here does
/// not fail, it widens. `crates/pond-core/tests/registration_matches_the_catalog.rs`
/// is what catches it, and it is the reason this is a const.
pub const CONTEXT_EXTENSION: &str = "giap-context";

/// Most items any one call will return, whatever the model asks for.
///
/// A tool result is re-prefilled into the prompt like everything else, and a
/// model that asks for 200 mail items on a 8K-class window has ended the
/// conversation rather than answered it.
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
    /// No caller could be resolved. **One string for every input that produces
    /// it** — no `_meta`, an unmapped session, a subagent's own session, a turn
    /// that has ended — because a caller that could tell them apart would
    /// eventually treat one of them as benign.
    Unresolved,
    /// The caller is an unidentified speaker.
    Guest,
}

impl Refusal {
    /// The text the model reads. Every branch ends by saying what to do instead:
    /// a refusal a 2-4B model cannot act on is one it retries verbatim.
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
    /// Every tool this server exposes, without constructing it or its deps.
    ///
    /// `tool_router()` is generated private to this module, so inventory code
    /// outside it could not reach the real definitions and resorted to scanning
    /// source text for `#[tool(` instead. This is the enumeration that scan was
    /// standing in for.
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

    /// Attach unified retrieval. Without it `recall` answers nothing rather than
    /// silently degrading to context-only results, which would make the tool's
    /// own description a lie.
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

    /// Install the caller resolution.
    ///
    /// `None` means **every** call is refused, not that every call is permitted.
    /// That is the opposite of what `DraftMcpServer` did before PAI-2 P5 found
    /// it: an absent authority made every decision unresolvable, and
    /// unresolvable under the default `PolicyMode::Audit` is *permitted*. There
    /// is no policy mode here — an unresolved caller has no scope, and a read
    /// with no scope is not a read this module knows how to narrow.
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

    /// The body of `search_context`, separated from the rmcp wrapper so tests
    /// can drive the decision rather than the transport.
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

        // Semantic when an embedder is wired, ranked on the context blend rather
        // than on raw cosine, so a near-future item can displace a marginally
        // more similar one from last month.
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

    /// The body of `recall`: one question answered across everything the pond
    /// knows, each line saying where it came from.
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

/// Render an answer or a refusal.
///
/// A refusal is a successful tool result carrying an explanation, not an MCP
/// protocol error: a protocol error is what makes a small model retry the
/// identical call.
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
//
// Same shape as `init_orchestrator_deps`, and for the same
// reason: `SpawnServerFn` is `fn(DuplexStream, DuplexStream)` — no parameters,
// no capture — so the repositories have to arrive through a global installed at
// startup.

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct ContextDeps {
    repo: Arc<dyn ContextRepository>,
    embedder: Option<Arc<dyn EmbeddingProvider + Send + Sync>>,
    /// Unified retrieval across memory, context and summaries (phase C). Absent
    /// on a pond with no embedder, where `recall` answers nothing.
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

/// Install the caller resolution. Absent, every call is refused — see
/// [`ContextMcpServer::with_authority`].
pub fn init_context_authority(authority: Option<Arc<dyn DraftAuthority>>) {
    let _ = CONTEXT_AUTHORITY.set(authority);
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_context_server(reader: DuplexStream, writer: DuplexStream) {
    // Missing deps = this path never initialised this extension (the voice/CLI
    // binary vs `serve` install different families). A skipped extension is a
    // logged, contained failure; a panic here took down every builtin server's
    // startup at once (2026-08-27, giap-context in the voice child).
    let Some(deps) = CONTEXT_DEPS.get() else {
        tracing::error!(
            "spawn_context_server called before init_context_deps — extension will not start"
        );
        return;
    };
    let server = ContextMcpServer::new(deps.repo.clone())
        .with_embedder(deps.embedder.clone())
        // Without this the `recall` tool is registered, offered to the model and
        // permanently answers nothing -- the exact reader-with-no-writer shape
        // this programme keeps recording.
        .with_retrieval(deps.retrieval.clone())
        .with_authority(CONTEXT_AUTHORITY.get().cloned().flatten());
    crate::serve_builtin(CONTEXT_EXTENSION, server, reader, writer);
}

#[cfg(test)]
mod tests {
    //! `the_extension_is_read_only` at the bottom of this module was dead from
    //! the day it was written until 2026-08. A merge left its `#[test]` stacked
    //! above the NEXT test's doc comment, so the attribute bound to
    //! `the_recall_tool_is_actually_handed_its_retrieval` instead: that test ran
    //! twice and reported two passes, while the read-only guard compiled as an
    //! ordinary private fn nothing ever called. The only signal was a
    //! `dead_code` warning among the crate's others, and the test count went UP,
    //! not down -- both of which read as healthy at a glance.
    //!
    //! Attributes and doc comments are both attributes to the parser and it
    //! accepts them in any order, so `#[test]` separated from its `fn` by prose
    //! is not an error. When editing here, check that every `#[test]` sits
    //! immediately above the `fn` it names, and that the run lists each test
    //! exactly once.

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

    /// A redactor that finds nothing. The redaction guards live in pond-core;
    /// what is under test here is the scope resolution.
    struct NoopRedactor;
    impl Redactor for NoopRedactor {
        fn redact(&self, text: &str, _level: RedactionLevel) -> Redacted {
            Redacted::unchanged(text)
        }
    }

    /// A read-only store holding one item per member.
    ///
    /// It applies `item_is_visible` rather than returning everything: a stub
    /// that ignored the scope would make every assertion below pass.
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
        async fn count_in_window(
            &self,
            _scope: &ProfileScope,
            _kind: SourceKind,
            _from: chrono::DateTime<chrono::Utc>,
            _to: chrono::DateTime<chrono::Utc>,
        ) -> anyhow::Result<u64> {
            Ok(0)
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

        // Vacuity control: the household sees both, so the assertions above are
        // about the scope and not about a store with one row in it.
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

    /// PAI-8 invariant 2, at the boundary rather than in a denylist.
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

    /// An unresolvable caller is refused, not defaulted to `Household`. Three
    /// inputs, one refusal.
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

        // (3) no authority installed at all -- the case that used to mean
        // "permitted" on the draft server.
        let server = ContextMcpServer::new(Arc::new(StubRepo::with_one_item_each()));
        assert_eq!(
            server
                .run_recent(&meta_for("engine-1"), RecentContextParams::default())
                .await
                .expect_err("no authority must mean no read, not every read"),
            Refusal::Unresolved
        );
    }

    /// Both refusals render as a successful tool result carrying an
    /// explanation, and neither leaks a row.
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

    /// `recall` must actually be wired, not merely registered.
    ///
    /// A tool that is offered to the model and always answers nothing is worse
    /// than an absent one: it burns a schema in every turn's prompt and teaches
    /// the model that asking is pointless. `spawn_context_server` is where that
    /// would silently happen, so this pins the builder call.
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

    /// The extension has no write tool, and must not grow one: a tool that let
    /// the model add to the corpus would let a prompt injection plant something
    /// the assistant later quotes as fact.
    ///
    /// It reads the PRODUCTION half of this file only. The test module below
    /// contains the same needle in string literals, and counting those made the
    /// first version of this guard report two handlers that do not exist -- a
    /// parser reading itself is the shape that turns a source guard into noise.
    #[test]
    fn the_extension_is_read_only() {
        const SRC: &str = include_str!("context.rs");
        let production = SRC
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields one");
        // Line comments out, so prose in a doc comment cannot satisfy this and a
        // commented-out tool cannot be counted. (Recorded vacuity shape 1.)
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
