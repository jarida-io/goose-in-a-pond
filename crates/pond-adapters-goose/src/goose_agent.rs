use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use goose::agents::{Agent as GooseAgent, AgentConfig, ExtensionConfig, GoosePlatform};
use goose::config::GooseMode;
use goose::conversation::message::Message;
use goose::providers::base::Provider;
use goose::session::SessionManager;
use pond_core::mcp::ports::tools::tool_registry::ToolRegistryPort;
use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
use pond_core::models::ports::agent::{
    Agent as AgentPort, AgentRequest, AgentResponse, AgentStreamEvent, WarmupPhase,
};
use pond_core::models::ports::embedding::EmbeddingProvider;
use pond_core::models::ports::model_repository::ModelRepository;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::models::ports::token_counter::TokenCounter as PondTokenCounter;
use pond_core::models::services::context::context_budget::CompactionProfile;
use pond_core::models::services::context::context_governor::{
    ContextGovernor, ContextInputs, WindowResolution,
};
use pond_core::models::services::context::prefix_cache::{InvalidationReason, PrefixCacheState};
use pond_core::models::services::context::token_counting::HeuristicTokenCounter;
use pond_core::models::services::prompt_builder::build_prompt_partition;
use pond_core::prompts::PromptState;
use pond_core::user_data::domain::memory::{cosine_similarity, MemoryFragment};
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::prompt_extra::PromptExtraRepository;
use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::skill::UserSkillRepository;
use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::extension_manager::GiapGooseExtensionManager;
use crate::giap_registration::registered_extensions;
use pond_core::user_data::domain::profile::ProfileScope;

/// Template for a `prompt_style` with no DB row: the shipped `balanced` one.
fn fallback_prompt() -> &'static str {
    pond_core::prompts::builtin_template_content("balanced")
        .map(|(content, _)| content)
        .unwrap_or(pond_core::prompts::PROMPT_BALANCED)
}

/// Memory candidates fetched per injection slot: ranking can only pick from what it sees.
const MEMORY_CANDIDATE_FANOUT: usize = 8;

/// Candidate-pool floor, so a small `agent_memory_limit` still ranks beyond the newest rows.
const MEMORY_CANDIDATE_FLOOR: usize = 40;

/// Verbatim copy of Goose's private `MAX_TURNS_MESSAGE`: GIAP's only sign a turn hit the cap.
const GOOSE_MAX_TURNS_MESSAGE: &str = "I've reached the maximum number of actions I can do without user input. Would you like me to continue?";

/// Goose's verbatim empty-turn text, matched so GIAP can re-engage instead of showing it.
const GOOSE_EMPTY_TURN_MESSAGE: &str =
    "The model returned an empty response. Please resend your message to continue.";

/// Verbatim prefix of goose's notification each time its completeness check re-arms.
const GOOSE_GOAL_NOTIFICATION_PREFIX: &str = "Goal: ";

/// Max completeness-check re-arms per turn; bounds tool-call loops.
const MAX_GOAL_RECHECKS_PER_TURN: u32 = 2;

/// How long a tripped guard waits for the engine to wind down; too short loses the message.
const GUARD_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Max identical-argument calls to one tool per turn, not just consecutive: loops interleave.
const MAX_IDENTICAL_TOOL_CALLS_PER_TURN: usize = 3;

/// Max re-engagements after a turn with no text and no tool call; each is a full turn.
const MAX_EMPTY_TURN_REENGAGEMENTS: usize = 2;

/// Key-order-independent hash of a tool call's arguments. Sorts keys itself:
/// `serde_json/preserve_order` is on in this graph, so `Map` keeps insertion order.
fn canonical_args_fingerprint(args: Option<&serde_json::Map<String, serde_json::Value>>) -> u64 {
    use std::hash::{Hash, Hasher};

    /// Render a value with every object's keys in sorted order, at every depth.
    fn canonical(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::Value::String((*k).clone()).to_string());
                    out.push(':');
                    canonical(&map[*k], out);
                }
                out.push('}');
            }
            // Arrays keep their order: [1,2] and [2,1] are different arguments.
            serde_json::Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    canonical(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }

    let mut rendered = String::new();
    match args {
        None => {}
        Some(map) => canonical(&serde_json::Value::Object(map.clone()), &mut rendered),
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    rendered.hash(&mut hasher);
    hasher.finish()
}

/// GIAP's prompt tag names; any in visible output means the model echoed scaffolding.
const SCAFFOLD_TAGS: &[&str] = &[
    "<system-context>",
    "<user-message>",
    "<answer-contract>",
    "<memories>",
    "<turn-context>",
];

/// Whether visible output is GIAP's prompt scaffolding echoed back; handled like an empty turn.
fn looks_like_leaked_scaffold(text: &str) -> bool {
    let trimmed = text.trim();
    SCAFFOLD_TAGS.iter().any(|tag| trimmed.contains(tag))
}

/// Appended when re-engaging after an empty turn: an unchanged prompt gets the same reply.
const EMPTY_TURN_STEER: &str = "(Your previous attempt produced only internal reasoning and no reply. \
Do not reason further — either call the tool you already decided on, or write the answer directly.)";

/// Shown once the re-engagement budget is spent, in place of silence.
const EMPTY_TURN_EXHAUSTED_MESSAGE: &str =
    "I could not produce a response to that, even after retrying. \
This usually clears if you reword the question — or start a new chat if it keeps happening.";

/// Goose env knobs GIAP owns (`None` = unset); also the change signature gating `set_var`.
fn goose_env_knobs(provider: &str, effective_ctx: usize) -> [(&'static str, Option<String>); 5] {
    let local = matches!(provider, "local" | "gguf");
    [
        // Goose otherwise assumes 128K and compacts near 102K, far past the real KV cache.
        ("GOOSE_CONTEXT_LIMIT", Some(effective_ctx.to_string())),
        // Unset: goose's default threshold. Proactive only; reactive compaction ignores it.
        ("GOOSE_AUTO_COMPACT_THRESHOLD", None),
        // Unset: summarisation runs, at the cost of background LLM calls on the single slot.
        ("GOOSE_TOOL_PAIR_SUMMARIZATION", None),
        // GIAP owns empty-turn recovery; goose's retry resends an unchanged prompt, same reply.
        ("GOOSE_MAX_EMPTY_TURN_RETRIES", Some("0".to_string())),
        // Goose's 200K-char default lets one user-added MCP result fill the window. Cap: a quarter
        // of the context, in bytes at ~4 chars/token. Local only; HTTP windows aren't ours.
        (
            "GOOSE_MAX_TOOL_RESPONSE_SIZE",
            local.then(|| ((effective_ctx / 4) * 4).to_string()),
        ),
    ]
}

/// Tool groups; only `permitted` may be enabled or advertised, so guests never see withheld ones.
struct SessionGroups {
    /// In the prompt for this turn.
    loaded: Vec<String>,
    /// The ceiling. Never widened by anything the model can say.
    permitted: Vec<String>,
}

/// Adapter: the `Agent` port on the Goose framework.
pub struct GooseAdapter {
    agent: Arc<GooseAgent>,
    session_manager: Arc<SessionManager>,
    settings_repo: Arc<dyn SettingsRepository>,
    template_repo: Arc<dyn PromptTemplateRepository>,
    extras_repo: Arc<dyn PromptExtraRepository>,
    skill_repo: Arc<dyn UserSkillRepository>,
    memory_repo: Arc<dyn MemoryRepository>,
    /// Memory-search embedder; `None` (fastembed failed or disabled) falls back to keyword LIKE.
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    /// Only source of `ModelRecord.context_length` (governor rung 3); `None` only on CLI paths.
    model_repo: Option<Arc<dyn ModelRepository>>,
    llamafile_url: String,
    /// Resolves GGUF model paths under `$data_dir/models/gguf/` for local inference.
    data_dir: Option<PathBuf>,
    /// The same lock as `AppState.mesh_provider`, read live so enabling mesh needs no restart.
    mesh_provider: Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>,
    extension_manager: Arc<GiapGooseExtensionManager>,
    /// Tracks the last "chat_provider:chat_model" key we wired into Goose.
    last_provider_key: Mutex<String>,
    /// Last provider + config, kept for new sessions: Goose stores model config per session and
    /// an unconfigured one falls back to the global goose config.
    current_provider: Mutex<Option<(Arc<dyn Provider>, goose_providers::model::ModelConfig)>>,
    /// Sessions configured with the `last_provider_key` pair; cleared on provider/model change.
    provider_configured_sessions: Mutex<HashSet<String>>,
    /// Last `enable_thinking` stamped; separate so a change re-stamps without a provider rebuild.
    last_thinking_param: Mutex<Option<bool>>,
    /// Exported env-knob signature, so `set_var` runs only when a knob changes.
    last_env_signature: Mutex<String>,
    /// Budget-path token counter, built lazily; `None` = construction failed, use chars/4.
    token_counter: tokio::sync::OnceCell<Option<Arc<crate::token_counter::TiktokenCounter>>>,
    /// Last resolved window with its provider (the clamp needs both); never read back from env.
    last_window: Mutex<Option<(String, WindowResolution)>>,
    /// `Settings::compaction_verbatim_days`, cached so the per-turn trimmer skips a settings load.
    last_verbatim_days: Mutex<Option<u32>>,
    /// Per-turn [`GiapProviderShim`] controls: last-mile veto over prompt, turn context and tools.
    shim_controls: Arc<crate::provider_shim::ShimControls>,
    /// Live turns' [`DelegationAuthority`] by ENGINE session id, shared with `GooseOrchestrator`.
    /// Revoked on stream drop, so only a running turn can authorise `delegate`.
    turn_authorities: Arc<pond_core::shared::services::turn_authority::TurnAuthorityRegistry>,
    /// Maps GIAP session IDs → Goose session IDs (Goose auto-generates its own IDs).
    goose_session_map: Mutex<HashMap<String, String>>,
    /// Goose sessions whose GIAP builtin extensions are already loaded.
    loaded_sessions: Mutex<HashSet<String>>,
    /// Source of the prose tool list; `None` renders none (native tool schemas still apply).
    tool_registry: Option<Arc<dyn ToolRegistryPort>>,
    /// Extensions the user added via the REST API; exempt from the per-turn extension strip.
    user_extensions: Arc<tokio::sync::RwLock<HashSet<String>>>,
    /// Adds voice-mode prompt instructions; set by the CLI's `--voice`.
    voice_mode: std::sync::atomic::AtomicBool,
    /// Runtime capabilities of the currently loaded model.
    model_capabilities: Mutex<pond_core::models::domain::model_capabilities::ModelCapabilities>,
    /// Last prefix hash sent; a match skips `override_system_prompt()` so the KV cache survives.
    last_prefix_hash: Mutex<u64>,
    /// Served/invalidated state of the `last_prefix_hash` prefix. Kept apart so the compaction
    /// decision never runs inside the prompt-assembly lock.
    prefix_cache: Mutex<PrefixCacheState>,
    /// Read-only source of the rolling summary for the trimmer; `None` just skips the splice.
    giap_session_storage:
        Option<Arc<dyn pond_core::user_data::ports::session_storage::SessionStorage>>,
    /// Engine-reported prompt tokens per session (trimmer feedback); an Arc the stream can hold.
    last_prompt_tokens_arc: std::sync::OnceLock<Arc<Mutex<HashMap<String, u32>>>>,
    /// Tool set from the last `list_tools()`; cleared when extensions change.
    cached_tools: tokio::sync::RwLock<Option<std::collections::HashSet<String>>>,
    /// Sessions whose Goose default extensions are already stripped (once per session).
    defaults_stripped: Mutex<HashSet<String>>,
    /// Session id -> chosen tool groups, held fixed so the tools JSON (and KV prefix) don't churn.
    /// Persisted in `pond_system.db`, so a restart keeps groups the model enabled itself.
    session_tool_groups: tokio::sync::RwLock<HashMap<String, Vec<String>>>,
    /// Session id -> groups it may EVER hold; the enable hatch widens only within this.
    /// Not persisted: recomputed from scope on restore, as a stored boundary can go stale.
    session_permitted_groups: tokio::sync::RwLock<HashMap<String, Vec<String>>>,
    /// Group-description embeddings. No `Option` inside: a failed first try must not stick.
    group_embeddings: tokio::sync::OnceCell<Vec<(String, Vec<f32>)>>,
}

/// Byte cap on a buffered reasoning passage (memory bound); crossing it splits, never drops.
const REASONING_BUFFER_LIMIT: usize = 64 * 1024;

/// One reasoning passage, from however many pieces the provider sends (per token or block).
/// Written only via [`ReasoningCoalescer::push`], so a gated-off turn stores no reasoning.
#[derive(Default)]
struct ReasoningCoalescer {
    buf: String,
}

impl ReasoningCoalescer {
    /// Append fragments raw: a lone `" "` is a word gap. Only [`Self::flush`] normalises.
    fn push(&mut self, msg: &Message, emit: bool) {
        for fragment in GooseAdapter::reasoning_frames(msg, emit) {
            self.buf.push_str(&fragment);
        }
    }

    fn over_cap(&self) -> bool {
        self.buf.len() >= REASONING_BUFFER_LIMIT
    }

    /// Bytes currently buffered — for the over-cap log line only.
    fn len(&self) -> usize {
        self.buf.len()
    }

    /// Take the passage trimmed, or `None` if blank, so no empty frame is ever emitted.
    fn flush(&mut self) -> Option<String> {
        let passage = std::mem::take(&mut self.buf);
        let trimmed = passage.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(trimmed.to_string())
    }
}

impl GooseAdapter {
    /// Primary factory — all repos are injected by `pond-server/main.rs`.
    pub async fn new(
        settings_repo: Arc<dyn SettingsRepository>,
        template_repo: Arc<dyn PromptTemplateRepository>,
        extras_repo: Arc<dyn PromptExtraRepository>,
        skill_repo: Arc<dyn UserSkillRepository>,
        memory_repo: Arc<dyn MemoryRepository>,
        llamafile_url: String,
        data_dir: Option<PathBuf>,
        tool_registry: Option<Arc<dyn ToolRegistryPort>>,
    ) -> Result<Self> {
        let session_manager = Arc::new(SessionManager::instance());
        let permission_manager = goose::config::permission::PermissionManager::instance();

        let config = AgentConfig::new(
            session_manager.clone(),
            permission_manager,
            None,
            GooseMode::Auto,
            // Skip goose's session naming (an LLM call); GIAP titles sessions itself.
            true,
            GoosePlatform::GooseCli,
        );

        let agent = Arc::new(GooseAgent::with_config(config));

        // The manager resolves its engine session lazily by name, so it survives a wiped store.
        let extension_manager = Arc::new(GiapGooseExtensionManager::new(
            agent.clone(),
            session_manager.clone(),
        ));

        Ok(Self {
            agent,
            session_manager,
            settings_repo,
            template_repo,
            extras_repo,
            skill_repo,
            memory_repo,
            embedding_provider: None,
            model_repo: None,
            llamafile_url,
            data_dir,
            mesh_provider: Arc::new(tokio::sync::RwLock::new(None)),
            extension_manager,
            last_provider_key: Mutex::new(String::new()),
            current_provider: Mutex::new(None),
            provider_configured_sessions: Mutex::new(HashSet::new()),
            last_thinking_param: Mutex::new(None),
            last_env_signature: Mutex::new(String::new()),
            token_counter: tokio::sync::OnceCell::new(),
            last_window: Mutex::new(None),
            last_verbatim_days: Mutex::new(None),
            shim_controls: Arc::new(crate::provider_shim::ShimControls::default()),
            turn_authorities: Arc::new(
                pond_core::shared::services::turn_authority::TurnAuthorityRegistry::new(),
            ),
            goose_session_map: Mutex::new(HashMap::new()),
            loaded_sessions: Mutex::new(HashSet::new()),
            tool_registry,
            user_extensions: Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            voice_mode: std::sync::atomic::AtomicBool::new(false),
            model_capabilities: Mutex::new(
                pond_core::models::domain::model_capabilities::ModelCapabilities::default(),
            ),
            last_prefix_hash: Mutex::new(0),
            prefix_cache: Mutex::new(PrefixCacheState::new(0, std::time::Instant::now())),
            giap_session_storage: None,
            last_prompt_tokens_arc: std::sync::OnceLock::new(),
            cached_tools: tokio::sync::RwLock::new(None),
            defaults_stripped: Mutex::new(HashSet::new()),
            session_tool_groups: tokio::sync::RwLock::new(HashMap::new()),
            session_permitted_groups: tokio::sync::RwLock::new(HashMap::new()),
            group_embeddings: tokio::sync::OnceCell::new(),
        })
    }

    /// Makes prompt templates ask for short, TTS-friendly replies.
    pub fn set_voice_mode(&self, enabled: bool) {
        self.voice_mode
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Factory for tests and CLI one-shots: mock repos, llamafile at `host`.
    pub async fn with_llamafile(host: Option<&str>) -> Result<Self> {
        use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
        use pond_core::user_data::mocks::mock_prompt_extra::MockPromptExtraRepository;
        use pond_core::user_data::mocks::mock_prompt_template::MockPromptTemplateRepository;
        use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
        use pond_core::user_data::mocks::mock_skill::MockSkillRepository;

        let url = host.unwrap_or("http://127.0.0.1:8080").to_string();
        Self::new(
            Arc::new(MockSettingsRepository::default()),
            Arc::new(MockPromptTemplateRepository::default()),
            Arc::new(MockPromptExtraRepository::default()),
            Arc::new(MockSkillRepository::default()),
            Arc::new(MockMemoryRepository::default()),
            url,
            None,
            None, // tool_registry
        )
        .await
    }

    // ── Prefix-cache bookkeeping ──────────────────────────────────────────
    // Synchronous, guard dropped before return: safe to call from `async fn`s that `.await`.

    fn note_prefix_invalidated(&self, reason: InvalidationReason) {
        let mut state = self.prefix_cache.lock().unwrap_or_else(|e| e.into_inner());
        let served = state.turns_served;
        state.invalidate(reason);
        // INFO: `giap::trace` isn't a verbose target, so the production filter drops DEBUG here.
        tracing::info!(
            target: "giap::trace",
            kind = "prefix_cache_invalidated",
            reason = reason.as_str(),
            turns_served = served,
            "KV prefix invalidated"
        );
    }

    /// Swap reason from the pre-swap `"provider:model"` key; split at the first ':' (`gemma4:e2b`).
    fn provider_change_reason(previous_key: &str, new_model: &str) -> InvalidationReason {
        match previous_key.split_once(':') {
            Some((_, previous_model)) if previous_model == new_model => {
                InvalidationReason::ProviderRebuilt
            }
            // Also no previous key (first use), which honestly reads as a model swap.
            _ => InvalidationReason::ModelSwapped,
        }
    }

    /// Record a new prefix; it isn't warm until a turn is served on it.
    fn note_prefix_rebuilt(&self, hash: u64) {
        self.prefix_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .rebuilt(hash, std::time::Instant::now());
    }

    /// Record that this turn is being served off the existing prefix.
    fn note_prefix_served(&self) {
        let (hash, turns_served) = {
            let mut state = self.prefix_cache.lock().unwrap_or_else(|e| e.into_inner());
            state.serve_turn();
            (state.hash, state.turns_served)
        };
        // `turns_served` rising means the prefix is actually being reused.
        tracing::info!(
            target: "giap::trace",
            kind = "prefix_cache_served",
            hash = %hash,
            turns_served,
            "KV prefix reused"
        );
    }

    pub fn extension_manager(&self) -> Arc<GiapGooseExtensionManager> {
        self.extension_manager.clone()
    }

    pub fn tool_registry(&self) -> Option<Arc<dyn ToolRegistryPort>> {
        self.tool_registry.clone()
    }

    /// Mark an extension name as user-added so it survives the per-turn extension strip.
    pub async fn track_user_extension(&self, name: &str) {
        self.user_extensions.write().await.insert(name.to_string());
        *self.cached_tools.write().await = None;
        self.note_prefix_invalidated(InvalidationReason::ToolSetChanged);
    }

    pub async fn untrack_user_extension(&self, name: &str) {
        self.user_extensions.write().await.remove(name);
        *self.cached_tools.write().await = None;
        self.note_prefix_invalidated(InvalidationReason::ToolSetChanged);
    }

    fn builtin_extension_config(name: &str) -> ExtensionConfig {
        ExtensionConfig::Builtin {
            name: name.to_string(),
            description: String::new(),
            display_name: None,
            timeout: Some(600),
            bundled: Some(false),
            available_tools: vec![],
        }
    }

    /// Add a named builtin extension to a Goose session (idempotent).
    pub async fn add_builtin_extension(&self, name: &str, session_id: &str) -> Result<()> {
        let config = Self::builtin_extension_config(name);

        // Also register it with the extension manager so it can be re-enabled if disabled
        self.extension_manager
            .register_config(name.to_string(), config.clone())
            .await;

        self.agent
            .add_extension(config, session_id)
            .await
            .map_err(|e| anyhow!("Failed to add builtin extension '{}': {}", name, e))
    }

    /// Bulk-add builtins (one persist vs ~3 SQLite trips each); returns how many loaded.
    async fn add_builtin_extensions(&self, names: &[String], session_id: &str) -> usize {
        for name in names {
            self.extension_manager
                .register_config(name.clone(), Self::builtin_extension_config(name))
                .await;
        }

        let configs: Vec<ExtensionConfig> = names
            .iter()
            .map(|n| Self::builtin_extension_config(n))
            .collect();

        match self.agent.add_extensions_bulk(configs, session_id).await {
            Ok(results) => {
                // Use each result's own `name`: bulk loading isn't documented to preserve order.
                let mut loaded = 0usize;
                for result in &results {
                    if result.success {
                        loaded += 1;
                        tracing::debug!("giap extension loaded: {}", result.name);
                    } else {
                        tracing::warn!(
                            "giap extension failed to load: {}: {}",
                            result.name,
                            result.error.as_deref().unwrap_or("unknown error")
                        );
                    }
                }
                loaded
            }
            Err(e) => {
                tracing::warn!("giap extensions failed to load in bulk: {e}");
                0
            }
        }
    }

    /// Live turns' delegation authorities; `GooseOrchestrator` must share this exact registry.
    pub fn turn_authorities(
        &self,
    ) -> Arc<pond_core::shared::services::turn_authority::TurnAuthorityRegistry> {
        self.turn_authorities.clone()
    }

    /// GIAP -> Goose session id (Goose mints its own `YYYYMMDD_N`), creating + hydrating if new.
    async fn resolve_goose_session(&self, giap_sid: &str) -> String {
        if let Some(gid) = self
            .goose_session_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(giap_sid)
            .cloned()
        {
            tracing::info!(
                giap_sid = %giap_sid,
                goose_sid = %gid,
                branch = "map",
                "resolved the engine session"
            );
            return gid;
        }
        // Persisted pairing, re-validated: goose's store can be wiped separately, and a session not
        // named after this GIAP id (e.g. a recycled id) is someone else's conversation.
        if let Some(storage) = &self.giap_session_storage {
            if let Ok(Some(gid)) = storage.get_engine_session_id(giap_sid).await {
                match self.session_manager.get_session(&gid, false).await {
                    Ok(session) if session.name == giap_sid => {
                        self.goose_session_map
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(giap_sid.to_string(), gid.clone());
                        tracing::debug!(
                            "Restored persisted goose session pairing {giap_sid} -> {gid}"
                        );
                        tracing::info!(
                            giap_sid = %giap_sid,
                            goose_sid = %gid,
                            branch = "persisted",
                            "resolved the engine session"
                        );
                        return gid;
                    }
                    Ok(_) => {
                        tracing::warn!(
                            "Persisted goose session '{gid}' for '{giap_sid}' is named for a \
                             different session — treating the pairing as stale and re-resolving"
                        );
                    }
                    Err(_) => {
                        tracing::info!(
                            "Persisted goose session '{gid}' for '{giap_sid}' is gone — re-creating"
                        );
                    }
                }
            }
        }
        // Try using the GIAP session_id as-is (e.g. if Goose already stored it).
        if self
            .session_manager
            .get_session(giap_sid, false)
            .await
            .is_ok()
        {
            self.remember_goose_session(giap_sid, giap_sid).await;
            tracing::info!(
                giap_sid = %giap_sid,
                goose_sid = %giap_sid,
                branch = "id-as-is",
                "resolved the engine session"
            );
            return giap_sid.to_string();
        }
        // Create a brand-new Goose session; use the GIAP id as the human name.
        match self
            .session_manager
            .create_session(
                std::env::current_dir().unwrap_or_default(),
                giap_sid.to_string(),
                goose::session::session_manager::SessionType::User,
                GooseMode::Auto,
            )
            .await
        {
            Ok(session) => {
                let gid = session.id.clone();
                // Goose ids are `MAX(today's ids) + 1`, so deleted ids get reused; worth logging.
                tracing::info!(
                    giap_sid = %giap_sid,
                    goose_sid = %gid,
                    branch = "created",
                    "resolved the engine session"
                );
                self.remember_goose_session(giap_sid, &gid).await;
                // Replay existing history: the trimmer returns early on an empty conversation.
                self.hydrate_goose_session(&gid, giap_sid).await;
                gid
            }
            Err(e) => {
                tracing::warn!("Failed to create Goose session for '{}': {e}", giap_sid);
                giap_sid.to_string()
            }
        }
    }

    /// Record a GIAP -> Goose pairing in the process cache and, if wired, durably.
    async fn remember_goose_session(&self, giap_sid: &str, goose_sid: &str) {
        self.goose_session_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(giap_sid.to_string(), goose_sid.to_string());
        if let Some(storage) = &self.giap_session_storage {
            if let Err(e) = storage.set_engine_session_id(giap_sid, goose_sid).await {
                // Non-fatal: the cache covers this run; a restart just re-hydrates.
                tracing::warn!("Failed to persist goose session pairing: {e}");
            }
        }
    }

    /// Replay a GIAP conversation into a fresh Goose session; failures are logged, never fatal.
    /// Drops tool rows: stored rows can't rebuild provider-valid call/result pairs.
    async fn hydrate_goose_session(&self, goose_sid: &str, giap_session_id: &str) {
        use pond_core::models::domain::message::Role as GiapRole;
        use pond_core::models::services::context::turn_trimmer::{plan_replay, TrimRole};

        // Before the storage read: the engine session is new even if no history is found.
        self.note_prefix_invalidated(InvalidationReason::SessionResumed);

        let Some(storage) = &self.giap_session_storage else {
            return;
        };
        // Cap the read: budgeting would drop most of a long session anyway.
        let history = match storage.get_recent_messages(giap_session_id, 200).await {
            Ok(rows) => rows,
            // A brand-new conversation has no row yet — the common case.
            Err(e) => {
                tracing::debug!("hydrate: no pond history for {giap_session_id}: {e}");
                return;
            }
        };

        // `ids` stays in lockstep with `rows`. These filters repeat `plan_replay`'s on purpose: its
        // `index` counts post-filter rows, so filtering first makes it a valid index into `ids`.
        let mut rows: Vec<(TrimRole, String)> = Vec::with_capacity(history.len());
        let mut ids: Vec<String> = Vec::with_capacity(history.len());
        for m in history {
            let role = match m.message.role {
                GiapRole::User => TrimRole::User,
                GiapRole::Assistant => TrimRole::Assistant,
                // Tool rows are dropped (see above); System never reaches history.
                GiapRole::Tool | GiapRole::System => continue,
            };
            if m.message.content.trim().is_empty() {
                continue;
            }
            rows.push((role, m.message.content));
            ids.push(m.id);
        }
        while matches!(rows.last(), Some((TrimRole::User, _))) {
            rows.pop();
            ids.pop();
        }
        if rows.is_empty() {
            return;
        }

        let rolling_summary = storage
            .get_rolling_summary(giap_session_id)
            .await
            .ok()
            .and_then(|(s, _)| s);

        let profile = self.turn_profile(giap_session_id).await;

        let planned = plan_replay(
            rows,
            &profile,
            rolling_summary.as_deref(),
            self.token_counter().await,
        );
        if planned.is_empty() {
            return;
        }

        // ── Which historical images get real pixels ───────────────────────────
        // Counted only over messages that survived the budget cut.
        let attachment_counts: std::collections::HashMap<String, usize> = storage
            .list_session_attachments(giap_session_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .fold(std::collections::HashMap::new(), |mut acc, a| {
                *acc.entry(a.message_id).or_insert(0) += 1;
                acc
            });

        let mut replay_images: Vec<usize> = vec![0; planned.len()];
        let mut had_images: Vec<usize> = vec![0; planned.len()];
        if !attachment_counts.is_empty() {
            for (slot, tm) in had_images.iter_mut().zip(planned.iter()) {
                // A spliced summary has no source row (`index == usize::MAX`).
                if let Some(id) = ids.get(tm.index) {
                    *slot = attachment_counts.get(id).copied().unwrap_or(0);
                }
            }
            // Only user rows carry pixels: assistants get no budget but keep their placeholder.
            let plannable: Vec<usize> = had_images
                .iter()
                .zip(planned.iter())
                .map(|(n, tm)| match tm.role {
                    TrimRole::Assistant => 0,
                    _ => *n,
                })
                .collect();
            // Same cap as the live trimmer, so a restart neither gains nor loses images.
            replay_images =
                pond_core::models::services::context::image_history::plan_history_images(
                    &plannable,
                );
        }

        let wanted_ids: Vec<String> = planned
            .iter()
            .zip(replay_images.iter())
            .filter(|(_, n)| **n > 0)
            .filter_map(|(tm, _)| ids.get(tm.index).cloned())
            .collect();
        let loaded_images = if wanted_ids.is_empty() {
            std::collections::HashMap::new()
        } else {
            storage
                .load_message_images(&wanted_ids)
                .await
                .unwrap_or_default()
        };

        let mut replayed_images_total = 0usize;
        let mut placeholders_total = 0usize;
        let mut replayed: Vec<Message> = Vec::with_capacity(planned.len());
        for (i, tm) in planned.iter().enumerate() {
            // Wording and counters follow what actually attached, not the plan: files go missing,
            // a storage error empties the load, and assistant rows attach nothing.
            let images = match tm.role {
                TrimRole::Assistant => None,
                _ => ids.get(tm.index).and_then(|id| loaded_images.get(id)),
            };
            let attached = replay_images[i].min(images.map_or(0, Vec::len));
            let dropped = had_images[i].saturating_sub(attached);

            // Tell the model an image was there; wording depends on whether any survived.
            let text = if dropped > 0 {
                placeholders_total += dropped;
                format!(
                    "{}\n{}",
                    tm.text,
                    pond_core::models::services::context::image_history::history_image_placeholder(
                        attached
                    )
                )
            } else {
                tm.text.clone()
            };

            replayed.push(match tm.role {
                TrimRole::Assistant => Message::assistant().with_text(&text),
                // The spliced summary rides a user message, like the trimmer's.
                _ => {
                    let mut msg = Message::user().with_text(&text);
                    for img in images.into_iter().flatten().take(attached) {
                        msg = msg.with_image(&img.data, &img.mime_type);
                        replayed_images_total += 1;
                    }
                    msg
                }
            });
        }
        let replayed_len = replayed.len();

        let conversation = goose::conversation::Conversation::new_unvalidated(replayed);
        match self
            .session_manager
            .replace_conversation(goose_sid, &conversation)
            .await
        {
            Ok(()) => tracing::info!(
                target: "giap::trace",
                kind = "history_hydrate",
                session_id = %giap_session_id,
                goose_session_id = %goose_sid,
                messages = replayed_len,
                summary_spliced = rolling_summary.is_some(),
                images_replayed = replayed_images_total,
                images_placeheld = placeholders_total,
            ),
            Err(e) => tracing::warn!("hydrate: replace_conversation failed: {e}"),
        }
    }

    /// Export the Goose env knobs GIAP owns, but only when they changed.
    async fn apply_goose_env_knobs(
        &self,
        settings: &pond_core::user_data::domain::settings::Settings,
    ) {
        let resolution = self
            .resolve_window(
                &settings.chat_provider,
                &settings.chat_model,
                settings.context_window_override,
            )
            .await;
        let effective_ctx = resolution.tokens;
        // Store before the signature guard's early return: budget paths read this every turn.
        *self.last_window.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((settings.chat_provider.clone(), resolution));
        // Likewise before the early return: the trimmer reads it every turn.
        *self
            .last_verbatim_days
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(settings.compaction_verbatim_days);
        let knobs = goose_env_knobs(&settings.chat_provider, effective_ctx);
        let signature = knobs
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_deref().unwrap_or("")))
            .collect::<Vec<_>>()
            .join(";");
        {
            let mut last = self
                .last_env_signature
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if *last == signature {
                return;
            }
            *last = signature.clone();
        }

        // SAFETY: set_var is unsound with threads; Goose already sets OLLAMA_HOST/OLLAMA_TIMEOUT
        // on this path, and the signature guard limits it to actual changes.
        #[allow(unused_unsafe)]
        unsafe {
            for (key, value) in &knobs {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
        tracing::info!(
            provider = %settings.chat_provider,
            model = %settings.chat_model,
            effective_ctx,
            knobs = %signature,
            "Applied Goose context/compaction env knobs"
        );
    }

    /// The registry-pinned local context size, which the engine uses as its real `n_ctx`.
    /// It outranks GOOSE_CONTEXT_LIMIT, so GIAP must never budget past it (Jetson pins 4096).
    fn registry_context_size(model: &str) -> Option<usize> {
        use goose::providers::local_inference::local_model_registry::get_registry;

        let registry = get_registry().lock().ok()?;
        let entry = registry.get_model(model)?;
        entry.settings.context_size.map(|c| c as usize)
    }

    /// Governor rung 3: the catalog's `context_length`, e.g. Ollama's real window from `/api/show`.
    async fn catalog_context_length(
        repo: Option<&Arc<dyn ModelRepository>>,
        provider: &str,
        model: &str,
    ) -> Option<u32> {
        let repo = repo?;
        let id = ModelRecord::id_for(&ModelCategory::for_chat_provider(provider), model);
        match repo.get_by_id(&id).await {
            Ok(Some(record)) => record.context_length,
            Ok(None) => None,
            Err(e) => {
                // Must not fail the turn: the governor has three lower rungs.
                tracing::warn!(model_id = %id, error = %e, "catalog context_length lookup failed");
                None
            }
        }
    }

    /// Resolve a provider/model's window; `override_tokens` (0 = unset) never beats a registry pin.
    async fn resolve_window(
        &self,
        provider: &str,
        model: &str,
        override_tokens: u32,
    ) -> WindowResolution {
        let pinned = match provider {
            "local" | "gguf" => Self::registry_context_size(model),
            _ => None,
        };
        Self::resolve_window_from(
            self.model_repo.as_ref(),
            provider,
            model,
            override_tokens,
            pinned,
        )
        .await
    }

    /// `resolve_window` minus the untestable registry read. A pin wins outright: no catalog read.
    async fn resolve_window_from(
        repo: Option<&Arc<dyn ModelRepository>>,
        provider: &str,
        model: &str,
        override_tokens: u32,
        pinned: Option<usize>,
    ) -> WindowResolution {
        let catalog = match pinned {
            Some(_) => None,
            None => Self::catalog_context_length(repo, provider, model).await,
        };
        Self::resolve_window_with(provider, model, override_tokens, pinned, catalog)
    }

    /// Feeds [`ContextGovernor`], which owns the precedence so trimmer and telemetry can't drift.
    fn resolve_window_with(
        provider: &str,
        model: &str,
        override_tokens: u32,
        pinned: Option<usize>,
        catalog: Option<u32>,
    ) -> WindowResolution {
        ContextGovernor::resolve(&ContextInputs {
            provider,
            model,
            override_tokens,
            registry_pinned: pinned,
            catalog_context_length: catalog,
            // Settings-scoped path: one session's last turn says nothing about it.
            engine_reported: None,
            // The capability cache is stale on turn one; the name heuristic is more reliable.
            capability_window: None,
        })
    }

    /// The tiktoken counter, else chars/4. Neither is exact for GGUF (hence overshoot feedback).
    async fn token_counter(&self) -> &dyn PondTokenCounter {
        static HEURISTIC: HeuristicTokenCounter = HeuristicTokenCounter;
        match self.token_counter_cell().await {
            Some(c) => c.as_ref(),
            None => &HEURISTIC,
        }
    }

    /// The same counter, owned for the `'static` stream; shared so its LRU cache isn't rebuilt.
    async fn token_counter_handle(&self) -> Arc<dyn PondTokenCounter> {
        match self.token_counter_cell().await {
            Some(c) => c.clone(),
            None => Arc::new(HeuristicTokenCounter),
        }
    }

    /// Builds the tiktoken counter once for both accessors; `None` if it failed.
    async fn token_counter_cell(&self) -> &Option<Arc<crate::token_counter::TiktokenCounter>> {
        self.token_counter
            .get_or_init(|| async {
                match crate::token_counter::TiktokenCounter::new().await {
                    Ok(c) => Some(Arc::new(c)),
                    Err(e) => {
                        tracing::warn!("token counter unavailable, falling back to chars/4: {e}");
                        None
                    }
                }
            })
            .await
    }

    /// The RAW resolved window and its provider. For budgets use [`GooseAdapter::turn_profile`].
    async fn window_and_provider(&self) -> (String, WindowResolution) {
        if let Some(cached) = self
            .last_window
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return cached;
        }
        let settings = self.settings_repo.get().await.unwrap_or_default();
        let resolution = self
            .resolve_window(
                &settings.chat_provider,
                &settings.chat_model,
                settings.context_window_override,
            )
            .await;
        (settings.chat_provider, resolution)
    }

    /// This turn's budget profile (history: full window; preamble: prompt clamp). The only budget
    /// source, keyed by session because a live child's reservation is per conversation.
    async fn turn_profile(&self, giap_session_id: &str) -> CompactionProfile {
        let (provider, window) = self.window_and_provider().await;
        let observed = self.observed_reasoning_samples().await;
        Self::profile_for_session(
            &crate::orchestrator::process_device_ledger(),
            &provider,
            window.tokens,
            giap_session_id,
            &observed,
        )
    }

    /// Recent `reasoning_tokens`, capped by rows scanned (400 ≈ a fortnight); errors yield none.
    async fn observed_reasoning_samples(&self) -> Vec<u32> {
        const SCAN_ROWS: usize = 400;
        let Some(storage) = &self.giap_session_storage else {
            return Vec::new();
        };
        match storage.recent_reasoning_samples(SCAN_ROWS).await {
            Ok(samples) => samples,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    "could not read reasoning history; keeping the anchor output reserve"
                );
                Vec::new()
            }
        }
    }

    /// Ledger read + profile build; the ledger is keyed by the GIAP session id, never Goose's.
    fn profile_for_session(
        ledger: &crate::orchestrator::DeviceLedger,
        provider: &str,
        resolved_window: usize,
        giap_session_id: &str,
        observed_reasoning: &[u32],
    ) -> CompactionProfile {
        Self::profile_for(
            provider,
            resolved_window,
            ledger.reserved_fraction(giap_session_id),
            observed_reasoning,
        )
    }

    /// Pure half of [`GooseAdapter::turn_profile`]. Swapping the two windows compiles but is wrong;
    /// `reserved_fraction` applies after `for_windows` so the preamble (and KV prefix) stay put.
    fn profile_for(
        provider: &str,
        resolved_window: usize,
        reserved_fraction: f32,
        observed_reasoning: &[u32],
    ) -> CompactionProfile {
        let profile = CompactionProfile::for_windows(
            resolved_window,
            ContextGovernor::prompt_window(provider, resolved_window),
        )
        .with_history_reserved(reserved_fraction);

        // After `for_windows`: the curve is the input, and the reserve floors at what it chose.
        let reserve = pond_core::models::services::context::context_budget::observed_output_reserve(
            observed_reasoning,
            profile.output_reserve_tokens,
            profile.context_window_tokens,
        );
        CompactionProfile {
            output_reserve_tokens: reserve,
            ..profile
        }
    }

    /// Whether the active model DECLARES vision, regardless of whether its encoder has downloaded.
    /// Must not flip mid-session: the `<vision>` prompt section sits in the KV-cached prefix.
    fn model_supports_vision(provider: &str, model: &str) -> bool {
        match provider {
            "local" | "gguf" => crate::vision_encoder::declares_vision(model),
            _ => pond_core::models::domain::model_capabilities::ModelCapabilities::name_implies_vision(
                model,
            ),
        }
    }

    /// Whether this turn's prompt carries `<vision>`. `voice` is the INSTANCE flag: it matches
    /// `capabilities()` and, fixed per process, can't flip the KV-cached prefix between turns.
    fn vision_section_applies(provider: &str, model: &str, voice: bool) -> bool {
        !voice && Self::model_supports_vision(provider, model)
    }

    /// Whether this turn's prompt carries `<thinking>` (and `enable_thinking`). "auto" reads the
    /// model file, not the capability cache, which is stale on turn one and would move the prefix.
    fn thinking_section_applies(
        mode: &str,
        provider: &str,
        model: &str,
        data_dir: Option<&std::path::Path>,
        voice: bool,
    ) -> bool {
        if voice {
            return false;
        }
        match mode {
            "on" => true,
            "off" => false,
            _ => crate::model_traits::model_reasons(provider, model, data_dir),
        }
    }

    /// Whether this turn may emit `AgentStreamEvent::Thinking`, gated here for all consumers.
    /// `voice` includes the per-request flag, which is safe: this never touches the prefix.
    fn reasoning_frames_enabled(show_thinking: bool, voice: bool) -> bool {
        show_thinking && !voice
    }

    /// Instance OR request flag; in serve mode `instance` is always false, so `request` decides.
    fn voice_turn(instance: bool, request: bool) -> bool {
        instance || request
    }

    /// Display-gated reasoning fragments (raw, maybe a lone space) for [`ReasoningCoalescer`].
    /// The gate must stay in here. `RedactedThinking` is provider ciphertext and is never shown.
    fn reasoning_frames(msg: &Message, emit: bool) -> Vec<String> {
        if !emit {
            return Vec::new();
        }
        Self::reasoning_blocks(msg)
    }

    /// Ungated reasoning fragments, shared by count and display; untrimmed (spaces are content).
    fn reasoning_blocks(msg: &Message) -> Vec<String> {
        msg.content
            .iter()
            .filter_map(|c| c.as_thinking())
            .map(|t| t.thinking.clone())
            .collect()
    }

    /// Whether this message ends the passage in flight: only content GIAP would yield does.
    fn message_ends_reasoning(msg: &Message) -> bool {
        use goose::conversation::message::MessageContent;
        msg.content.iter().any(|c| {
            matches!(
                c,
                MessageContent::ToolRequest(_) | MessageContent::ToolResponse(_)
            )
        }) || !msg.as_concat_text().is_empty()
    }

    /// Reasoning tokens in this message, ungated: the cost must not depend on `show_thinking`.
    /// Counted from the text (inexact); no shipped provider reports it.
    fn count_reasoning_tokens(msg: &Message, counter: &dyn PondTokenCounter) -> u32 {
        Self::reasoning_blocks(msg)
            .iter()
            .map(|t| counter.count(t) as u32)
            .sum()
    }

    /// `HH:MM`, or spoken words for voice: small models botch reading digits aloud.
    fn format_current_time(now: chrono::DateTime<chrono::Local>, voice: bool) -> String {
        use chrono::Timelike;
        if voice {
            pond_core::models::services::voice::spoken_time::spoken_time(now.hour(), now.minute())
        } else {
            now.format("%H:%M").to_string()
        }
    }

    /// Append `<vision>` to the template pre-Tera, so it lands inside the hashed static prefix.
    fn apply_vision_section(template: String, vision: bool, compact: bool) -> String {
        if !vision {
            return template;
        }
        format!(
            "{template}\n{}",
            pond_core::prompts::vision_capability_section(compact)
        )
    }

    /// Stamp `enable_thinking` onto a local/GGUF ModelConfig (only `false` acts there). Skipped
    /// for HTTP providers, which may serialize `request_params` straight into request bodies.
    fn with_thinking_param(
        provider: &str,
        cfg: goose_providers::model::ModelConfig,
        enable_thinking: bool,
    ) -> goose_providers::model::ModelConfig {
        if !matches!(provider, "local" | "gguf") {
            return cfg;
        }
        cfg.with_merged_request_params(HashMap::from([(
            "enable_thinking".to_string(),
            serde_json::Value::Bool(enable_thinking),
        )]))
    }

    /// Hot-swap the provider on a provider/model change, or re-stamp it if only thinking changed.
    async fn ensure_provider_current(
        &self,
        settings: &pond_core::user_data::domain::settings::Settings,
        session_id: &str,
        enable_thinking: bool,
    ) -> Result<()> {
        let key = format!("{}:{}", settings.chat_provider, settings.chat_model);
        // Snapshot: `last_provider_key` is overwritten before the swap block classifies the change.
        let previous_key = {
            let last = self
                .last_provider_key
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            last.clone()
        };
        let key_unchanged = previous_key == key;
        let thinking_unchanged = {
            let last = self
                .last_thinking_param
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *last == Some(enable_thinking)
        };
        // HTTP providers never carry the param: just record the change.
        if key_unchanged
            && !thinking_unchanged
            && !matches!(settings.chat_provider.as_str(), "local" | "gguf")
        {
            *self
                .last_thinking_param
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(enable_thinking);
        } else if key_unchanged && !thinking_unchanged {
            // Re-stamp, don't rebuild. The prefix moves anyway: thinking feeds PromptState.
            let cached = self
                .current_provider
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some((p, cfg)) = cached {
                let cfg = Self::with_thinking_param(&settings.chat_provider, cfg, enable_thinking);
                self.agent
                    .update_provider(p.clone(), cfg.clone(), session_id)
                    .await?;
                *self
                    .current_provider
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some((p, cfg));
                *self
                    .last_thinking_param
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(enable_thinking);
                {
                    let mut configured = self
                        .provider_configured_sessions
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    configured.clear();
                    configured.insert(session_id.to_string());
                }
                self.note_prefix_invalidated(InvalidationReason::ProviderRebuilt);
                tracing::info!(
                    enable_thinking,
                    "Re-stamped engine thinking flag on the current provider"
                );
                return Ok(());
            }
        }
        if key_unchanged {
            // Goose resolves the model PER SESSION; an unconfigured session falls back to the
            // global goose config (maybe a long-gone model). Configure each one once.
            let session_configured = self
                .provider_configured_sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(session_id);
            if session_configured {
                // INFO so it's visible: this in-memory claim can outlive a deleted, reissued row.
                tracing::info!(
                    session_id = %session_id,
                    provider_key = %key,
                    "skipping provider setup: this session is believed already configured"
                );
                return Ok(());
            }
            let cached = self
                .current_provider
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let Some((p, cfg)) = cached else {
                // Bail, don't return Ok: Goose would fail later with an opaque model-config error.
                anyhow::bail!(
                    "no chat provider has been built yet, so Goose session \
                     '{session_id}' cannot be configured for {key}"
                );
            };
            self.agent.update_provider(p, cfg, session_id).await?;
            self.provider_configured_sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(session_id.to_string());
            // INFO, not DEBUG: once per session, and the deployed filter drops DEBUG here.
            tracing::info!(
                session_id = %session_id,
                provider_key = %key,
                "configured a new Goose session with the current provider"
            );
            return Ok(());
        }
        tracing::debug!("[model-switch] provider change detected -> {}", key);

        // Env knobs come from `apply_goose_env_knobs`, run first: ModelConfig reads the limit.

        // Third element: the URL the provider sends to (None in-process); the shim gates on it.
        let provider: Option<(
            Arc<dyn Provider>,
            goose_providers::model::ModelConfig,
            Option<String>,
        )> = match settings.chat_provider.as_str() {
            // Lock shared with AppState.mesh_provider, read per turn; no MCP tools over mesh yet.
            "mesh" => match self.mesh_provider.read().await.clone() {
                Some(provider) => {
                    let cfg = goose_providers::model::ModelConfig::new("mesh");
                    Some((
                        Arc::new(crate::mesh_provider::MeshProvider::new(provider))
                            as Arc<dyn Provider>,
                        cfg,
                        // No endpoint: mesh egress is libp2p, not HTTP.
                        None,
                    ))
                }
                None => {
                    tracing::warn!(
                        "[model-switch] chat_provider=mesh but no mesh_provider wired into \
                         GooseAdapter — keeping current provider"
                    );
                    None
                }
            },
            // In-process llama.cpp; LocalInferenceProvider finds the .gguf via Goose's registry.
            "local" | "gguf" => {
                let model_name = if settings.chat_model.is_empty() {
                    "llamafile".to_string()
                } else {
                    settings.chat_model.clone()
                };
                // Canonical key, so aliases of one GGUF can't load it twice under two ids.
                let registry_key = match self.data_dir {
                    Some(ref dd) => Self::register_gguf_model(&model_name, dd),
                    None => model_name.trim_end_matches(".gguf").to_string(),
                };
                // Registration leaves `mmproj_path` (the engine's vision gate) None. Non-blocking
                // (~1 GB fetch); the path is resolved per generation, so no restart is needed.
                if let Some(ref dd) = self.data_dir {
                    crate::vision_encoder::ensure_mmproj_available(dd, &registry_key);
                    // The engine finds the speculative-decoding drafter only via a registry row.
                    crate::mtp_drafter::ensure_drafter_registered(dd, &registry_key);
                }
                let cfg = goose_providers::model::ModelConfig::new(&registry_key);
                tracing::debug!(
                    "[model-switch] building LocalInferenceProvider for '{}'...",
                    model_name
                );
                // Direct construction skips the HF-token/config resolver wiring ProviderDef does.
                goose::providers::local_inference::configure_local_inference();
                match goose::providers::local_inference::LocalInferenceProvider::from_env().await {
                    Ok(p) => {
                        tracing::debug!(
                            "[model-switch] LocalInferenceProvider ready for '{}'",
                            model_name
                        );
                        tracing::info!("Built LocalInferenceProvider for model '{}'", model_name);
                        Some((Arc::new(p), cfg, None))
                    }
                    Err(e) => {
                        tracing::debug!(
                            "[model-switch] FAILED to build LocalInferenceProvider for '{}': {e}",
                            model_name
                        );
                        tracing::warn!(
                            "Failed to build local inference provider for '{}': {e}",
                            model_name
                        );
                        None
                    }
                }
            }
            // llamafile uses the Ollama wire protocol over HTTP.
            "llamafile" => {
                std::env::set_var("OLLAMA_HOST", &self.llamafile_url);
                std::env::set_var("OLLAMA_TIMEOUT", "600");
                let model_name = if settings.chat_model.is_empty() {
                    "llamafile".to_string()
                } else {
                    settings.chat_model.clone()
                };
                let cfg = goose_providers::model::ModelConfig::new(&model_name);
                tracing::debug!(
                    "[model-switch] building llamafile OllamaProvider for '{}'...",
                    model_name
                );
                match goose::providers::ollama_def::from_env(None).await {
                    Ok(p) => {
                        tracing::debug!(
                            "[model-switch] llamafile provider ready for '{}'",
                            model_name
                        );
                        Some((Arc::new(p), cfg, Some(self.llamafile_url.clone())))
                    }
                    Err(e) => {
                        tracing::debug!(
                            "[model-switch] FAILED to build llamafile provider for '{}': {e}",
                            model_name
                        );
                        tracing::warn!("Failed to build llamafile provider: {e}");
                        None
                    }
                }
            }
            // mistral.rs speaks OpenAI. Mac-only; docs/developer/mistralrs-provider.md says why.
            "mistralrs" => {
                let host = std::env::var("GIAP_MISTRALRS_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:9002".to_string());
                std::env::set_var("OPENAI_HOST", &host);
                std::env::set_var("OPENAI_BASE_PATH", "v1/chat/completions");
                // mistral.rs needs no key, but goose's OpenAI provider won't build without one.
                if std::env::var("OPENAI_API_KEY").is_err() {
                    std::env::set_var("OPENAI_API_KEY", "not-required-by-mistralrs");
                }
                std::env::set_var("OPENAI_TIMEOUT", "600");
                let model_name = if settings.chat_model.is_empty() {
                    "default".to_string()
                } else {
                    settings.chat_model.clone()
                };
                let cfg = goose_providers::model::ModelConfig::new(&model_name);
                match goose::providers::openai_def::from_env(None).await {
                    Ok(p) => Some((Arc::new(p), cfg, Some(host))),
                    Err(e) => {
                        tracing::warn!("Failed to build mistral.rs provider: {e}");
                        None
                    }
                }
            }
            "ollama" => {
                let ollama_host = std::env::var("GIAP_OLLAMA_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
                std::env::set_var("OLLAMA_HOST", &ollama_host);
                std::env::set_var("OLLAMA_TIMEOUT", "600");
                let model_name = if settings.chat_model.is_empty() {
                    "llama3.2".to_string()
                } else {
                    settings.chat_model.clone()
                };
                tracing::debug!(
                    "[model-switch] building Ollama provider for '{}'...",
                    model_name
                );
                let cfg = goose_providers::model::ModelConfig::new(&model_name);
                match goose::providers::ollama_def::from_env(None).await {
                    Ok(p) => {
                        tracing::debug!(
                            "[model-switch] Ollama provider ready for '{}'",
                            model_name
                        );
                        Some((Arc::new(p), cfg, Some(ollama_host.clone())))
                    }
                    Err(e) => {
                        tracing::debug!(
                            "[model-switch] FAILED to build Ollama provider for '{}': {e}",
                            model_name
                        );
                        tracing::warn!("Failed to build ollama provider: {e}");
                        None
                    }
                }
            }
            _ => {
                tracing::debug!(
                    "[model-switch] unknown provider '{}', keeping current",
                    settings.chat_provider
                );
                None
            }
        };

        if let Some((p, model_cfg, endpoint)) = provider {
            let model_cfg =
                Self::with_thinking_param(&settings.chat_provider, model_cfg, enable_thinking);
            // Every provider Goose sees goes through the shim: last-mile veto and egress gate.
            let p: Arc<dyn Provider> = Arc::new(crate::provider_shim::GiapProviderShim::new(
                p,
                self.shim_controls.clone(),
                endpoint,
            ));
            tracing::debug!(
                "[model-switch] swapping Goose provider to {}:{} for session {}",
                settings.chat_provider,
                settings.chat_model,
                session_id
            );
            tracing::info!(
                target: "giap::trace",
                kind = "provider_swap",
                session_id = %session_id,
                provider = %settings.chat_provider,
                model = %settings.chat_model,
                "Switching Goose provider"
            );
            self.agent
                .update_provider(p.clone(), model_cfg.clone(), session_id)
                .await?;
            *self
                .last_provider_key
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = key.clone();
            *self
                .last_thinking_param
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(enable_thinking);
            {
                let mut configured = self
                    .provider_configured_sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                configured.clear();
                configured.insert(session_id.to_string());
            }
            *self
                .current_provider
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some((p, model_cfg));
            // Goose's fallback for sessions with no model_config reads env before config.yaml.
            std::env::set_var("GOOSE_MODEL", &settings.chat_model);
            // Goose resolves the fallback provider before the model, so both must be exported.
            std::env::set_var("GOOSE_PROVIDER", &settings.chat_provider);
            // Log the read-back, not the write: these have been seen unset after this ran.
            tracing::info!(
                goose_provider = ?std::env::var("GOOSE_PROVIDER").ok(),
                goose_model = ?std::env::var("GOOSE_MODEL").ok(),
                "exported the global goose provider fallback"
            );

            // Name heuristic first, then overridden from evidence (for a GGUF, the template and
            // metadata `thinking_section_applies` reads) so this cache can't contradict the prompt.
            let mut caps =
                pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
                    &settings.chat_model,
                );
            caps.vision =
                Self::model_supports_vision(&settings.chat_provider, &settings.chat_model);
            caps.thinking = crate::model_traits::model_reasons(
                &settings.chat_provider,
                &settings.chat_model,
                self.data_dir.as_deref(),
            );
            caps.tool_calling = crate::model_traits::model_uses_native_tools(
                &settings.chat_provider,
                &settings.chat_model,
                self.data_dir.as_deref(),
            );
            if matches!(settings.chat_provider.as_str(), "local" | "gguf") {
                // llama.cpp can GBNF-constrain any GGUF, quant tag in its name or not.
                caps.structured_output = true;
            }
            if let Some(trained) = crate::model_traits::trained_context_window(
                &settings.chat_provider,
                &settings.chat_model,
                self.data_dir.as_deref(),
            ) {
                // Upper bound only; `ContextGovernor` ranks a registry pin or engine cap above it.
                caps.context_window_tokens = trained;
            }
            tracing::debug!(
                "[model-switch] capabilities: thinking={}, vision={}, context={}k",
                caps.thinking,
                caps.vision,
                caps.context_window_tokens / 1000
            );
            *self
                .model_capabilities
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = caps;

            // Forces a prompt rebuild with the new caps; the swap already dropped the KV cache.
            *self
                .last_prefix_hash
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = 0;

            self.note_prefix_invalidated(Self::provider_change_reason(
                &previous_key,
                &settings.chat_model,
            ));

            tracing::debug!("[model-switch] swap complete, key={}", key);
        } else {
            // Unknown provider or failed build; the only caller is a turn that needs a provider.
            anyhow::bail!(
                "no chat provider could be built for '{}' with model '{}' — check the \
                 provider and model in Settings",
                settings.chat_provider,
                settings.chat_model
            );
        }
        Ok(())
    }

    /// Shared handle to the last-prompt-token map, for the 'static stream closure.
    fn last_prompt_tokens_handle(&self) -> Arc<Mutex<HashMap<String, u32>>> {
        self.last_prompt_tokens_arc
            .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
            .clone()
    }

    /// Memories relevant to `message`, with cosine similarity when known. Never fails the turn.
    async fn topical_memories(
        &self,
        message: &str,
        scope: &ProfileScope,
        limit: usize,
    ) -> Vec<(MemoryFragment, Option<f32>)> {
        // Before embedding: a Guest turn matches nothing and shouldn't reach the embedder.
        if scope.excludes_everything() {
            return Vec::new();
        }
        if let Some(provider) = &self.embedding_provider {
            // Query prefix, not document: asymmetric retrievers (nomic) embed them differently.
            match provider.embed_query(message).await {
                Ok(query_vector) => {
                    match self
                        .memory_repo
                        .search_similar(&query_vector, scope, limit)
                        .await
                    {
                        Ok(hits) => {
                            return hits
                                .into_iter()
                                .map(|fragment| {
                                    // No vector, or one from another model (other width): `None`,
                                    // not `Some(0.0)`, a real score that would sink the blend.
                                    let similarity = fragment
                                        .embedding
                                        .as_deref()
                                        .filter(|e| e.len() == query_vector.len())
                                        .map(|e| cosine_similarity(&query_vector, e));
                                    (fragment, similarity)
                                })
                                .collect();
                        }
                        Err(e) => tracing::warn!("memory: semantic search failed: {e}"),
                    }
                }
                Err(e) => tracing::warn!("memory: embedding the turn failed, using keywords: {e}"),
            }
        }

        let keywords = pond_core::user_data::services::memory_relevance::keyword_terms(message);
        if keywords.is_empty() {
            return vec![];
        }
        match self
            .memory_repo
            .search_by_content(&keywords, scope, limit)
            .await
        {
            Ok(hits) => hits.into_iter().map(|m| (m, None)).collect(),
            Err(e) => {
                tracing::warn!("memory: keyword search failed: {e}");
                vec![]
            }
        }
    }

    // ── Per-session tool selection ──────────────────────────────────────────

    /// Group-description vectors, cached on first success; `None` means "don't narrow".
    /// Failures aren't cached (the embedder may still be downloading), so a later session retries.
    async fn group_description_embeddings(&self) -> Option<&Vec<(String, Vec<f32>)>> {
        self.group_embeddings
            .get_or_try_init(|| async {
                let provider = self.embedding_provider.as_ref().ok_or(())?;
                let available: Vec<String> = registered_extensions().to_vec();
                let scorable =
                    pond_core::mcp::services::tool_selection::scorable_groups(&available);
                let mut out = Vec::with_capacity(scorable.len());
                for (extension, description) in scorable {
                    match provider.embed(description).await {
                        Ok(v) => out.push((extension.to_string(), v)),
                        // Unscored groups stay dormant until the escape hatch pulls them in.
                        Err(e) => {
                            tracing::warn!("tool selection: embedding '{extension}' failed: {e}")
                        }
                    }
                }
                // Empty is a failure: `Err` leaves the cell unset so a later session retries.
                if out.is_empty() {
                    return Err(());
                }
                Ok(out)
            })
            .await
            .ok()
    }

    /// This session's tool groups, sticky: per-turn re-scoring would break KV prefix reuse.
    async fn resolve_session_tool_groups(
        &self,
        giap_session_id: &str,
        first_message: &str,
        memories: &str,
        skills: &str,
        scope: &ProfileScope,
        minimal: bool,
    ) -> SessionGroups {
        use pond_core::mcp::services::tool_selection as sel;

        // Filtered up front, which sticks: `select_groups` filters core groups by `available`.
        let permitted = self.permitted_groups(scope);

        if let Some(cached) = self
            .session_tool_groups
            .read()
            .await
            .get(giap_session_id)
            .cloned()
        {
            // Clamp to what is permitted NOW: scope is re-derived per turn, and an Owner turn
            // may have filled this cache before a Guest (or failed-rung) turn reads it.
            let cached =
                pond_core::mcp::services::tool_selection::clamp_to_permitted(cached, &permitted);
            self.remember_permitted(giap_session_id, &permitted).await;
            return SessionGroups {
                loaded: cached,
                permitted,
            };
        }

        if let Some(storage) = &self.giap_session_storage {
            if let Ok(Some(groups)) = storage.get_session_tool_groups(giap_session_id).await {
                if !groups.is_empty() {
                    // Stored per session, scope is per turn: clamp to what is permitted now.
                    let groups = pond_core::mcp::services::tool_selection::clamp_to_permitted(
                        groups, &permitted,
                    );
                    self.session_tool_groups
                        .write()
                        .await
                        .insert(giap_session_id.to_string(), groups.clone());
                    self.remember_permitted(giap_session_id, &permitted).await;
                    tracing::debug!(
                        session_id = %giap_session_id,
                        groups = ?groups,
                        "tool selection: restored persisted groups"
                    );
                    return SessionGroups {
                        loaded: groups,
                        permitted,
                    };
                }
            }
        }

        let available: Vec<String> = permitted.clone();

        // "minimal": the hatch only, no scoring. After the cache and persisted row so groups
        // the model enabled earlier this session come back without another round trip.
        if minimal {
            let selection = sel::minimal_groups(&available);
            self.persist_session_tool_groups(giap_session_id, &selection.groups)
                .await;
            self.remember_permitted(giap_session_id, &permitted).await;
            tracing::info!(
                target: "giap::trace",
                kind = "tool_selection",
                session_id = %giap_session_id,
                mode = "minimal",
                basis = "mode_minimal",
                groups = selection.groups.len(),
                groups_total = permitted.len(),
                "tool selection: hatch only"
            );
            return SessionGroups {
                loaded: selection.groups,
                permitted,
            };
        }

        // Scored separately and max-merged, so a kilobyte of memories can't drown a short question.
        let signals = sel::selection_signals(first_message, memories, skills);

        // Failures widen to all groups: a missing tool is a wrong answer, a surplus one is tokens.
        let scores: Option<Vec<sel::GroupScore>> = match (
            self.group_description_embeddings().await,
            self.embedding_provider.as_ref(),
        ) {
            (Some(group_vectors), Some(provider)) => {
                let mut per_signal: Vec<Vec<sel::GroupScore>> = Vec::with_capacity(signals.len());
                let mut failed = None;
                for signal in &signals {
                    match provider.embed(signal).await {
                        Ok(query) => per_signal.push(
                            group_vectors
                                .iter()
                                .map(|(extension, v)| sel::GroupScore {
                                    extension: extension.clone(),
                                    score: cosine_similarity(&query, v),
                                })
                                .collect(),
                        ),
                        Err(e) => failed = Some(e),
                    }
                }
                match (per_signal.is_empty(), failed) {
                    // Every signal failed to embed: widen.
                    (true, Some(e)) => {
                        tracing::warn!("tool selection: embedding the opening message failed: {e}");
                        None
                    }
                    (true, None) => None,
                    // Score whatever embedded; don't drop a good signal because another failed.
                    _ => Some(sel::merge_scores(&per_signal)),
                }
            }
            _ => None,
        };

        let selection = sel::select_groups(
            &available,
            scores.as_deref(),
            sel::DEFAULT_RELEVANCE_THRESHOLD,
        );

        // Make a failed narrowing visible; otherwise it looks like mode = "relevant" worked.
        if matches!(selection.basis, sel::SelectionBasis::NoEmbedder) {
            tracing::warn!(
                target: "giap::trace",
                kind = "tool_selection_widened",
                session_id = %giap_session_id,
                reason = if self.embedding_provider.is_none() {
                    "no_embedder"
                } else {
                    "embed_failed"
                },
                groups = permitted.len(),
                "tool selection asked for narrowing and could not narrow"
            );
        }

        if tracing::enabled!(tracing::Level::DEBUG) {
            if let Some(scores) = scores.as_deref() {
                let mut ranked: Vec<&sel::GroupScore> = scores.iter().collect();
                ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
                let top: Vec<String> = ranked
                    .iter()
                    .take(5)
                    .map(|s| format!("{}={:.3}", s.extension, s.score))
                    .collect();
                tracing::debug!(
                    session_id = %giap_session_id,
                    threshold = sel::DEFAULT_RELEVANCE_THRESHOLD,
                    top_scores = %top.join(" "),
                    "tool selection: group scores"
                );
            }
        }

        self.persist_session_tool_groups(giap_session_id, &selection.groups)
            .await;
        self.remember_permitted(giap_session_id, &permitted).await;
        SessionGroups {
            loaded: selection.groups,
            permitted,
        }
    }

    /// Caches in-process and persists; a failed persist only costs stickiness across restarts.
    async fn persist_session_tool_groups(&self, giap_session_id: &str, groups: &[String]) {
        self.session_tool_groups
            .write()
            .await
            .insert(giap_session_id.to_string(), groups.to_vec());
        if let Some(storage) = &self.giap_session_storage {
            if let Err(e) = storage
                .set_session_tool_groups(giap_session_id, groups)
                .await
            {
                tracing::warn!("tool selection: persisting groups failed: {e}");
            }
        }
    }

    /// The groups this scope may ever hold; the rule is `tool_selection::permitted_groups`.
    fn permitted_groups(&self, scope: &ProfileScope) -> Vec<String> {
        let available = registered_extensions();
        let kept = pond_core::mcp::services::tool_selection::permitted_groups(available, scope);
        if kept.len() != available.len() {
            tracing::info!(
                withheld = available.len() - kept.len(),
                "unidentified speaker: personal-data tool groups withheld"
            );
        }
        kept
    }

    /// Caps history images that keep pixels; the pending turn's image isn't in history yet.
    /// Writes nothing when within the cap: `replace_conversation` rewrites every row to eMMC.
    async fn cap_history_images(&self, goose_sid: &str) {
        use pond_core::models::services::context::image_history::history_image_placeholder;

        let Ok(session) = self.session_manager.get_session(goose_sid, true).await else {
            return;
        };
        let Some(conversation) = session.conversation else {
            return;
        };
        let source: Vec<Message> = conversation.messages().clone();
        if source.is_empty() {
            return;
        }

        // Tool-part messages are never rewritten: providers reject re-keyed tool responses.
        let (had, keep, dropped) = plan_image_cap(&source);
        if dropped == 0 {
            return;
        }

        let rebuilt: Vec<Message> = source
            .iter()
            .enumerate()
            .map(|(i, m)| {
                if had[i] > keep[i] {
                    cap_message_images(m, keep[i], &history_image_placeholder(keep[i]))
                } else {
                    m.clone()
                }
            })
            .collect();

        let capped = goose::conversation::Conversation::new_unvalidated(rebuilt);
        if let Err(e) = self
            .session_manager
            .replace_conversation(goose_sid, &capped)
            .await
        {
            // Non-fatal: a failed cap costs encoder time, never the turn.
            tracing::warn!("image cap: replacing the conversation failed: {e}");
            return;
        }
        tracing::info!(
            target: "giap::trace",
            kind = "image_cap",
            goose_sid = %goose_sid,
            dropped,
            "capped historical images"
        );
    }

    /// Engine → GIAP session id: goose's `agent-session-id` is all an MCP tool can trust.
    /// Linear scan is fine: `sse_semaphore` caps live conversations at 4.
    fn giap_session_for_engine(&self, engine_session_id: &str) -> Option<String> {
        self.goose_session_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(_, goose_sid)| goose_sid.as_str() == engine_session_id)
            .map(|(giap_sid, _)| giap_sid.clone())
    }

    async fn remember_permitted(&self, giap_session_id: &str, permitted: &[String]) {
        self.session_permitted_groups
            .write()
            .await
            .insert(giap_session_id.to_string(), permitted.to_vec());
    }

    /// Embedding provider for memory retrieval; optional, as keyword LIKE search is the fallback.
    pub fn with_embedding_provider(mut self, provider: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    /// Model catalog, so the governor can use `ModelRecord.context_length` over the name heuristic.
    pub fn with_model_repo(mut self, repo: Arc<dyn ModelRepository>) -> Self {
        self.model_repo = Some(repo);
        self
    }

    /// Borrowing provider behind `chat_provider = "mesh"`.
    /// Takes the lock `AppState.mesh_provider` holds, so hot-enabling mesh needs no rebuild.
    pub fn with_mesh_provider(
        mut self,
        provider: Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>,
    ) -> Self {
        self.mesh_provider = provider;
        self
    }

    /// Session storage, so the turn trimmer can splice in the rolling `<conversation-summary>`.
    pub fn with_giap_session_storage(
        mut self,
        storage: Arc<dyn pond_core::user_data::ports::session_storage::SessionStorage>,
    ) -> Self {
        self.giap_session_storage = Some(storage);
        self
    }

    /// Registers a `$data_dir/models/gguf/` model (bare stem or filename) with Goose.
    /// Returns the canonical key (quant suffix collapsed); build `ModelConfig` from it.
    fn register_gguf_model(model_name: &str, data_dir: &std::path::Path) -> String {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, LocalModelEntry, LocalModelStorage,
        };

        let gguf_dir = data_dir.join("models").join("gguf");

        // The file comes from the ORIGINAL name, so an explicit quant still pins its exact file.
        let stem = canonical_model_stem(model_name, &gguf_dir);
        let filename = resolve_gguf_filename(model_name, &gguf_dir);
        let local_path = gguf_dir.join(&filename);
        if !local_path.exists() {
            tracing::warn!(
                "GGUF model file not found at {} — LocalInferenceProvider may fail to load",
                local_path.display()
            );
        }

        match get_registry().lock() {
            Ok(mut registry) => {
                let registry: &mut goose::providers::local_inference::local_model_registry::LocalModelRegistry = &mut registry;
                // Re-register stale rows whose file is gone too; `add_model` upserts in place.
                let needs_register = registry
                    .get_model(&stem)
                    .map(|entry| !entry.local_path.exists())
                    .unwrap_or(true);

                if needs_register {
                    // Keep existing tuning: a default would drop the Jetson `context_size` stamp.
                    let mut settings = registry
                        .get_model(&stem)
                        .map(|entry| entry.settings.clone())
                        .unwrap_or_default();
                    // Probed from this file's own chat template; live turns resolve to this row.
                    settings.tool_calling = crate::model_traits::tool_mode_for_gguf(&local_path);
                    let entry = LocalModelEntry {
                        id: stem.clone(),
                        repo_id: format!("local/{}", stem),
                        filename: filename.clone(),
                        quantization: String::new(),
                        local_path,
                        source_url: String::new(),
                        backend_id: None,
                        // GIAP owns the file; Goose must not delete it as managed storage.
                        storage: LocalModelStorage::ManualPath,
                        settings,
                        size_bytes: 0,
                        mmproj_path: None,
                        mmproj_source_url: None,
                        mmproj_size_bytes: 0,
                        mmproj_checked: false,
                        shard_files: vec![],
                    };
                    match registry.add_model(entry) {
                        Ok(_) => {
                            tracing::info!("Registered GGUF model '{}' in local registry", stem)
                        }
                        Err(e) => tracing::warn!("Could not register GGUF model '{}': {}", stem, e),
                    }
                } else if let Some(entry) = registry.get_model(&stem) {
                    // Re-probe every time so persisted rows can't keep a stale mode (memoised).
                    let mut s = entry.settings.clone();
                    let mode = crate::model_traits::tool_mode_for_gguf(&local_path);
                    if s.tool_calling != mode {
                        s.tool_calling = mode;
                        let _ = registry.update_model_settings(&stem, s);
                    }
                }
            }
            Err(e) => tracing::warn!("GGUF registry lock poisoned: {}", e),
        }
        stem
    }

    pub async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<AgentStreamEvent>>> {
        let settings = self.settings_repo.get().await.unwrap_or_default();
        let session_id = request.session_id.clone();
        let model_role = request.model_role.clone();

        // Fail early: otherwise the engine swaps images for a note and the model bluffs.
        if !request.images.is_empty() && matches!(settings.chat_provider.as_str(), "local" | "gguf")
        {
            let model = settings.chat_model.as_str();
            if !crate::vision_encoder::declares_vision(model) {
                anyhow::bail!(
                    "The active model ({model}) cannot read images. Switch to a vision-capable \
                     model such as gemma-4-E2B-it and try again."
                );
            }
            let ready = self
                .data_dir
                .as_ref()
                .is_some_and(|dd| crate::vision_encoder::mmproj_ready(dd, model));
            if !ready {
                if let Some(ref dd) = self.data_dir {
                    // An image turn means the encoder is wanted; ensure a fetch is running.
                    crate::vision_encoder::ensure_mmproj_available(dd, model);
                }
                anyhow::bail!(
                    "The vision encoder for {model} is still downloading. Image input becomes \
                     available as soon as it finishes - no restart needed. Your message was not \
                     sent."
                );
            }
        }

        // Read by MCP tool handlers (ToolCaller params, outbound HTTP trace events).
        pond_mcp_server::set_last_user_message(&request.message);
        pond_mcp_server::set_current_session_id(&session_id);

        // Every turn so toggles apply at once. Must precede session hydration and ModelConfig:
        // it exports GOOSE_CONTEXT_LIMIT and fills `last_window` for the budget paths.
        self.apply_goose_env_knobs(&settings).await;

        // Goose maintains its own sessions.db with auto-generated IDs.
        let goose_sid = self.resolve_goose_session(&session_id).await;

        // ── 0. Load GIAP builtin MCP extensions (once per session) ────────────
        {
            let needs_load = !self
                .loaded_sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&goose_sid);
            if needs_load {
                let extensions = registered_extensions();
                let total = extensions.len();
                let loaded = self.add_builtin_extensions(extensions, &goose_sid).await;
                self.loaded_sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(goose_sid.clone());

                let tools = self.agent.list_tools(&goose_sid, None).await;
                for t in &tools {
                    tracing::debug!("giap tool available: {}", t.name);
                }
                tracing::info!(
                    target: "giap::trace",
                    "giap extensions ready: {loaded}/{total} loaded, {} tools",
                    tools.len()
                );
            }
        }

        // ── 1-4. System prompt, extras, skills, memory — fetched in parallel ─
        // `memory_limit` memories are injected, from a wider `candidate_limit` pool for ranking
        // (~free: semantic search cosines every row anyway). Unidentified speakers get none.
        let turn_scope = request.profile_scope.clone();
        let memory_limit = if settings.agent_memory_inject && turn_scope.allows_personal_data() {
            Some(settings.agent_memory_limit as usize)
        } else {
            None
        };
        let candidate_limit =
            memory_limit.map(|limit| (limit * MEMORY_CANDIDATE_FANOUT).max(MEMORY_CANDIDATE_FLOOR));

        let (template_result, extras_result, skills_result, recent_memories, relevant_memories) = tokio::join!(
            self.template_repo.get(&settings.prompt_style),
            self.extras_repo.list_active(),
            self.skill_repo.list_active(),
            async {
                match candidate_limit {
                    Some(limit) => self.memory_repo.search_recent(&turn_scope, limit).await,
                    None => Ok(vec![]),
                }
            },
            // Inside the join! so embedding the message overlaps the other fetches.
            async {
                match candidate_limit {
                    Some(limit) => {
                        self.topical_memories(&request.message, &turn_scope, limit)
                            .await
                    }
                    None => vec![],
                }
            },
        );

        // Merge, keeping semantic hits' similarity; `None` marks a recency-only hit.
        let mut memory_candidates: Vec<(MemoryFragment, Option<f32>)> = recent_memories
            .unwrap_or_default()
            .into_iter()
            .map(|m| (m, None))
            .collect();
        for (fragment, similarity) in relevant_memories {
            match memory_candidates
                .iter_mut()
                .find(|(existing, _)| existing.id == fragment.id)
            {
                Some(entry) => entry.1 = similarity,
                None => memory_candidates.push((fragment, similarity)),
            }
        }

        let template_content = template_result
            .ok()
            .flatten()
            .map(|t| t.content)
            .unwrap_or_else(|| fallback_prompt().to_string());

        // Voice = the instance flag (CLI --voice) or the request's (desktop voice pipeline).
        let voice_instance = self.voice_mode.load(std::sync::atomic::Ordering::Relaxed);
        let is_voice = Self::voice_turn(voice_instance, request.voice_mode);

        // Voice disables thinking (it wastes TTS time and can leak as speech). Drives both the
        // prompt's <thinking> section and the engine's `enable_thinking`, which must agree.
        let thinking_enabled = Self::thinking_section_applies(
            &settings.thinking_mode,
            &settings.chat_provider,
            &settings.chat_model,
            self.data_dir.as_deref(),
            is_voice,
        );

        // One profile per turn, from what `apply_goose_env_knobs` cached, for every budget below.
        let turn_profile = self.turn_profile(&session_id).await;
        let effective_ctx = turn_profile.context_window_tokens;

        let prompt_state = {
            use chrono::Local;
            let now = Local::now();
            // Uses the clamped prompt window: a huge KV cache must not pick the verbose tier.
            let compact_prompt = turn_profile.use_compact_prompt();

            // Prose for external MCP tools only; builtins reach the model as native schemas.
            let available_tools: Vec<String> = match &self.tool_registry {
                Some(registry) => registry.prompt_description_lines(compact_prompt).await,
                None => Vec::new(),
            };
            let has_prose_tools = !available_tools.is_empty();

            PromptState {
                current_date: now.format("%A, %-d %B %Y").to_string(),
                current_time: Self::format_current_time(now, is_voice),
                voice_mode: is_voice,
                available_tools,
                thinking_enabled,
                compact_prompt,
                // The chat template already carries the tools JSON; don't list the schemas twice.
                native_tools_json: matches!(
                    settings.chat_provider.as_str(),
                    "local" | "gguf" | "mistralrs"
                ),
                // Whether any tools are offered. `GIAP_NO_TOOLS` is checked here too: user MCP
                // servers bypass registration, and the shim would strip their tools anyway.
                tools_offered: !pond_core::mcp::domain::tool_group::no_tools_env_set()
                    && (!registered_extensions().is_empty() || has_prose_tools),
                prefix_hash: None, // filled by build_prompt_partition below
            }
        };

        // Vision models otherwise deny seeing images and confuse attachments with camera frames.
        // Never for text-only models (invites hallucination) or voice (no attachment reaches it).
        let template_content = Self::apply_vision_section(
            template_content,
            Self::vision_section_applies(
                &settings.chat_provider,
                &settings.chat_model,
                voice_instance,
            ),
            prompt_state.compact_prompt,
        );

        // Date/time and profile ride the user message so the system+tools prefix stays stable.
        let mut dynamic_suffix_for_user_msg = String::new();

        // ── Partitioned prompt: static prefix + dynamic suffix ──────────
        // The prefix is rebuilt only when its hash changes, so the engine reuses its KV cache.
        if settings.prefix_cache_prompt {
            // Profile is resolved at the API edge (identity boundary); it lands in the dynamic
            // suffix, so a mid-session speaker switch costs no re-prefill.
            let partition = build_prompt_partition(
                &settings,
                request.profile_context.as_ref(),
                &prompt_state,
                &template_content,
            );

            // The shim rebuilds any Goose-mutated system prompt from this.
            self.shim_controls
                .set_system_prefix(partition.static_prefix.clone());

            // Guard dropped before any `.await` to keep the future `Send`.
            let prefix_changed = {
                let last_hash = self
                    .last_prefix_hash
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *last_hash != partition.prefix_hash
            };

            if prefix_changed {
                tracing::info!(
                    new_hash = %partition.prefix_hash,
                    "Static prefix changed — rebuilding system prompt"
                );
                self.agent
                    .override_system_prompt(partition.static_prefix)
                    .await;
                {
                    let mut last_hash = self
                        .last_prefix_hash
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    *last_hash = partition.prefix_hash;
                }
                // `note_prefix_rebuilt` keeps the reason; this turn's trimmer must still read Cold.
                self.note_prefix_invalidated(InvalidationReason::PromptChanged);
                self.note_prefix_rebuilt(partition.prefix_hash);
            } else {
                tracing::debug!(
                    hash = %partition.prefix_hash,
                    "Static prefix unchanged — skipping override_system_prompt (KV-cache reuse)"
                );
                // The only place a prefix becomes warm; everything else only invalidates.
                self.note_prefix_served();
            }

            dynamic_suffix_for_user_msg = partition.dynamic_suffix;
        } else {
            // Rebuilds every turn. Still needs the profile (name, language, atypical speech).
            let system_prompt = pond_core::prompts::build_system_prompt_from_template_full(
                &settings,
                request.profile_context.as_ref(),
                Some(&prompt_state),
                &template_content,
            );
            self.shim_controls.set_system_prefix(system_prompt.clone());
            self.agent.override_system_prompt(system_prompt).await;
            // Rebuilt every turn, so always cold for the trimmer.
            self.note_prefix_invalidated(InvalidationReason::PromptChanged);
        }

        // The only delivery path for extras/skills: the shim appends it after vetoing Goose's.
        // Not `extend_system_prompt`: Goose never removes extras, so deactivated ones would linger.
        let mut shim_appendix: Vec<String> = Vec::new();

        // Outside prefix_hash by design; <extension-notes> sets it apart from core prompt sections.
        if let Ok(extras) = extras_result {
            for extra in extras {
                let body = format!(
                    "<extension-notes name=\"{}\">\n{}\n</extension-notes>",
                    extra.key, extra.instruction
                );
                shim_appendix.push(body);
            }
        }

        // Only name + description per turn; `giap-device__load_skill` fetches the rest on demand.
        // The same text feeds tool selection (6c), so a skill can pull in the groups it needs.
        let mut skill_selection_signal = String::new();
        if let Ok(skills) = skills_result {
            if !skills.is_empty() {
                let mut skills_body = String::from(
                    "Active user-defined skills, as \"name: description\". When one looks \
                     relevant to what the user is asking, call giap-device__load_skill(name) \
                     to get its full instructions before acting on it.",
                );
                for skill in &skills {
                    skills_body.push_str(&format!("\n- {}: {}", skill.name, skill.description));
                    skill_selection_signal
                        .push_str(&format!("{}: {}\n", skill.name, skill.description));
                }
                shim_appendix.push(format!(
                    "<extension-notes name=\"skills\">\n{}\n</extension-notes>",
                    skills_body
                ));
            }
        }

        self.shim_controls
            .session(&goose_sid)
            .set_turn_appendix(if shim_appendix.is_empty() {
                None
            } else {
                Some(shim_appendix.join("\n\n"))
            });

        // ── Token-budgeted memory injection ──────────────────────────────
        // Memories ride the user message's <system-context> to keep the prefix KV-stable.
        let mut memory_block_for_user_msg = String::new();
        // A preamble budget (clamped window): injected memories are re-prefilled every turn.
        let compaction_profile = &turn_profile;

        if !memory_candidates.is_empty() {
            // Blended ranking, so a topical memory can displace the high-importance identity block.
            pond_core::user_data::services::memory_relevance::rank_by_relevance(
                &mut memory_candidates,
                chrono::Utc::now(),
            );

            memory_candidates.truncate(compaction_profile.max_memory_fragments);

            let token_budget = compaction_profile.memory_token_budget;
            let mut tokens_used: usize = 0;
            let mut budgeted: Vec<&MemoryFragment> = Vec::new();
            for (m, _) in &memory_candidates {
                let estimated_tokens = m.content.len() / 4 + 1;
                if tokens_used + estimated_tokens > token_budget && !budgeted.is_empty() {
                    break;
                }
                tokens_used += estimated_tokens;
                budgeted.push(m);
            }

            if !budgeted.is_empty() {
                let block = budgeted
                    .iter()
                    .map(|m| {
                        let seg = m
                            .segment
                            .as_ref()
                            .map(|s| format!("{:?}", s).to_lowercase())
                            .unwrap_or_default();
                        if seg.is_empty() {
                            format!("- {}", m.content)
                        } else {
                            format!("- [{}] {}", seg, m.content)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                tracing::debug!(
                    fragments_injected = budgeted.len(),
                    fragments_available = memory_candidates.len(),
                    tokens_used,
                    token_budget,
                    semantic = self.embedding_provider.is_some(),
                    "Memory injection (budget from CompactionProfile ctx={})",
                    effective_ctx,
                );

                memory_block_for_user_msg = block;

                // Feeds decay tracking; spawned to keep sequential DB writes off the hot path.
                let ids: Vec<String> = budgeted.iter().map(|m| m.id.clone()).collect();
                let repo = self.memory_repo.clone();
                tokio::spawn(async move {
                    for id in ids {
                        let _ = repo.record_access(&id).await;
                    }
                });
            }
        }

        // ── 4b. Upcoming schedules context ──────────────────────────────────
        // TODO: inject schedule context once GooseAdapter has a SchedulerPort ref.

        // ── 5. Provider hot-swap ──────────────────────────────────────────────
        // Fail the turn here: continuing ends in Goose's opaque model-config error instead.
        if let Err(e) = self
            .ensure_provider_current(&settings, &goose_sid, thinking_enabled)
            .await
        {
            tracing::warn!(
                session_id = %goose_sid,
                error = %e,
                "provider not configured for this session — refusing the turn"
            );
            anyhow::bail!(
                "I have no chat model configured to answer with. {e}. Set a provider and \
                 model in Settings, then try again."
            );
        }

        // ── 6. Extension cleanup ──────────────────────────────────────────────
        // Strip Goose default extensions that would pollute the prompt, once per session.
        {
            let already_stripped = self
                .defaults_stripped
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&goose_sid);
            if !already_stripped {
                // Shared list: the prompt filter below and the orchestrator's planner read it too.
                let strip_list: &[&str] = &crate::orchestrator::GOOSE_STRIPPED_BUILTINS;
                // Only loaded ones: `remove_extension` costs a DB read and write even when absent.
                let present: std::collections::HashSet<String> = self
                    .agent
                    .list_extensions()
                    .await
                    .into_iter()
                    .map(|e| e.to_string())
                    .collect();
                let user_exts = self.user_extensions.read().await;
                for ext in strip_list {
                    if !user_exts.contains(*ext) && present.contains(*ext) {
                        self.agent.remove_extension(ext, &goose_sid).await.ok();
                    }
                }
                drop(user_exts);
                self.defaults_stripped
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(goose_sid.clone());
                // Invalidate tool cache since extensions changed.
                *self.cached_tools.write().await = None;
            }
        }

        // ── 6b. Extension tool discovery ─────────────────────────────────────
        // Re-queried only after the cache is invalidated (extensions added/removed/stripped).
        let allowed_tools = {
            let cache = self.cached_tools.read().await;
            if let Some(cached) = cache.as_ref() {
                cached.clone()
            } else {
                drop(cache);
                let all_tools = self.agent.list_tools(&goose_sid, None).await;
                let tools_set: std::collections::HashSet<String> =
                    all_tools.iter().map(|t| t.name.to_string()).collect();

                let mut ext_map: HashMap<String, Vec<(String, String)>> = HashMap::new();
                for tool in &all_tools {
                    let name = tool.name.as_ref();
                    if let Some(sep) = name.find("__") {
                        let ext_name = &name[..sep];
                        let tool_name = &name[sep + 2..];
                        let desc = tool
                            .description
                            .as_deref()
                            .unwrap_or("No description")
                            .to_string();
                        ext_map
                            .entry(ext_name.to_string())
                            .or_default()
                            .push((tool_name.to_string(), desc));
                    }
                }

                // `default` and `suggestions` carry no tools: filtered here, never stripped.
                let is_builtin = |name: &str| {
                    registered_extensions().iter().any(|e| e == name)
                        || crate::orchestrator::GOOSE_STRIPPED_BUILTINS.contains(&name)
                        || matches!(name, "default" | "suggestions")
                };

                let external_extensions: Vec<(String, Vec<(String, String)>)> = ext_map
                    .into_iter()
                    .filter(|(name, _)| !is_builtin(name.as_str()))
                    .collect();

                if !external_extensions.is_empty() {
                    let mut desc_lines = Vec::with_capacity(external_extensions.len() * 6);
                    desc_lines.push("# MCP Extensions".to_string());
                    desc_lines.push(
                        "The following MCP extensions are loaded. Use their tools when the user's request matches."
                            .to_string(),
                    );

                    for (ext_name, tools) in &external_extensions {
                        desc_lines.push(format!("\n## {}", ext_name));
                        for (tool_name, tool_desc) in tools {
                            desc_lines.push(format!("  - {}: {}", tool_name, tool_desc));
                        }
                    }

                    let ext_description = desc_lines.join("\n");

                    tracing::info!(
                        extensions = external_extensions.len(),
                        "Injecting {} external extension(s) into system prompt",
                        external_extensions.len(),
                    );

                    // Shim only: Goose's copy would land in an appendix the veto discards.
                    self.shim_controls
                        .set_extension_appendix(Some(ext_description));
                } else {
                    self.shim_controls.set_extension_appendix(None);
                }

                *self.cached_tools.write().await = Some(tools_set.clone());
                tools_set
            }
        };

        // ── 6c. Per-session tool relevance ───────────────────────────────────
        // Goose always sends the full union, so narrowing is enforced by the shim per call.
        // It only picks which schemas ride the prompt (cost), not whether tools get used.
        let mut dormant_groups_note = String::new();
        // Entitlement (all it could widen to; bounds delegation). `None` in "all" mode.
        let mut entitled_tools: Option<HashSet<String>> = None;
        let allowed_tools = if settings.tool_selection_narrows() {
            let groups = self
                .resolve_session_tool_groups(
                    &session_id,
                    &request.message,
                    &memory_block_for_user_msg,
                    &skill_selection_signal,
                    &turn_scope,
                    settings.tool_selection_is_minimal(),
                )
                .await;
            let selected: HashSet<String> =
                pond_core::mcp::services::tool_selection::filter_tools_by_groups(
                    allowed_tools.iter(),
                    &groups.loaded,
                )
                .into_iter()
                .collect();

            // Permitted, not registered: a group withheld from this speaker isn't offered either.
            dormant_groups_note = pond_core::mcp::services::tool_selection::dormant_groups_note(
                &groups.permitted,
                &groups.loaded,
            );

            entitled_tools = Some(
                pond_core::mcp::services::tool_selection::filter_tools_by_groups(
                    allowed_tools.iter(),
                    &groups.permitted,
                )
                .into_iter()
                .collect(),
            );

            tracing::info!(
                target: "giap::trace",
                kind = "tool_selection",
                session_id = %session_id,
                // The real mode: this is the only per-turn record of it.
                mode = %settings.tool_selection_mode,
                groups = ?groups.loaded,
                groups_total = groups.permitted.len(),
                groups_registered = registered_extensions().len(),
                tools = selected.len(),
                tools_total = allowed_tools.len(),
            );
            selected
        } else {
            tracing::info!(
                target: "giap::trace",
                kind = "tool_selection",
                session_id = %session_id,
                mode = "all",
                groups_total = registered_extensions().len(),
                tools = allowed_tools.len(),
                tools_total = allowed_tools.len(),
            );
            allowed_tools
        };

        // ── 6d. Guest tool subtraction, in BOTH selection modes ──────────────
        // The group-level pass only runs when narrowing (not the default "all"); this set is
        // what the shim gets, so every mode must subtract here.
        let allowed_tools = if turn_scope.excludes_everything() {
            let before = allowed_tools.len();
            let kept: HashSet<String> =
                pond_core::mcp::services::tool_selection::subtract_guest_denied_tools(
                    allowed_tools.iter(),
                )
                .into_iter()
                .collect();
            if kept.len() != before {
                tracing::info!(
                    target: "giap::trace",
                    kind = "guest_tools_withheld",
                    session_id = %session_id,
                    removed = before - kept.len(),
                    kept = kept.len(),
                    "unidentified speaker: personal-data tools withheld from the turn"
                );
            }
            kept
        } else {
            allowed_tools
        };

        // Same subtraction for the entitlement: the delegation authority is built from it.
        let entitled_tools = entitled_tools.map(|tools| {
            if turn_scope.excludes_everything() {
                pond_core::mcp::services::tool_selection::subtract_guest_denied_tools(tools.iter())
                    .into_iter()
                    .collect()
            } else {
                tools
            }
        });

        // A recipe's YAML `extensions:` narrow this turn to those groups, in every mode.
        let allowed_tools = if let Some(allowlist) = &request.tool_group_allowlist {
            let before = allowed_tools.len();
            let kept: HashSet<String> = allowed_tools
                .into_iter()
                .filter(|tool| {
                    pond_core::mcp::domain::tool_group::group_of_tool(tool)
                        .is_some_and(|group| allowlist.iter().any(|g| g == group))
                })
                .collect();
            tracing::info!(
                target: "giap::trace",
                kind = "recipe_tools_restricted",
                session_id = %session_id,
                allowlist = ?allowlist,
                removed = before - kept.len(),
                kept = kept.len(),
                "recipe extensions restricted this turn's tools"
            );
            kept
        } else {
            allowed_tools
        };

        tracing::debug!(target: "pond_adapters_goose::goose_agent", "Allowed tools for turn: {:?}", allowed_tools);

        // Vetoes anything Goose adds (platform tools, final_output). The tool-call guard reads
        // this handle live, so a group enabled mid-turn is admitted at once.
        let session_controls = self.shim_controls.session(&goose_sid);
        session_controls.set_allowed_tools(allowed_tools.clone());

        // ── 7. GooseMode from model_role ──────────────────────────────────────
        let goose_mode = GooseMode::Auto;
        self.agent
            .update_goose_mode(goose_mode, &goose_sid)
            .await
            .ok();

        // ── 8. Run the agentic loop ───────────────────────────────────────────
        // Everything per-turn rides the user message so the system+tools prefix stays byte-stable.
        // The shim strips Goose's <turn-context>, so this note is the model's only turn budget.
        let max_turns = settings.effective_max_turns(is_voice);
        let turn_budget_block = pond_core::models::services::turn_budget::turn_budget_note(
            (!settings.turns_are_uncapped(is_voice)).then_some(max_turns),
        );

        let user_text = {
            let mut msg = String::with_capacity(512 + request.message.len());
            // `<user-message>` must come FIRST: stripping the trailing envelope from past turns
            // is then a suffix cut, keeping the prefix byte-identical for KV reuse.
            msg.push_str("<user-message>\n");
            msg.push_str(&request.message);
            msg.push_str("\n</user-message>\n");
            // Always emitted, since the budget note is unconditional.
            msg.push_str("<system-context>\n");
            if !dynamic_suffix_for_user_msg.is_empty() {
                msg.push_str(&dynamic_suffix_for_user_msg);
                msg.push('\n');
            }
            if !memory_block_for_user_msg.is_empty() {
                msg.push_str("<memories>\n");
                msg.push_str(&memory_block_for_user_msg);
                msg.push_str("\n</memories>\n");
            }
            // Session-specific, so never in the system prompt; empty when nothing is dormant.
            if !dormant_groups_note.is_empty() {
                msg.push_str(&dormant_groups_note);
                msg.push('\n');
            }
            msg.push_str(&turn_budget_block);
            msg.push('\n');
            // Last, nearest where the model writes: restates the part of the prefix that decays.
            msg.push_str(&pond_core::models::services::answer_contract::answer_contract());
            msg.push('\n');
            msg.push_str("</system-context>");
            msg
        };
        // Kept in pieces: empty-turn recovery re-engages with a steered variant of the text.
        let turn_text = user_text.clone();
        let turn_images = request.images.clone();
        // An image turn forfeits KV retention; noted before anything below reads the posture.
        if !turn_images.is_empty() {
            self.note_prefix_invalidated(InvalidationReason::MultimodalTurn);
        }
        let turn_goose_sid = goose_sid.clone();

        let agent_clone = self.agent.clone();
        let last_prompt_tokens_map = self.last_prompt_tokens_handle();
        // Live handle for the tool-call guard inside the 'static stream closure.
        let guard_controls = session_controls.clone();

        // ── History image cap ────────────────────────────────────────────
        // Goose owns compaction; the image cap stays because each replayed image re-runs the
        // mmproj encoder (0.7-2.7 s), a cost goose can't see.
        self.cap_history_images(&goose_sid).await;

        let user_msg_len = request.message.len();
        let turn_start = std::time::Instant::now();

        // Resolved here so the 'static closure carries a decision, not a repository handle.
        let emit_reasoning = Self::reasoning_frames_enabled(settings.show_thinking, is_voice);

        // An owned counter for the 'static stream closure.
        let reasoning_counter = self.token_counter_handle().await;

        // Cancelled when the stream drops (e.g. voice interrupt); Goose checks it each turn.
        let cancel_token = CancellationToken::new();
        let cancel_guard = cancel_token.clone().drop_guard();

        // ── This turn's delegation authority ──────────────────────────────────
        // Delegation ceiling: the unforgeable API-edge scope and the post-6d entitlement, so it
        // must stay after 6d. The lease dies with the stream.
        let authority_lease = self.turn_authorities.publish(
            &goose_sid,
            pond_core::shared::domain::orchestration::DelegationAuthority::for_turn(
                session_id.clone(),
                turn_scope.clone(),
                entitled_tools
                    .as_ref()
                    .unwrap_or(&allowed_tools)
                    .iter()
                    .map(String::as_str),
            ),
            cancel_token.clone(),
        );

        // For the device claim; from the same settings load `turn_profile` budgeted against.
        let turn_provider = settings.chat_provider.clone();
        let device_session_id = session_id.clone();

        // ── Where this turn hears about its own delegations ──────────────────
        // Keyed by the GIAP session id (a `TaskSpec`'s parent); frames before subscribing are
        // dropped. Dies with the stream, taking its bus entry with it.
        let mut progress = crate::orchestrator::process_progress_bus().subscribe(&session_id);

        let stream = async_stream::stream! {
            // Hold the guard — dropped when the stream is dropped → cancels token.
            let _guard = cancel_guard;
            // Dropped with the stream, revoking this turn's authority to delegate.
            let _authority_lease = authority_lease;

            // ── Invariant 3: this turn's claim on the device ──────────────────
            // One KV prefix per model slot: a child replying mid-turn would overwrite ours.
            // Taken inside the stream so the client sees it waiting; `None` for remote providers.
            let _device = crate::orchestrator::claim_device_for_turn(
                &device_session_id,
                &turn_provider,
                &cancel_token,
            ).await;

            yield Ok(AgentStreamEvent::Status { content: "Agent working...".to_string() });
            let mut total_output_chars: usize = 0;
            // Track tool call ID → tool name so ToolResult events carry the tool name.
            let mut tool_id_to_name: HashMap<String, String> = HashMap::new();
            // Calls the allow-set guard refused to surface; their results are dropped too.
            let mut suppressed_tool_ids: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            // Wall-clock start per tool call (keyed by Goose tool-call ID) for latency.
            let mut tool_call_starts: HashMap<String, std::time::Instant> = HashMap::new();

            // ── Round-trip attribution ───────────────────────────────────────
            // What `AgentEvent::Usage` lacks: the gap since the last inference and the tools run
            // in between (telling tool round-trips from re-engagements or compactions).
            let mut last_inference_end: Option<std::time::Instant> = None;
            let mut tools_since_inference: u32 = 0;

            // ── Repetition guard ─────────────────────────────────────────────
            // Outside `'attempts`: re-engaging is the same question, so no fresh budget.
            let mut goal_rechecks: u32 = 0;
            let mut tool_call_counts: HashMap<(String, u64), usize> = HashMap::new();
            let mut guard_tripped = false;
            // Set when a guard trips: goose's pull-based stream only flushes its history once
            // polled to the end, so keep draining after cancel, up to this bounded deadline.
            let mut drain_deadline: Option<tokio::time::Instant> = None;

            tracing::info!(
                target: "giap::trace",
                kind = "turn_start",
                session_id = %session_id,
                // Goose's errors name this id, so it ties engine failures back to the turn.
                goose_sid = %turn_goose_sid,
                model = %settings.chat_model,
                provider = %settings.chat_provider,
                message_len = user_msg_len,
            );

                let mut turn_stats = pond_core::shared::domain::turn_stats::TurnStats::default();
                let mut saw_usage = false;
                // ── Empty-turn recovery (GIAP-owned) ─────────────────────────
                // An empty turn never reaches the user as silence. Goose hands it straight back
                // (GOOSE_MAX_EMPTY_TURN_RETRIES=0); GIAP re-engages with a changed prompt.
                let mut attempt: usize = 0;
                // One passage in flight; every attempt flushes it, so none crosses a re-engagement.
                let mut reasoning = ReasoningCoalescer::default();
                'attempts: loop {
                    let attempt_text = if attempt == 0 {
                        turn_text.clone()
                    } else {
                        format!("{turn_text}\n\n{EMPTY_TURN_STEER}")
                    };
                    let attempt_msg =
                        attach_images(Message::user().with_text(&attempt_text), &turn_images);
                    // Arms Goose's completeness check. Session-keyed (streams run concurrently),
                    // and the RAW request: `turn_text` carries household memories.
                    agent_clone
                        .set_session_goal(
                            &turn_goose_sid,
                            (settings.goal_check_enabled && !request.warmup)
                                .then(|| request.message.clone()),
                        )
                        .await;
                    let attempt_cfg = goose::agents::types::SessionConfig {
                        id: turn_goose_sid.clone(),
                        schedule_id: None,
                        // Tighter for voice; a 0 setting is already the uncapped sentinel here.
                        max_turns: Some(max_turns),
                        retry_config: None,
                    };
                    // Text or a tool call — anything the user actually receives.
                    let mut produced_visible = false;
                    // Errored, not quiet: re-engaging an error only repeats it.
                    let mut stream_failed = false;
                let mut goose_stream = match agent_clone.reply(attempt_msg, attempt_cfg, Some(cancel_token.clone())).await {
                    Ok(s) => s,
                    Err(e) => {
                        yield Ok(AgentStreamEvent::Error { content: e.to_string() });
                        return;
                    }
                };

                // The label is load-bearing: a source test finds this loop's closing brace by it.
                'engine: loop {
                    // Also polls delegation progress: the child runs inside `goose_stream`'s poll.
                    let step = if let Some(deadline) = drain_deadline {
                        match tokio::time::timeout_at(
                            deadline,
                            crate::orchestrator::next_parent_step(&mut goose_stream, &mut progress),
                        ).await {
                            Ok(step) => step,
                            Err(_) => {
                                tracing::warn!(
                                    session_id = %session_id,
                                    "the engine did not wind up within the guard drain timeout; \
                                     dropping the stream, so this turn's last message may be \
                                     missing from the engine's own history",
                                );
                                break 'engine;
                            }
                        }
                    } else {
                        crate::orchestrator::next_parent_step(&mut goose_stream, &mut progress).await
                    };
                    let event_result = match step {
                        crate::orchestrator::ParentStep::Progress(frame) => {
                            // Yielded, never persisted: only the result is, as a tool response.
                            if drain_deadline.is_none() {
                                yield Ok(frame.into());
                            }
                            continue 'engine;
                        }
                        crate::orchestrator::ParentStep::EngineEnded => break 'engine,
                        crate::orchestrator::ParentStep::Engine(event_result) => event_result,
                    };
                    if drain_deadline.is_some() {
                        // Draining: keep polling so goose flushes, but surface nothing.
                        continue 'engine;
                    }
                    match event_result {
                        Ok(event) => match event {
                            goose::agents::AgentEvent::Message(msg) => {
                                // Reasoning goes first and never sets `produced_visible`.
                                // Counted outside the display gate: it was decoded either way.
                                *turn_stats.reasoning_tokens.get_or_insert(0) +=
                                    Self::count_reasoning_tokens(&msg, reasoning_counter.as_ref());
                                // Buffer per passage: on local/gguf one message is one token.
                                reasoning.push(&msg, emit_reasoning);
                                let ends_block = Self::message_ends_reasoning(&msg);
                                let overflowed = reasoning.over_cap();
                                if overflowed {
                                    tracing::warn!(
                                        session_id = %session_id,
                                        bytes = reasoning.len(),
                                        "reasoning passage exceeded the coalescer cap; \
                                         emitting it as a partial block",
                                    );
                                }
                                if ends_block || overflowed {
                                    if let Some(content) = reasoning.flush() {
                                        yield Ok(AgentStreamEvent::Thinking { content });
                                    }
                                }
                                for content in &msg.content {
                                    match content {
                                        goose::conversation::message::MessageContent::ToolRequest(tr) => {
                                            if let Ok(tool_call) = &tr.tool_call {
                                                let tool_name = tool_call.name.to_string();
                                                // Read LIVE: `enable_tool_group` widens the set
                                                // mid-turn and the next call must be admitted.
                                                if !guard_controls.is_tool_allowed(&tool_name) {
                                                    // Not a block: Goose runs the tool anyway, so
                                                    // this only hides the call and its result.
                                                    suppressed_tool_ids.insert(tr.id.clone());
                                                    tracing::warn!(
                                                        tool = %tool_name,
                                                        tool_id = %tr.id,
                                                        "tool call outside this session's allow-set; suppressing its call and result events (the tool itself still runs)",
                                                    );
                                                    continue;
                                                }
                                                // Repeat guard; suppressed calls never count.
                                                let fingerprint = canonical_args_fingerprint(
                                                    tool_call.arguments.as_ref(),
                                                );
                                                let seen = tool_call_counts
                                                    .entry((tool_name.clone(), fingerprint))
                                                    .or_insert(0);
                                                *seen += 1;
                                                if *seen > MAX_IDENTICAL_TOOL_CALLS_PER_TURN {
                                                    let repeats = *seen;
                                                    tracing::warn!(
                                                        tool = %tool_name,
                                                        repeats,
                                                        "the model called one tool with identical arguments {repeats} times in a turn; stopping it",
                                                    );
                                                    tracing::info!(
                                                        target: "giap::trace",
                                                        kind = "turn_repetition_guard",
                                                        session_id = %session_id,
                                                        reason = "identical_tool_call",
                                                        tool = %tool_name,
                                                        repeats,
                                                        goal_rechecks,
                                                    );
                                                    // Disarm, or the NEXT turn starts armed with
                                                    // this question (we skip Goose's own clear).
                                                    agent_clone.set_session_goal(&turn_goose_sid, None).await;
                                                    guard_tripped = true;
                                                    yield Ok(AgentStreamEvent::TurnLimitReached {
                                                        max_turns: MAX_IDENTICAL_TOOL_CALLS_PER_TURN as u32,
                                                    });
                                                    // Cancel, then drain: goose checks the token
                                                    // only when polled; the repeat never runs.
                                                    cancel_token.cancel();
                                                    drain_deadline = Some(
                                                        tokio::time::Instant::now() + GUARD_DRAIN_TIMEOUT,
                                                    );
                                                    continue 'engine;
                                                }
                                                tool_id_to_name.insert(tr.id.clone(), tool_name.clone());
                                                tool_call_starts.insert(tr.id.clone(), std::time::Instant::now());
                                                // Classifies the NEXT round-trip as tool-driven.
                                                tools_since_inference += 1;
                                                tracing::info!(
                                                    target: "giap::trace",
                                                    kind = "tool_call",
                                                    session_id = %session_id,
                                                    tool = %tool_name,
                                                    tool_id = %tr.id,
                                                );
                                                produced_visible = true;
                                                yield Ok(AgentStreamEvent::ToolCall {
                                                    id: tr.id.clone(),
                                                    tool: tool_name,
                                                    input: tool_call.arguments.clone().map(serde_json::Value::Object),
                                                });
                                            }
                                        }
                                        goose::conversation::message::MessageContent::SystemNotification(n)
                                            if n.msg.starts_with(GOOSE_GOAL_NOTIFICATION_PREFIX) =>
                                        {
                                            // Re-armed check; genuine multi-tool turns emit none.
                                            goal_rechecks += 1;
                                            if goal_rechecks > MAX_GOAL_RECHECKS_PER_TURN {
                                                tracing::warn!(
                                                    goal_rechecks,
                                                    "the completeness check re-armed {goal_rechecks} times in one turn; stopping it re-answering",
                                                );
                                                tracing::info!(
                                                    target: "giap::trace",
                                                    kind = "turn_repetition_guard",
                                                    session_id = %session_id,
                                                    reason = "goal_recheck",
                                                    goal_rechecks,
                                                );
                                                agent_clone.set_session_goal(&turn_goose_sid, None).await;
                                                guard_tripped = true;
                                                yield Ok(AgentStreamEvent::TurnLimitReached {
                                                    max_turns: MAX_GOAL_RECHECKS_PER_TURN,
                                                });
                                                // Drain, don't break: the shown answer is still
                                                // unflushed in goose's `messages_to_add`.
                                                cancel_token.cancel();
                                                drain_deadline = Some(
                                                    tokio::time::Instant::now() + GUARD_DRAIN_TIMEOUT,
                                                );
                                                continue 'engine;
                                            }
                                        }
                                        goose::conversation::message::MessageContent::ToolResponse(tr) => {
                                            // A suppressed call's result must never be streamed.
                                            if suppressed_tool_ids.remove(&tr.id) {
                                                tracing::warn!(
                                                    target: "giap::trace",
                                                    kind = "tool_result_suppressed",
                                                    session_id = %session_id,
                                                    tool_id = %tr.id,
                                                    "dropped the result of a tool outside this session's allow-set",
                                                );
                                                tool_call_starts.remove(&tr.id);
                                                continue;
                                            }
                                            // Surface errors too: the model sees them anyway.
                                            let (content_text, failed) = match &tr.tool_result {
                                                Ok(tool_result) => (
                                                    tool_result
                                                        .content
                                                        .iter()
                                                        .filter_map(|c| match c.deref() {
                                                            rmcp::model::RawContent::Text(t) => Some(t.text.clone()),
                                                            _ => None,
                                                        })
                                                        .collect::<Vec<_>>()
                                                        .join("\n"),
                                                    false,
                                                ),
                                                Err(e) => (format!("Error: {e}"), true),
                                            };

                                            let tool_name = tool_id_to_name
                                                .get(&tr.id)
                                                .cloned()
                                                .unwrap_or_default();
                                            let tool_latency_ms = tool_call_starts
                                                .remove(&tr.id)
                                                .map(|s| s.elapsed().as_millis() as u64)
                                                .unwrap_or(0);
                                            tracing::info!(
                                                target: "giap::trace",
                                                kind = "tool_result",
                                                session_id = %session_id,
                                                tool = %tool_name,
                                                tool_id = %tr.id,
                                                latency_ms = tool_latency_ms,
                                                result_len = content_text.len(),
                                                failed = failed,
                                            );
                                            if failed {
                                                tracing::warn!(
                                                    tool = %tool_name,
                                                    tool_id = %tr.id,
                                                    error = %content_text,
                                                    "tool call failed",
                                                );
                                            }
                                            yield Ok(AgentStreamEvent::ToolResult {
                                                id: tr.id.clone(),
                                                tool: tool_name,
                                                content: content_text,
                                            });
                                        }
                                        _ => {}
                                    }
                                }
                                // Raw: the SSE ThoughtFilter strips think tags across chunks, and a
                                // per-chunk strip here would eat close tags it is waiting for.
                                let raw_text = msg.as_concat_text();
                                if !raw_text.is_empty() && raw_text.trim() == GOOSE_EMPTY_TURN_MESSAGE {
                                    // Swallowed so the recovery loop below re-engages.
                                    tracing::warn!(
                                        session_id = %session_id,
                                        "goose reported an empty turn",
                                    );
                                } else if !raw_text.is_empty() && looks_like_leaked_scaffold(&raw_text) {
                                    // Echoed prompt scaffolding gets the empty-turn recovery.
                                    tracing::warn!(
                                        session_id = %session_id,
                                        leaked = %raw_text,
                                        "model echoed internal scaffolding instead of answering",
                                    );
                                } else if !raw_text.is_empty() {
                                    // Goose streams its turn-cap notice as text; also re-emit it
                                    // as a structured event so a client can offer Continue.
                                    let hit_turn_limit = raw_text.trim() == GOOSE_MAX_TURNS_MESSAGE;
                                    produced_visible = true;
                                    total_output_chars += raw_text.len();
                                    yield Ok(AgentStreamEvent::Text { content: raw_text });
                                    if hit_turn_limit {
                                        tracing::info!(
                                            target: "giap::trace",
                                            kind = "turn_limit_reached",
                                            session_id = %session_id,
                                            max_turns,
                                        );
                                        yield Ok(AgentStreamEvent::TurnLimitReached { max_turns });
                                    }
                                }
                            }
                            goose::agents::AgentEvent::HistoryReplaced(_) => {
                                yield Ok(AgentStreamEvent::Status { content: "Compacting context...".to_string() });
                            }
                            // Per inference: the last input is the context load; outputs sum.
                            goose::agents::AgentEvent::Usage(pu) => {
                                saw_usage = true;
                                turn_stats.inference_count += 1;

                                // ── The round-trip line ──────────────────────
                                // `gap_ms`: time since the last inference, unseen by the engine.
                                let now = std::time::Instant::now();
                                let gap_ms = last_inference_end
                                    .map(|t| now.duration_since(t).as_millis() as u64);
                                let engine_ttft = pu.stats.as_ref().and_then(|s| s.time_to_first_token_ms);
                                let engine_prefill = pu.stats.as_ref().and_then(|s| s.prefill_ms);
                                tracing::info!(
                                    target: "giap::trace",
                                    kind = "inference",
                                    session_id = %session_id,
                                    n = turn_stats.inference_count,
                                    gap_ms = gap_ms.unwrap_or(0),
                                    since_turn_ms = now.duration_since(turn_start).as_millis() as u64,
                                    engine_ttft_ms = engine_ttft.unwrap_or(0),
                                    engine_prefill_ms = engine_prefill.unwrap_or(0),
                                    prompt_tokens = pu.usage.input_tokens.unwrap_or(0).max(0) as u32,
                                    output_tokens = pu.usage.output_tokens.unwrap_or(0).max(0) as u32,
                                    tools_before = tools_since_inference,
                                    reengagement = attempt as u32,
                                    cause = if turn_stats.inference_count == 1 {
                                        "first"
                                    } else if tools_since_inference > 0 {
                                        "after_tools"
                                    } else if attempt > 0 {
                                        "reengagement"
                                    } else {
                                        "continuation"
                                    },
                                );
                                last_inference_end = Some(now);
                                tools_since_inference = 0;

                                if let Some(input) = pu.usage.input_tokens {
                                    turn_stats.prompt_tokens = input.max(0) as u32;
                                }
                                if let Some(output) = pu.usage.output_tokens {
                                    turn_stats.completion_tokens += output.max(0) as u32;
                                }
                                if let Some(stats) = &pu.stats {
                                    // Decoded vs KV-reused, so the prefill rate is honest.
                                    if let Some(reused) = stats.reused_prefix_tokens {
                                        let reused = reused as u32;
                                        turn_stats.reused_prefix_tokens =
                                            Some(turn_stats.reused_prefix_tokens.unwrap_or(0) + reused);
                                        turn_stats.prefilled_tokens += pu
                                            .usage
                                            .input_tokens
                                            .map(|i| i.max(0) as u32)
                                            .unwrap_or(0)
                                            .saturating_sub(reused);
                                    } else {
                                        // No reuse reported: the whole prompt was decoded.
                                        turn_stats.prefilled_tokens += pu
                                            .usage
                                            .input_tokens
                                            .map(|i| i.max(0) as u32)
                                            .unwrap_or(0);
                                    }
                                    if turn_stats.ttft_ms.is_none() {
                                        turn_stats.ttft_ms = stats.time_to_first_token_ms;
                                    }
                                    if let Some(load) = stats.model_load_ms {
                                        turn_stats.model_load_ms =
                                            Some(turn_stats.model_load_ms.unwrap_or(0) + load);
                                    }
                                    if let Some(prefill) = stats.prefill_ms {
                                        turn_stats.prefill_ms =
                                            Some(turn_stats.prefill_ms.unwrap_or(0) + prefill);
                                    }
                                    if let Some(elapsed) = stats.elapsed_ms {
                                        let decode =
                                            elapsed.saturating_sub(stats.prefill_ms.unwrap_or(0));
                                        turn_stats.decode_ms =
                                            Some(turn_stats.decode_ms.unwrap_or(0) + decode);
                                    }
                                    if let Some(n_ctx) = stats.effective_context_tokens {
                                        turn_stats.context_limit_tokens = Some(n_ctx as u32);
                                    }
                                    if let Some(draft) = &stats.draft {
                                        turn_stats.draft_accept_rate = Some(draft.accept_rate as f32);
                                    }
                                }
                            }
                            _ => {}
                        },
                        Err(e) => {
                            // The only place a mid-stream failure's cause is logged.
                            tracing::warn!(
                                session_id = %session_id,
                                error = %e,
                                "turn failed mid-stream; not re-engaging"
                            );
                            stream_failed = true;
                            yield Ok(AgentStreamEvent::Error { content: e.to_string() });
                        }
                    }
                }

                    // Turns can end in pure reasoning; flushed per attempt and at stream end.
                    if let Some(content) = reasoning.flush() {
                        yield Ok(AgentStreamEvent::Thinking { content });
                    }
                    // An errored attempt is never retried: the user already has the error.
                    if stream_failed {
                        break 'attempts;
                    }
                    // A guard-stopped turn is never re-engaged (don't rely on `produced_visible`).
                    if guard_tripped {
                        break 'attempts;
                    }
                    if produced_visible {
                        break 'attempts;
                    }
                    if attempt >= MAX_EMPTY_TURN_REENGAGEMENTS {
                        tracing::warn!(
                            session_id = %session_id,
                            attempts = attempt + 1,
                            "empty turn: re-engagement budget spent",
                        );
                        total_output_chars += EMPTY_TURN_EXHAUSTED_MESSAGE.len();
                        yield Ok(AgentStreamEvent::Text {
                            content: EMPTY_TURN_EXHAUSTED_MESSAGE.to_string(),
                        });
                        break 'attempts;
                    }
                    attempt += 1;
                    tracing::warn!(
                        session_id = %session_id,
                        attempt,
                        max = MAX_EMPTY_TURN_REENGAGEMENTS,
                        "empty turn: re-engaging with a steered prompt",
                    );
                    yield Ok(AgentStreamEvent::Status {
                        content: format!(
                            "No response — re-engaging ({attempt}/{MAX_EMPTY_TURN_REENGAGEMENTS})"
                        ),
                    });
                }
            // `attempt` already counts attempts beyond the first (0 on an ordinary turn).
            turn_stats.reengagements = attempt as u32;

            // chars/4 fallback only for providers that emit no Usage events (some HTTP ones).
            let usage = if saw_usage {
                turn_stats.context_used_tokens = Some(turn_stats.prompt_tokens);
                pond_core::models::ports::provider::UsageStats {
                    prompt_tokens: turn_stats.prompt_tokens,
                    completion_tokens: turn_stats.completion_tokens,
                    // Reported alongside, never deducted from the engine's measured output count.
                    reasoning_tokens: turn_stats.reasoning_tokens,
                }
            } else {
                pond_core::models::ports::provider::UsageStats {
                    prompt_tokens: (user_msg_len / 4).max(1) as u32,
                    completion_tokens: (total_output_chars / 4).max(1) as u32,
                    // Additive: thinking blocks never reach `total_output_chars`.
                    reasoning_tokens: turn_stats.reasoning_tokens,
                }
            };
            turn_stats.finalize_rates();
            if saw_usage {
                last_prompt_tokens_map
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(session_id.clone(), turn_stats.prompt_tokens);
            }
            let total_latency_ms = turn_start.elapsed().as_millis() as u64;
            tracing::info!(
                target: "giap::trace",
                kind = "turn_end",
                session_id = %session_id,
                prompt_tokens = usage.prompt_tokens,
                completion_tokens = usage.completion_tokens,
                total_latency_ms,
                ttft_ms = turn_stats.ttft_ms,
                prefill_ms = turn_stats.prefill_ms,
                decode_tok_per_sec = turn_stats.decode_tok_per_sec,
                context_used_tokens = turn_stats.context_used_tokens,
                context_limit_tokens = turn_stats.context_limit_tokens,
                inference_count = turn_stats.inference_count,
            );
            let stats = saw_usage.then_some(turn_stats);
            yield Ok(AgentStreamEvent::Done { session_id, model_role, usage: Some(usage), stats });
        };

        Ok(Box::pin(stream))
    }
}

#[async_trait]
impl AgentPort for GooseAdapter {
    /// Runs one throwaway turn through the real chat path so the KV cache holds turn 1's prefix.
    /// A fresh session id per call; reusing one would replay old warm-ups into the prompt.
    async fn prewarm(
        &self,
        voice_mode: bool,
        progress: std::sync::Arc<dyn Fn(WarmupPhase) + Send + Sync>,
    ) {
        if std::env::var("POND_DISABLE_PREWARM").as_deref() == Ok("1") {
            progress(WarmupPhase::Skipped {
                reason: "POND_DISABLE_PREWARM=1".to_string(),
            });
            return;
        }
        let settings = self.settings_repo.get().await.unwrap_or_default();
        if !matches!(settings.chat_provider.as_str(), "local" | "gguf") {
            progress(WarmupPhase::Skipped {
                reason: format!(
                    "provider '{}' keeps no local prefix cache",
                    settings.chat_provider
                ),
            });
            return;
        }

        progress(WarmupPhase::Warming);
        let started = std::time::Instant::now();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let request = AgentRequest {
            message: "Warm-up ping. Reply with only: ok".to_string(),
            session_id: format!("prewarm-{stamp}"),
            model_role: "chat".to_string(),
            images: Vec::new(),
            voice_mode,
            canvas_mode: false,
            // The static prefix is speaker-independent; one household warm-up serves everyone.
            profile_scope: ProfileScope::household(),
            profile_context: None,
            // Real turns' prefix has the full tool set; narrowing would warm one no turn uses.
            tool_group_allowlist: None,
            // No completeness check: a whole extra round-trip for a ping nobody reads.
            warmup: true,
        };

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(300), async {
            let mut stream = self.chat_stream(request).await?;
            use futures::StreamExt;
            while let Some(event) = stream.next().await {
                // Only the prefill matters; the reply is discarded.
                event?;
            }
            Ok::<(), anyhow::Error>(())
        })
        .await;

        let elapsed_ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(Ok(())) => {
                tracing::info!(
                    target: "giap::trace",
                    kind = "prefix_prewarm",
                    elapsed_ms,
                    voice_mode,
                    model = %settings.chat_model,
                    "static prefix precompiled; first turn will reuse it"
                );
                progress(WarmupPhase::Ready);
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "prefix_prewarm",
                    elapsed_ms,
                    error = %e,
                    "prefix warm-up failed; first turn pays the full prefill"
                );
                progress(WarmupPhase::Failed {
                    reason: e.to_string(),
                });
            }
            Err(_) => {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "prefix_prewarm",
                    elapsed_ms,
                    "prefix warm-up timed out; the prefill may still have landed"
                );
                progress(WarmupPhase::Failed {
                    reason: "timed out after 300s".to_string(),
                });
            }
        }
    }

    fn capabilities(&self) -> pond_core::models::domain::model_capabilities::ModelCapabilities {
        let mut caps = self
            .model_capabilities
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // Voice: thinking wastes TTS time; vision/audio inputs go unused.
        // `vision_section_applies` reads this same flag, so voice prompts never claim vision.
        if self.voice_mode.load(std::sync::atomic::Ordering::Relaxed) {
            caps.thinking = false;
            caps.vision = false;
            caps.audio_input = false;
        }
        caps
    }

    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
        let mut stream: futures::stream::BoxStream<'static, Result<AgentStreamEvent>> =
            self.chat_stream(request).await?;
        let mut full_text = String::new();
        let mut tool_call_ids = Vec::new();

        while let Some(event_result) = stream.next().await {
            match event_result? {
                AgentStreamEvent::Text { content } => {
                    full_text.push_str(&content);
                }
                AgentStreamEvent::ToolCall { id, .. } => {
                    tool_call_ids.push(id);
                }
                AgentStreamEvent::Error { content } => {
                    return Err(anyhow!(content));
                }
                _ => {}
            }
        }

        if full_text.is_empty() {
            return Err(anyhow!("Received empty response from Goose agent"));
        }

        let mut metadata = HashMap::new();
        if !tool_call_ids.is_empty() {
            metadata.insert(
                "tool_calls".to_string(),
                serde_json::to_string(&tool_call_ids).unwrap_or_default(),
            );
        }

        Ok(AgentResponse {
            text: full_text,
            metadata,
        })
    }

    /// Compacts through goose's own compaction; failures are errors, never `Ok(None)`.
    /// `manual_compact: true`: with no turn in flight, goose needn't keep the last user message.
    async fn compact_session(&self, session_id: &str) -> Result<Option<u32>> {
        let goose_sid = self.resolve_goose_session(session_id).await;

        let session = self
            .session_manager
            .get_session(&goose_sid, true)
            .await
            .map_err(|e| anyhow::anyhow!("no engine session for {session_id}: {e}"))?;
        let Some(conversation) = session.conversation else {
            return Ok(None);
        };
        if conversation.messages().is_empty() {
            return Ok(None);
        }

        let provider = self.agent.provider().await?;
        let model_config = self.agent.model_config_for_session(&goose_sid).await?;

        let result = goose::context_mgmt::compact_messages(
            provider.as_ref(),
            &model_config,
            &goose_sid,
            &conversation,
            true,
        )
        .await?;

        self.session_manager
            .replace_conversation(&goose_sid, &result.conversation)
            .await
            .map_err(|e| {
                anyhow::anyhow!("compaction produced a conversation we could not store: {e}")
            })?;

        // The KV-cached prefix is gone; noting it keeps the next cold turn attributable.
        self.note_prefix_invalidated(InvalidationReason::PromptChanged);

        let retained = u32::try_from(result.retained_context_tokens).ok();
        tracing::info!(
            target: "giap::trace",
            kind = "manual_compaction",
            session_id = %session_id,
            goose_sid = %goose_sid,
            retained_tokens = retained.unwrap_or(0),
            "compacted on request"
        );
        Ok(retained)
    }

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<AgentStreamEvent>>> {
        self.chat_stream(request).await
    }

    async fn call_tool(
        &self,
        session_id: &str,
        tool_name: &str,
        args_json: &str,
    ) -> Result<String> {
        let goose_sid = self.resolve_goose_session(session_id).await;
        let session = self
            .session_manager
            .get_session(&goose_sid, false)
            .await
            .map_err(|e| anyhow!("Failed to get session: {e}"))?;

        let arguments: serde_json::Map<String, serde_json::Value> =
            if args_json.is_empty() || args_json == "{}" {
                serde_json::Map::new()
            } else {
                serde_json::from_str(args_json).unwrap_or_default()
            };

        let tool_call = rmcp::model::CallToolRequestParams::new(tool_name.to_string())
            .with_arguments(arguments);

        let request_id = uuid::Uuid::new_v4().to_string();
        let (_req_id, dispatch_result) = self
            .agent
            .dispatch_tool_call(tool_call, request_id, None, &session)
            .await;

        match dispatch_result {
            Ok(mut tool_call_result) => {
                let tool_result = tool_call_result.result.as_mut().await;
                match tool_result {
                    Ok(call_result) => {
                        let text = call_result
                            .content
                            .iter()
                            .filter_map(|c| match c.deref() {
                                rmcp::model::RawContent::Text(t) => Some(t.text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        Ok(text)
                    }
                    Err(e) => Err(anyhow!("Tool returned error: {}", e.message)),
                }
            }
            Err(e) => Err(anyhow!("Tool dispatch failed: {}", e.message)),
        }
    }

    /// Deletes the engine session paired with a deleted GIAP session, if one was ever paired.
    /// Not via `resolve_goose_session`: it would create (and hydrate) one just to delete it.
    async fn forget_session(&self, session_id: &str) {
        let cached = self
            .goose_session_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned();

        let goose_sid = match cached {
            Some(gid) => Some(gid),
            None => match &self.giap_session_storage {
                Some(storage) => storage
                    .get_engine_session_id(session_id)
                    .await
                    .ok()
                    .flatten(),
                None => None,
            },
        };

        // Unconditionally, before the engine delete: this GIAP id must never resolve again.
        self.goose_session_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);

        let Some(goose_sid) = goose_sid else {
            return;
        };

        if let Err(e) = self.session_manager.delete_session(&goose_sid).await {
            // Best-effort: an orphaned engine session beats failing the user's delete.
            tracing::warn!(
                "Failed to delete Goose engine session '{goose_sid}' for GIAP session \
                 '{session_id}': {e}"
            );
        }
    }

    /// Returns a snapshot: handing out the lock would let compaction straddle prompt assembly.
    fn prefix_cache_state(&self) -> Option<PrefixCacheState> {
        Some(*self.prefix_cache.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

// ── Driving a child agent ───────────────────────────────────────────────────
//
// Mechanism only: every policy decision is already made in `TaskSpec` / `ChildPlan`.
// Lives here, not in `orchestrator.rs`, because it needs this type's private fields.
impl GooseAdapter {
    /// The parent's live engine surface, for [`crate::orchestrator::build_child_plan`].
    /// Tools come from the parent's own session (not the catalog), so a child's are a subset.
    pub(crate) async fn child_environment(
        &self,
        parent_session_id: &str,
    ) -> Result<crate::orchestrator::ChildEnvironment> {
        let settings = self.settings_repo.get().await?;
        let template_content = self
            .template_repo
            .get(&settings.prompt_style)
            .await
            .ok()
            .flatten()
            .map(|t| t.content)
            .unwrap_or_else(|| fallback_prompt().to_string());

        // Lean on purpose: compact (a child gets a window fraction), no prose tool list (the
        // envelope names them), no thinking (the drain's `as_concat_text()` drops it).
        let prompt_state = PromptState {
            current_date: String::new(),
            current_time: String::new(),
            voice_mode: false,
            available_tools: Vec::new(),
            thinking_enabled: false,
            compact_prompt: true,
            native_tools_json: matches!(
                settings.chat_provider.as_str(),
                "local" | "gguf" | "mistralrs"
            ),
            // Its envelope names its tools, so it has some whenever the pond does.
            tools_offered: !registered_extensions().is_empty(),
            prefix_hash: None,
        };

        // Same partition as the parent's turn, so a child speaks as this assistant, not Goose's
        // stock subagent. Static prefix only: the suffix is date/time/profile lines.
        let partition = build_prompt_partition(&settings, None, &prompt_state, &template_content);

        let goose_sid = self.resolve_goose_session(parent_session_id).await;
        let mut parent_tools: std::collections::BTreeMap<
            String,
            std::collections::BTreeSet<String>,
        > = std::collections::BTreeMap::new();
        for tool in self.agent.list_tools(&goose_sid, None).await {
            let name = tool.name.to_string();
            // Bare names: Goose matches `available_tools` against the unprefixed tool name.
            let Some((extension, bare)) = crate::orchestrator::split_extension_tool(&name) else {
                continue;
            };
            parent_tools
                .entry(extension.to_string())
                .or_default()
                .insert(bare.to_string());
        }

        Ok(crate::orchestrator::ChildEnvironment {
            provider_name: settings.chat_provider.clone(),
            base_system_prefix: partition.static_prefix,
            parent_tools,
        })
    }

    /// The row must exist before the plan runs: `Agent::update_provider` errors without it.
    pub(crate) async fn open_child_session(&self, role: &str) -> Result<String> {
        let session = self
            .session_manager
            .create_session(
                std::env::current_dir().unwrap_or_default(),
                format!("giap-subagent:{role}"),
                goose::session::session_manager::SessionType::SubAgent,
                GooseMode::Auto,
            )
            .await
            .map_err(|e| anyhow!("Failed to create subagent session for role '{role}': {e}"))?;
        Ok(session.id)
    }

    /// Forgets the shim entry too: stale child entries would evict a live parent's allow-set.
    pub(crate) async fn release_child_session(&self, child_session_id: &str) {
        self.shim_controls.forget_session(child_session_id);
        if let Err(e) = self.session_manager.delete_session(child_session_id).await {
            tracing::warn!("Failed to release subagent engine session '{child_session_id}': {e}");
        }
    }

    /// Runs one child until done or `cancel` trips. Replaces Goose's `get_agent_messages`.
    pub(crate) async fn run_child_agent(
        &self,
        plan: crate::orchestrator::ChildPlan,
        cancel: CancellationToken,
    ) -> Result<crate::orchestrator::ChildOutcome> {
        let (provider, model_config) = self
            .current_provider
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| {
                anyhow!(
                    "no provider is configured on this pond yet - a subagent cannot be given one"
                )
            })?;

        // The child gets its own `ModelConfig` on the same provider; the parent's is untouched.
        let model_config = crate::orchestrator::child_model_config(&plan.model, model_config);

        // ── The child's second tool layer, and its prompt ─────────────────────
        //
        // Before the child's first provider call: the shim is a no-op for a session with no entry.
        // The override stops `enforce_system` silently rebuilding away the delegation envelope.
        let child_controls = self.shim_controls.session(&plan.child_session_id);
        child_controls.set_allowed_tools(plan.allowed_tool_names.iter().cloned().collect());
        child_controls.set_system_override(Some(plan.system_prompt.clone()));

        // Fresh agent: Goose loads no default extensions, so the child has only the plan's tools.
        // `GooseMode::Auto` is mandatory: approval modes hang on the child's `confirmation_rx`, so
        // its tool set is its only safety boundary. No scheduler: no `manage_schedule_tool`.
        let config = AgentConfig::new(
            self.session_manager.clone(),
            goose::config::permission::PermissionManager::instance(),
            None,
            GooseMode::Auto,
            true,
            GoosePlatform::GooseCli,
        );
        let child = GooseAgent::with_config(config);

        child
            .update_provider(provider, model_config, &plan.child_session_id)
            .await
            .map_err(|e| anyhow!("Failed to set the provider on the subagent: {e}"))?;

        // Fail loudly: Goose's own loop swallows a failed extension and runs the child anyway.
        for extension in &plan.extensions {
            child
                .add_extension(extension.clone(), &plan.child_session_id)
                .await
                .map_err(|e| {
                    anyhow!(
                        "Failed to load extension '{}' for subagent role '{}': {e}",
                        extension.name(),
                        plan.role
                    )
                })?;
        }

        child
            .override_system_prompt(plan.system_prompt.clone())
            .await;

        // Unioned with a post-run read, which alone would miss an extension removed mid-run.
        let mut loaded_extensions: std::collections::BTreeSet<String> = child
            .list_extensions()
            .await
            .into_iter()
            .map(|e| e.to_string())
            .collect();

        let session_config = goose::agents::SessionConfig {
            id: plan.child_session_id.clone(),
            schedule_id: None,
            // Always bounded: an unbounded on-device child blocks the parent's turn for minutes.
            max_turns: Some(plan.max_turns),
            retry_config: None,
        };
        let user_message = Message::user().with_text(&plan.user_message);

        // The child overwrites the one retained KV prefix; without this the parent's next turn
        // records it warm. Before `reply`: a stream that fails part-way has still prefilled.
        self.note_prefix_invalidated(InvalidationReason::PromptChanged);

        let mut stream =
            goose::session_context::with_session_id(Some(plan.child_session_id.clone()), async {
                child
                    .reply(user_message.clone(), session_config, Some(cancel.clone()))
                    .await
            })
            .await
            .map_err(|e| anyhow!("Failed to start the subagent reply: {e}"))?;

        // A `Message` event is a fragment, not a turn; `ChildTurns` owns the assembly rule.
        let mut turns = crate::orchestrator::ChildTurns::default();
        while let Some(event) = stream.next().await {
            match event {
                Ok(goose::agents::AgentEvent::Message(msg)) => {
                    // `as_concat_text()` drops `Thinking`: the child path's only reasoning gate.
                    crate::orchestrator::child_stream_step(
                        &mut turns,
                        msg.role == rmcp::model::Role::Assistant,
                        &msg.as_concat_text(),
                    );
                    // Never read `msg.content` here; it bypasses the reasoning gate.
                    for tool in crate::orchestrator::child_tool_names(&msg) {
                        crate::orchestrator::report_child_progress(
                            &plan.parent_session_id,
                            &plan.task_id,
                            &plan.role,
                            pond_core::shared::domain::agent::SubagentStatus::Tool,
                            Some(tool),
                        );
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        role = %plan.role,
                        "subagent stream error, ending the run: {e}"
                    );
                    break;
                }
            }
        }
        drop(stream);
        let (last_text, assistant_turns) = turns.finish();

        // Post-run read: an extension can arrive without `add_extension` (e.g. `default_enabled`).
        loaded_extensions.extend(
            child
                .list_extensions()
                .await
                .into_iter()
                .map(|e| e.to_string()),
        );

        Ok(crate::orchestrator::ChildOutcome {
            last_text,
            assistant_turns,
            loaded_extensions,
        })
    }
}

/// The `.gguf` file for a model name that may lack its quant suffix (a catalog display name).
/// Falls back to the naive `{name}.gguf` so the caller's file-not-found warning still fires.
pub(crate) fn resolve_gguf_filename(model_name: &str, gguf_dir: &std::path::Path) -> String {
    if model_name.ends_with(".gguf") {
        return model_name.to_string();
    }

    let exact = format!("{model_name}.gguf");
    if gguf_dir.join(&exact).exists() {
        return exact;
    }

    if let Ok(entries) = std::fs::read_dir(gguf_dir) {
        let mut variants: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|f| f.ends_with(".gguf"))
            .filter(|f| {
                let base = f.trim_end_matches(".gguf");
                // Require a real quant tag: "gemma-4-E2B" must not match "gemma-4-E2B-it-Q4_K_M".
                base.strip_prefix(model_name)
                    .and_then(|rest| rest.strip_prefix(['-', '.']))
                    .is_some_and(looks_like_quant_tag)
            })
            .collect();
        variants.sort();
        if let Some(filename) = variants.into_iter().next() {
            return filename;
        }
    }

    exact
}

/// Images ride the user message: the system prefix must stay byte-identical for KV reuse.
fn attach_images(
    msg: Message,
    images: &[pond_core::models::domain::message::ImageAttachment],
) -> Message {
    images
        .iter()
        .fold(msg, |m, img| m.with_image(&img.data, &img.mime_type))
}

fn image_part_count(msg: &Message) -> usize {
    msg.content
        .iter()
        .filter(|c| matches!(c, goose::conversation::message::MessageContent::Image(_)))
        .count()
}

/// True for messages the image cap must not touch: providers reject re-keyed tool responses.
fn has_tool_parts(msg: &Message) -> bool {
    use goose::conversation::message::MessageContent as C;
    msg.content.iter().any(|c| {
        matches!(
            c,
            C::ToolRequest(_)
                | C::ToolResponse(_)
                | C::ToolConfirmationRequest(_)
                // Pending elicitations/confirmations: same bookkeeping, just as unsafe to rewrite.
                | C::ActionRequired(_)
                | C::FrontendToolRequest(_)
        )
    })
}

/// Keeps the leading `keep` images and swaps the text for `text`, with exactly one placeholder.
/// Capping is staged (2 to 1, then 1 to 0): old placeholders are stripped, never stacked.
fn cap_message_images(original: &Message, keep: usize, text: &str) -> Message {
    use goose::conversation::message::MessageContent as C;
    use pond_core::models::services::context::image_history::{
        contains_history_image_placeholder, history_image_placeholder,
        strip_history_image_placeholders,
    };

    // An earlier pass's placeholder means one is still owed even if this pass drops nothing.
    let placeholder_owed = contains_history_image_placeholder(text)
        || original.content.iter().any(|c| match c {
            C::Text(t) => contains_history_image_placeholder(&t.text),
            _ => false,
        });
    let wanted_text = strip_history_image_placeholders(text);
    // Forced by `placeholder_owed` so the old placeholder goes even if the texts compare equal.
    let text_changed = placeholder_owed || original.as_concat_text() != wanted_text;
    let mut kept = 0usize;
    let mut dropped = 0usize;
    let mut text_emitted = false;
    let mut content: Vec<C> = Vec::with_capacity(original.content.len() + 1);

    for part in &original.content {
        match part {
            C::Image(_) => {
                if kept < keep {
                    kept += 1;
                    content.push(part.clone());
                } else {
                    dropped += 1;
                }
            }
            C::Text(_) if text_changed => {
                if !text_emitted {
                    text_emitted = true;
                    // Some providers reject empty text parts.
                    if !wanted_text.is_empty() {
                        content.push(C::text(wanted_text.as_ref()));
                    }
                }
            }
            other => content.push(other.clone()),
        }
    }

    if dropped > 0 || placeholder_owed {
        content.push(C::text(history_image_placeholder(kept)));
    }

    let mut capped = original.clone();
    capped.content = content;
    capped
}

/// Returns `(had, keep, dropped_total)`, aligned with `source`; tool messages count as zero.
/// `dropped_total == 0` means the conversation already fits and must stay byte-identical.
fn plan_image_cap(source: &[Message]) -> (Vec<usize>, Vec<usize>, usize) {
    use pond_core::models::services::context::image_history::{
        dropped_image_count, plan_history_images,
    };

    let had: Vec<usize> = source
        .iter()
        .map(|m| {
            if has_tool_parts(m) {
                0
            } else {
                image_part_count(m)
            }
        })
        .collect();
    let keep = plan_history_images(&had);
    let dropped = dropped_image_count(&had, &keep);
    (had, keep, dropped)
}

/// Whether `tag` starts with a GGUF quant marker; only needs to tell one from `it`/`instruct`.
pub(crate) fn looks_like_quant_tag(tag: &str) -> bool {
    // Unsloth dynamic quants are spelled `UD-Q4_K_XL`; `UD-` marks the quant, not the model.
    let tag = tag.strip_prefix("UD-").unwrap_or(tag);
    let digit_after = |prefix: &str| {
        tag.strip_prefix(prefix)
            .and_then(|r| r.chars().next())
            .is_some_and(|c| c.is_ascii_digit())
    };
    tag.starts_with("F16")
        || tag.starts_with("F32")
        || tag.starts_with("BF16")
        || digit_after("IQ")
        || digit_after("Q")
}

/// Drops a quant suffix only when the stem resolves to the same file, so one GGUF gets one id.
fn canonical_model_stem(model_name: &str, gguf_dir: &std::path::Path) -> String {
    let stem = model_name.trim_end_matches(".gguf");
    let resolved_stem = resolve_gguf_filename(stem, gguf_dir);
    // Shortest base first, not the last separator: quant tags can be compound (`UD-Q4_K_XL`).
    for (i, _) in stem.match_indices(['-', '.']) {
        let (base, tail) = stem.split_at(i);
        // `tail` starts with the ASCII separator that matched.
        let tag = &tail[1..];
        if base.is_empty() || !looks_like_quant_tag(tag) {
            continue;
        }
        if resolve_gguf_filename(base, gguf_dir) == resolved_stem {
            return base.to_string();
        }
    }
    stem.to_string()
}

/// Driven by the always-on `giap-toolkit` extension, so it's reachable in any narrowed session.
#[async_trait]
impl pond_core::mcp::ports::tools::tool_selection_control::ToolSelectionControl for GooseAdapter {
    async fn group_status(
        &self,
        engine_session_id: &str,
    ) -> Vec<pond_core::mcp::ports::tools::tool_selection_control::ToolGroupStatus> {
        use pond_core::mcp::domain::tool_group::{find_group, group_of_tool};
        use pond_core::mcp::ports::tools::tool_selection_control::ToolGroupStatus;

        // The caller can only know goose's session; the maps are keyed by GIAP's.
        let session_id = match self.giap_session_for_engine(engine_session_id) {
            Some(s) => s,
            None => String::new(),
        };
        let session_id = session_id.as_str();
        let settings = self.settings_repo.get().await.unwrap_or_default();
        let loaded: Option<Vec<String>> = if settings.tool_selection_narrows() {
            self.session_tool_groups
                .read()
                .await
                .get(session_id)
                .cloned()
        } else {
            None
        };

        // From the live cache only; a cold cache reports 0 rather than a guess.
        let mut counts: HashMap<String, usize> = HashMap::new();
        if let Some(cache) = self.cached_tools.read().await.as_ref() {
            for tool in cache {
                if let Some(ext) = group_of_tool(tool) {
                    *counts.entry(ext.to_string()).or_insert(0) += 1;
                }
            }
        }

        registered_extensions()
            .iter()
            .filter_map(|extension| {
                let group = find_group(extension)?;
                Some(ToolGroupStatus {
                    extension: extension.clone(),
                    description: group.description.to_string(),
                    loaded: match &loaded {
                        Some(groups) => groups.iter().any(|g| g == extension),
                        None => true,
                    },
                    core: group.core,
                    tool_count: counts.get(extension).copied().unwrap_or(0),
                })
            })
            .collect()
    }

    async fn enable_group(
        &self,
        engine_session_id: &str,
        group: &str,
    ) -> Result<Vec<String>, pond_core::mcp::ports::tools::tool_selection_control::ToolSelectionError>
    {
        use pond_core::mcp::domain::tool_group::is_catalog_extension;
        use pond_core::mcp::ports::tools::tool_selection_control::ToolSelectionError;

        // An unattributed call must not widen ANY session.
        let Some(session_id) = self.giap_session_for_engine(engine_session_id) else {
            return Err(ToolSelectionError::NotActive);
        };
        let session_id = session_id.as_str();

        let group = group.trim();
        if !is_catalog_extension(group) {
            return Err(ToolSelectionError::UnknownGroup(group.to_string()));
        }
        if !registered_extensions().iter().any(|e| e == group) {
            return Err(ToolSelectionError::GroupNotRegistered(group.to_string()));
        }

        // Guests can call `giap-toolkit` too, so the session's group boundary is enforced here.
        // Refused as `GroupNotRegistered` so the caller can't learn the group exists.
        if let Some(permitted) = self.session_permitted_groups.read().await.get(session_id) {
            if !permitted.iter().any(|p| p == group) {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "tool_group_widen_refused",
                    session_id = %session_id,
                    group,
                    "enable_tool_group named a group outside this session's boundary"
                );
                return Err(ToolSelectionError::GroupNotRegistered(group.to_string()));
            }
        }

        let groups = {
            let mut map = self.session_tool_groups.write().await;
            // No entry: selection never ran (mode "all" or a non-chat path), so nothing to widen.
            let Some(entry) = map.get_mut(session_id) else {
                return Err(ToolSelectionError::NotActive);
            };
            if !entry.iter().any(|g| g == group) {
                entry.push(group.to_string());
                entry.sort();
            }
            entry.clone()
        };

        if let Some(storage) = &self.giap_session_storage {
            if let Err(e) = storage.set_session_tool_groups(session_id, &groups).await {
                // Non-fatal: the widen holds for this run either way.
                tracing::warn!("tool selection: persisting the widened groups failed: {e}");
            }
        }

        // Takes effect on the next provider call, even mid-turn: the shim and guard read it live.
        let goose_sid = self
            .goose_session_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned();
        if let Some(goose_sid) = goose_sid {
            let newly_allowed: Vec<String> = match self.cached_tools.read().await.as_ref() {
                Some(cache) => cache
                    .iter()
                    .filter(|t| pond_core::mcp::domain::tool_group::group_of_tool(t) == Some(group))
                    .cloned()
                    .collect(),
                None => Vec::new(),
            };
            if newly_allowed.is_empty() {
                // Cold cache: the widen can't apply this turn, and the model would waste a turn
                // calling a suppressed tool. `NotReady` is a tool success that explains why.
                tracing::warn!(
                    target: "giap::trace",
                    kind = "tool_group_enable_deferred",
                    session_id = %session_id,
                    group,
                    "enabled a group while the tool cache was cold"
                );
                return Err(ToolSelectionError::NotReady(group.to_string()));
            }
            self.shim_controls
                .session(&goose_sid)
                .extend_allowed_tools(newly_allowed);
        }

        Ok(groups)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── thinking section stability ────────────────────────────────────────

    /// Turns 1 and 2 must agree in "auto", or every session re-prefills on its second turn.
    #[test]
    fn auto_thinking_is_decided_without_a_cache_that_fills_in_later() {
        assert!(GooseAdapter::thinking_section_applies(
            "auto",
            "ollama",
            "gemma-4-E2B-it",
            None,
            false
        ));
        assert!(!GooseAdapter::thinking_section_applies(
            "auto",
            "ollama",
            "llama-3.2-3b",
            None,
            false
        ));
    }

    #[test]
    fn auto_thinking_gives_the_same_answer_twice() {
        for model in ["gemma-4-E2B-it", "llama-3.2-3b", "NVIDIA-Nemotron3-Nano-4B"] {
            let first = GooseAdapter::thinking_section_applies("auto", "local", model, None, false);
            let second =
                GooseAdapter::thinking_section_applies("auto", "local", model, None, false);
            assert_eq!(first, second, "{model} answered differently on turn 2");
        }
    }

    // ── Which reason a provider swap records ──────────────────────────────

    #[test]
    fn the_same_model_behind_a_new_provider_is_a_rebuild_not_a_swap() {
        assert_eq!(
            GooseAdapter::provider_change_reason("ollama:gemma4:e2b", "gemma4:e2b"),
            InvalidationReason::ProviderRebuilt
        );
        assert_eq!(
            GooseAdapter::provider_change_reason("local:gemma-4-E2B-it", "gemma-4-E2B-it"),
            InvalidationReason::ProviderRebuilt
        );
    }

    /// Every Ollama tag has a colon, so the provider key must be split at the first one.
    #[test]
    fn a_model_name_containing_a_colon_survives_the_key_split() {
        assert_eq!(
            GooseAdapter::provider_change_reason("ollama:gemma4:e2b", "gemma4:e4b"),
            InvalidationReason::ModelSwapped
        );
        assert_eq!(
            GooseAdapter::provider_change_reason("llamafile:gemma4:e2b", "gemma4:e2b"),
            InvalidationReason::ProviderRebuilt
        );
    }

    #[test]
    fn a_different_model_is_a_swap_and_so_is_the_very_first_provider() {
        assert_eq!(
            GooseAdapter::provider_change_reason("local:gemma-4-E2B-it", "gemma-4-E4B-it"),
            InvalidationReason::ModelSwapped
        );
        // Startup: `last_provider_key` is empty, so there is no previous model.
        assert_eq!(
            GooseAdapter::provider_change_reason("", "gemma-4-E2B-it"),
            InvalidationReason::ModelSwapped
        );
    }

    #[test]
    fn explicit_thinking_modes_ignore_the_model_and_voice_always_wins() {
        assert!(GooseAdapter::thinking_section_applies(
            "on",
            "ollama",
            "llama-3.2-3b",
            None,
            false
        ));
        assert!(!GooseAdapter::thinking_section_applies(
            "off",
            "ollama",
            "gemma-4-E2B-it",
            None,
            false
        ));
        for mode in ["on", "off", "auto"] {
            assert!(
                !GooseAdapter::thinking_section_applies(
                    mode,
                    "ollama",
                    "gemma-4-E2B-it",
                    None,
                    true
                ),
                "voice mode must suppress <thinking> regardless of mode ({mode})"
            );
        }
    }

    // ── The structured reasoning channel ──────────────────────────────────

    /// The producer is the only gate: downstream forwards `Thinking` frames unconditionally.
    #[test]
    fn a_voice_turn_never_surfaces_reasoning() {
        for show_thinking in [true, false] {
            assert!(
                !GooseAdapter::reasoning_frames_enabled(show_thinking, true),
                "voice mode leaked reasoning with show_thinking={show_thinking}"
            );
        }
        assert!(GooseAdapter::reasoning_frames_enabled(true, false));
        assert!(
            !GooseAdapter::reasoning_frames_enabled(false, false),
            "a user who turned thinking off must not receive reasoning frames"
        );
    }

    /// Serve mode hardcodes the instance flag to false, so the request flag is the only defence.
    #[test]
    fn a_request_flagged_voice_is_a_voice_turn_even_on_a_text_started_process() {
        assert!(
            GooseAdapter::voice_turn(false, true),
            "the shipped desktop hardcodes the instance flag to false, so the per-request \
             flag is the ONLY signal that a turn is spoken. Dropping it re-opens the leak \
             P1 closed, and every gate downstream keeps looking correct."
        );
        assert!(
            GooseAdapter::voice_turn(true, false),
            "the CLI `--voice` instance flag must still count on its own"
        );
        assert!(GooseAdapter::voice_turn(true, true));
        assert!(
            !GooseAdapter::voice_turn(false, false),
            "a text turn on a text process must not be treated as voice, or reasoning \
             is suppressed for everyone"
        );
    }

    /// A failed settings read falls back to `Settings::default()`, which must narrow.
    #[test]
    fn the_settings_default_emits_no_reasoning() {
        let fallback = pond_core::user_data::domain::settings::Settings::default();
        assert!(!GooseAdapter::reasoning_frames_enabled(
            fallback.show_thinking,
            false
        ));
    }

    /// `as_concat_text()` drops `Thinking`, so reasoning needs its own lift.
    #[test]
    fn reasoning_is_lifted_out_of_a_message_that_also_carries_an_answer() {
        let msg = Message::assistant()
            .with_thinking("  the user asked about the porch light  ", "")
            .with_text("The porch light is on.");

        assert_eq!(
            msg.as_concat_text(),
            "The porch light is on.",
            "as_concat_text is still the answer-only view; that is the whole reason \
             a separate lift is needed"
        );
        // Raw lift: the coalescer trims once per passage, not per fragment.
        assert_eq!(
            GooseAdapter::reasoning_frames(&msg, true),
            vec!["  the user asked about the porch light  ".to_string()],
        );
        assert!(
            GooseAdapter::reasoning_frames(&msg, false).is_empty(),
            "the gate is applied inside the lift, not only at the call site"
        );
        // And the frame a user actually sees is the trimmed passage.
        let mut coalescer = ReasoningCoalescer::default();
        coalescer.push(&msg, true);
        assert_eq!(
            coalescer.flush(),
            Some("the user asked about the porch light".to_string()),
        );
    }

    /// Local gguf emits one `Message` per token piece; the UI renders one `<p>` per frame.
    #[test]
    fn a_reasoning_passage_arrives_as_one_frame_with_its_spacing_intact() {
        let mut coalescer = ReasoningCoalescer::default();
        let mut frames: Vec<String> = Vec::new();
        for fragment in [" the user", " asked about", " the light"] {
            let msg = Message::assistant().with_thinking(fragment, "");
            coalescer.push(&msg, true);
            assert!(
                !GooseAdapter::message_ends_reasoning(&msg),
                "a message carrying only reasoning must not end the passage, or \
                 every delta flushes and nothing was coalesced"
            );
        }
        if let Some(content) = coalescer.flush() {
            frames.push(content);
        }

        assert_eq!(
            frames.len(),
            1,
            "a single reasoning passage produced {} frames; the consumer renders \
             one <p> per frame, so this is the per-token confetti P1 shipped. \
             Frames: {:?}",
            frames.len(),
            frames
        );
        assert_eq!(
            frames[0], "the user asked about the light",
            "the passage lost its inter-fragment whitespace. A per-fragment trim \
             joins the deltas as \"the userasked aboutthe light\"; the trim must \
             happen ONCE, on the assembled passage."
        );
    }

    /// On `anthropic.rs` and non-streaming paths one message is a whole block; don't fuse two.
    #[test]
    fn a_whole_block_provider_is_not_merged_into_one_giant_block() {
        const FIRST: &str = "The user wants the porch light. I should check the registry.";
        const SECOND: &str = "The registry says it exists and is off. I can turn it on.";

        let mut coalescer = ReasoningCoalescer::default();
        let mut frames: Vec<String> = Vec::new();

        // Mirrors the stream's own push-then-flush sequence.
        fn feed(msg: Message, coalescer: &mut ReasoningCoalescer, frames: &mut Vec<String>) {
            coalescer.push(&msg, true);
            if GooseAdapter::message_ends_reasoning(&msg) {
                if let Some(content) = coalescer.flush() {
                    frames.push(content);
                }
            }
        }

        feed(
            Message::assistant().with_thinking(FIRST, ""),
            &mut coalescer,
            &mut frames,
        );
        // Something visible: the answer text that closes the first block.
        feed(
            Message::assistant().with_text("Checking the registry."),
            &mut coalescer,
            &mut frames,
        );
        feed(
            Message::assistant().with_thinking(SECOND, ""),
            &mut coalescer,
            &mut frames,
        );
        if let Some(content) = coalescer.flush() {
            frames.push(content);
        }

        assert_eq!(
            frames.len(),
            2,
            "two complete provider blocks came out as {} frame(s). Merging them \
             fuses reasoning the model emitted separately. Frames: {:?}",
            frames.len(),
            frames
        );
        assert_eq!(frames[0], FIRST);
        assert_eq!(frames[1], SECOND);
    }

    /// A turn can end in pure reasoning (gemma-4-E2B does), so the last passage must flush.
    #[test]
    fn a_block_that_ends_the_turn_is_not_dropped() {
        let mut coalescer = ReasoningCoalescer::default();
        for fragment in ["I should", " check the", " device registry."] {
            let msg = Message::assistant().with_thinking(fragment, "");
            coalescer.push(&msg, true);
            assert!(!GooseAdapter::message_ends_reasoning(&msg));
        }
        assert_eq!(
            coalescer.flush(),
            Some("I should check the device registry.".to_string()),
            "the turn ended in pure reasoning and the buffered passage was lost. \
             The stream needs a flush AFTER the goose event loop drains, not only \
             inside it."
        );
    }

    /// With the gate shut nothing is buffered, so no later flush can emit it.
    #[test]
    fn the_display_gate_still_owns_the_buffer() {
        let mut coalescer = ReasoningCoalescer::default();
        for fragment in [" the user", " asked about", " the light"] {
            coalescer.push(&Message::assistant().with_thinking(fragment, ""), false);
        }
        assert_eq!(
            coalescer.flush(),
            None,
            "reasoning was buffered with the display gate shut; on a voice turn \
             that is unspeakable text one flush away from the speaker"
        );

        let fallback = pond_core::user_data::domain::settings::Settings::default();
        let emit = GooseAdapter::reasoning_frames_enabled(fallback.show_thinking, false);
        let mut coalescer = ReasoningCoalescer::default();
        coalescer.push(
            &Message::assistant().with_thinking("something private", ""),
            emit,
        );
        assert_eq!(
            coalescer.flush(),
            None,
            "the settings-read fallback emitted reasoning; a scope-widening default \
             is a bug"
        );
    }

    /// `RedactedThinking` is provider ciphertext, and blank blocks render as a flicker.
    /// Asserted at the flush: the lift keeps a lone `" "`, which is the space between two words.
    #[test]
    fn ciphertext_and_blank_reasoning_never_reach_the_stream() {
        let msg = Message::assistant()
            .with_redacted_thinking("ZW5jcnlwdGVkLXJlYXNvbmluZw==")
            .with_thinking("   ", "")
            .with_thinking("\n\t", "");

        assert!(
            !GooseAdapter::reasoning_frames(&msg, true)
                .iter()
                .any(|f| f.contains("ZW5jcnlwdGVk")),
            "provider ciphertext entered the reasoning channel; RedactedThinking \
             must be dropped in the lift, not merely trimmed later"
        );

        let mut coalescer = ReasoningCoalescer::default();
        coalescer.push(&msg, true);
        assert_eq!(
            coalescer.flush(),
            None,
            "redacted or blank reasoning produced a frame"
        );
    }

    /// Production source lines with `//` comments cut, so prose can't satisfy a guard.
    fn stream_body_code() -> Vec<String> {
        let src = include_str!("goose_agent.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        body.lines()
            .map(|l| match l.find("//") {
                Some(i) => l[..i].to_string(),
                None => l.to_string(),
            })
            .collect()
    }

    /// Skipping it is safe: the nudge is a later message, so the first request is byte-identical.
    /// That is guarded by `pond-mcp-server`'s prefix oracle, not by this test.
    #[test]
    fn the_prefix_warm_up_does_not_arm_the_completeness_check() {
        let lines = stream_body_code();

        let built = lines
            .iter()
            .position(|l| l.contains("prewarm-{stamp}"))
            .expect(
                "the warm-up no longer builds a `prewarm-` session id. If it was renamed, this \
                 test needs the new anchor -- it is the only thing pinning that the warm-up \
                 declares itself.",
            );

        // A window, not a fixed offset: the request literal carries several fields and comments.
        let window = lines[built..(built + 30).min(lines.len())].join("\n");

        assert!(
            window.contains("warmup: true"),
            "the warm-up request does not set `warmup: true`, so it arms the completeness check \
             and pays a second full round-trip for a reply nobody reads. Window:\n{window}"
        );
    }

    /// Inside `<system-context>` it is stripped from past turns; outside, each turn keeps a copy.
    #[test]
    fn the_answer_contract_rides_inside_the_system_context_envelope() {
        let lines = stream_body_code();

        let open = lines
            .iter()
            .position(|l| l.contains("push_str(\"<system-context>"))
            .expect("the <system-context> envelope is no longer opened here");
        let close = lines
            .iter()
            .skip(open)
            .position(|l| l.contains("push_str(\"</system-context>"))
            .map(|i| i + open)
            .expect("the <system-context> envelope is no longer closed here");

        let inside = lines[open..close].join("\n");
        assert!(
            inside.contains("answer_contract()"),
            "the answer contract is not inside the <system-context> envelope. Outside \
             it, the trimmer does not strip it from prior turns and every historical \
             turn keeps a copy. Envelope:\n{inside}"
        );

        // After the budget note, so the reply's shape is the last thing before the request.
        let budget_at = inside
            .find("turn_budget_block")
            .expect("the turn-budget note left the envelope");
        let contract_at = inside
            .find("answer_contract()")
            .expect("checked immediately above");
        assert!(
            contract_at > budget_at,
            "the answer contract is emitted before the turn budget. It is the closest \
             instruction to where the answer gets written, which is the only reason it \
             is restated here at all."
        );
    }

    /// The lift and its gate can both be right while the stream yields `Thinking` elsewhere.
    #[test]
    fn every_thinking_frame_leaves_through_the_gate() {
        let src = include_str!("goose_agent.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        let code = stream_body_code();

        // Not a count: a new raw yield would pass a bumped number; each yield must follow a flush.
        let yields: Vec<usize> = code
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains("yield Ok(AgentStreamEvent::Thinking"))
            .map(|(i, _)| i)
            .collect();
        assert!(
            !yields.is_empty(),
            "nothing yields a Thinking frame any more; PAI-5 P1's structured \
             reasoning channel has been removed"
        );
        for i in &yields {
            let window = &code[i.saturating_sub(3)..*i];
            assert!(
                window.iter().any(|l| l.contains("reasoning.flush()")),
                "the Thinking frame yielded at line {} does not come out of \
                 ReasoningCoalescer::flush. The coalescer is where the once-per-\
                 passage trim and the buffered display gate live, so a raw yield \
                 here re-opens both the per-token confetti and the path by which \
                 ungated reasoning reaches a voice session.",
                i + 1
            );
        }
        assert_eq!(
            code.iter()
                .filter(|l| l.contains("reasoning.push(&msg, emit_reasoning)"))
                .count(),
            1,
            "the gated push into the reasoning coalescer must appear exactly once. \
             A second, ungated push would fill the buffer on a voice turn and the \
             next flush would emit it."
        );
        assert!(
            body.contains("Self::reasoning_frames_enabled(settings.show_thinking, is_voice)"),
            "emit_reasoning is no longer bound from show_thinking AND the voice flag"
        );
        // Pin the composition too: the identifier `is_voice` says nothing about what it holds.
        assert!(
            body.contains("let is_voice = Self::voice_turn(voice_instance, request.voice_mode);"),
            "is_voice is no longer composed by voice_turn(instance, request). If the \
             per-request flag was dropped, reasoning leaks to every desktop voice turn: \
             the serve-mode adapter hardcodes the instance flag to false."
        );
    }

    /// Anchored on the loop's closing brace, since the in-loop flush also follows its opening.
    #[test]
    fn the_last_reasoning_block_of_a_turn_is_flushed_after_the_event_loop() {
        let code = stream_body_code();

        let loop_start = code
            .iter()
            .position(|l| l.contains("'engine: loop {"))
            .expect(
                "the goose event loop is gone. PAI-6 P6 turned it from a `while let` \
                 over `goose_stream.next()` into a labelled `loop` that selects the \
                 engine against the subagent progress channel; the label is what this \
                 guard anchors on, because a bare `loop {` matches the `'attempts` \
                 loop above it",
            );
        let indent = |l: &String| l.len() - l.trim_start().len();
        let loop_indent = indent(&code[loop_start]);
        let loop_end = (loop_start + 1..code.len())
            .find(|&i| code[i].trim() == "}" && indent(&code[i]) == loop_indent)
            .expect("could not find the end of the goose event loop");
        let recovery = code
            .iter()
            .position(|l| l.contains("if produced_visible {"))
            .expect("the empty-turn recovery check is gone");
        assert!(
            loop_end < recovery,
            "the event loop no longer closes before the empty-turn recovery check; \
             this guard's anchors have rotted and must be re-derived"
        );

        assert!(
            code[loop_end + 1..recovery]
                .iter()
                .any(|l| l.contains("reasoning.flush()")),
            "there is no reasoning.flush() between the end of the goose event loop \
             (line {}) and the empty-turn recovery check (line {}). Without it the \
             LAST reasoning passage of the turn is buffered and never emitted — and \
             a turn ending in pure reasoning is real, not hypothetical: it is the \
             case the re-engagement loop directly below exists to handle. A \
             coalescer that drops the final block is worse than the per-token \
             frames it replaced.",
            loop_end + 1,
            recovery + 1
        );
    }

    // ── Reasoning tokens ──────────────────────────────────────────────────

    /// The shipped default hides thinking, and the output reserve is sized from this count.
    #[test]
    fn reasoning_is_counted_even_when_it_is_not_shown() {
        use pond_core::models::services::context::token_counting::HeuristicTokenCounter;
        let msg = Message::assistant()
            .with_thinking(
                "The user asked about the porch light. I should check the device \
                 registry before claiming it exists.",
                "",
            )
            .with_text("The porch light is off.");

        // Display gate shut: nothing leaves as a frame.
        assert!(
            GooseAdapter::reasoning_frames(&msg, false).is_empty(),
            "the display gate stopped gating"
        );
        // Count is taken anyway.
        let counted = GooseAdapter::count_reasoning_tokens(&msg, &HeuristicTokenCounter);
        assert!(
            counted > 0,
            "reasoning must be counted even when show_thinking is off; got {counted}"
        );
        // And it is the same number the gate-open case would produce.
        assert_eq!(
            counted,
            GooseAdapter::count_reasoning_tokens(&msg, &HeuristicTokenCounter),
            "the count depends on something other than the message"
        );
    }

    /// Rules out counting `as_concat_text()`, which would report every answer as reasoning.
    #[test]
    fn answer_text_is_not_reasoning() {
        use pond_core::models::services::context::token_counting::HeuristicTokenCounter;
        let msg = Message::assistant()
            .with_text("A long and perfectly ordinary answer with no reasoning channel at all.");
        assert_eq!(
            GooseAdapter::count_reasoning_tokens(&msg, &HeuristicTokenCounter),
            0
        );
    }

    /// The pure-counter tests pass even if the stream counts inside the display gate.
    #[test]
    fn the_reasoning_count_is_taken_outside_the_display_gate() {
        let src = include_str!("goose_agent.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        let lines = stream_body_code();

        let gate_line = lines
            .iter()
            .position(|l| l.contains("reasoning.push(&msg, emit_reasoning)"))
            .expect("the gated push is gone; the P1 guard should have caught this first");

        let window = &lines[gate_line.saturating_sub(8)..gate_line];
        window
            .iter()
            .position(|l| l.contains("Self::count_reasoning_tokens(&msg,"))
            .unwrap_or_else(|| {
                panic!(
                    "the reasoning token count is not taken in the eight lines before the \
                     display gate. If it moved inside `for content in reasoning_frames(..)`, \
                     the count is now zero whenever show_thinking is off — which is the \
                     shipped default, so every Jetson turn would report no thinking."
                )
            });
        // The whole window: an `if emit_reasoning {` on a preceding line gates it just as well.
        // That would zero it: `stream_response_inner`, the only persisting path, is always voice.
        for line in window {
            assert!(
                !line.contains("emit_reasoning"),
                "the reasoning accumulation is now conditioned on emit_reasoning \
                 (`{}`); the cost of a turn must not depend on whether anybody is \
                 watching. On the only path that persists this number the flag is \
                 always false, so this reports every turn as having done no thinking.",
                line.trim()
            );
        }
        assert!(
            body.contains("turn_stats.reasoning_tokens.get_or_insert(0)"),
            "the count no longer accumulates into TurnStats, so nothing downstream sees it"
        );
        // The carry-out: both `UsageStats` arms (reported usage and fallback) must pass it on.
        assert_eq!(
            body.matches("reasoning_tokens: turn_stats.reasoning_tokens,")
                .count(),
            2,
            "both UsageStats arms must carry the counted reasoning out of the stream. \
             A literal `None` in either one drops the number on the providers that take \
             that path, and every unit test here still passes because they all call the \
             pure counter."
        );
    }

    /// Re-engaging an error retries a doomed turn and buries its cause under a generic message.
    #[test]
    fn a_turn_that_errored_is_not_re_engaged_as_an_empty_one() {
        let lines = stream_body_code();

        let set = lines
            .iter()
            .position(|l| l.contains("stream_failed = true"))
            .expect(
                "nothing sets `stream_failed`. The mid-stream error arm is back to \
                 falling through into the re-engagement logic, which retries a failure \
                 that cannot succeed and then buries its cause under a generic sentence.",
            );

        let checked = lines
            .iter()
            .position(|l| l.contains("if stream_failed {"))
            .expect("`stream_failed` is set but never checked, so nothing breaks the loop");

        let exhausted = lines
            .iter()
            .position(|l| l.contains("EMPTY_TURN_EXHAUSTED_MESSAGE.to_string()"))
            .expect("the exhausted-fallback emit has moved; this guard anchors on it");

        assert!(
            set < checked,
            "`stream_failed` is checked at line {checked}, above the assignment at line \
             {set} — so the flag read is always the previous attempt's.",
        );
        assert!(
            checked < exhausted,
            "the `stream_failed` break is at line {checked}, BELOW the fallback emit at \
             line {exhausted}. An errored turn would still be given \
             EMPTY_TURN_EXHAUSTED_MESSAGE, which is the bug this guards: a sentence \
             saying nothing could be produced, printed over a cause that was known.",
        );
    }

    /// An unconfigured session falls back to goose's global config, which has no provider.
    #[test]
    fn failing_to_configure_a_session_is_an_error_not_a_shrug() {
        let src = include_str!("goose_agent.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        let start = body
            .find("async fn ensure_provider_current")
            .expect("ensure_provider_current has been renamed; this guard is checking nothing");
        let rest = &body[start..];
        let end = rest[1..]
            .find("\n    async fn ")
            .or_else(|| rest[1..].find("\n    fn "))
            .map(|i| i + 1)
            .unwrap_or(rest.len());
        let func = &rest[..end];

        // Positive: the thinking re-stamp branch legitimately uses `if let Some(cached)`.
        assert!(
            func.contains("let Some((p, cfg)) = cached else {"),
            "the new-session backfill no longer uses `let ... else`. With `if let` and \
             no else it configures nothing and returns Ok when no provider has been \
             built, and the turn then fails one layer later inside goose with a message \
             about model config that names neither the session nor the cause.",
        );
        assert_eq!(
            func.matches("anyhow::bail!").count(),
            2,
            "expected both no-provider exits to bail — the missing cached provider and \
             the provider that could not be built. Found {}. One of them has gone back \
             to reporting success for a session it did not configure.",
            func.matches("anyhow::bail!").count(),
        );
    }

    /// An assignment above any `break 'attempts` reports 0 for the silent turns it should count.
    #[test]
    fn the_re_engagement_count_is_taken_after_every_exit_from_the_attempts_loop() {
        let lines = stream_body_code();

        let assign = lines
            .iter()
            .position(|l| l.contains("turn_stats.reengagements ="))
            .expect(
                "nothing assigns `turn_stats.reengagements`. The empty-turn recovery is \
                 unmeasured again: a re-engaged turn is the whole turn a second time, \
                 prefill included, and without this the only trace is a WARN.",
            );

        // From the counter, not a literal: `= 0` compiles and passes the first assertion.
        assert!(
            lines[assign].contains("attempt"),
            "`turn_stats.reengagements` is assigned from something other than the \
             attempt counter (`{}`), so the field no longer says what the turn cost.",
            lines[assign].trim()
        );

        let breaks: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains("break 'attempts"))
            .map(|(i, _)| i)
            .collect();
        assert!(
            breaks.len() >= 2,
            "expected the several exits from `'attempts` this guard is about; found {}. \
             The loop has been reshaped and this test is now checking nothing.",
            breaks.len()
        );
        let last_break = *breaks.last().unwrap();
        assert!(
            assign > last_break,
            "`turn_stats.reengagements` is assigned at line {assign}, above the exit at \
             line {last_break}. Every `break 'attempts` after the assignment skips it, and \
             the turns that skip it are the silent ones -- so the count would read 0 for \
             precisely the turns it exists to measure.",
        );

        // And before the stats are sealed, or the value never leaves.
        let finalize = lines
            .iter()
            .position(|l| l.contains("turn_stats.finalize_rates()"))
            .expect("the stream no longer finalizes its TurnStats");
        assert!(
            assign < finalize,
            "the re-engagement count is assigned after `finalize_rates`, i.e. after the \
             turn's stats have been sealed and sent.",
        );
    }

    /// `set_goal` would leak one member's request into another's turn: streams share one agent.
    /// `turn_text` carries injected memories, which the echoed goal would ask the model to satisfy.
    #[test]
    fn the_goal_is_armed_per_session_and_from_the_raw_request() {
        let lines = stream_body_code();

        let armed = lines
            .iter()
            .position(|l| l.contains("set_session_goal("))
            .expect(
                "nothing arms the per-session goal. Goose's completeness check is guarded on a \
                 goal being set, so without this the reply loop terminates when the model stops \
                 asking for tools and never when the question was answered -- which on 2026-08-12 \
                 was a ten-item query answered with zero tool calls and no objection.",
            );

        // A window, not the anchor line: the call spans several lines.
        let window = lines[armed..(armed + 10).min(lines.len())].join("\n");

        assert!(
            window.contains("request.message"),
            "the goal is armed from something other than the raw request. `turn_text` carries \
             <system-context> with injected memories and the turn budget, and the goal is echoed \
             back to the model as a thing to satisfy. Window:\n{window}"
        );
        assert!(
            !window.contains("turn_text"),
            "the goal is armed from `turn_text`, which is the assembled envelope rather than the \
             request. Window:\n{window}"
        );
        assert!(
            window.contains("goal_check_enabled"),
            "the goal is armed unconditionally. It costs roughly twice the inferences per turn, \
             so it rides `Settings::goal_check_enabled`. Window:\n{window}"
        );
        assert!(
            window.contains("warmup"),
            "the goal is armed for the prefix warm-up as well as for real turns. The warm-up is a \
             ping nobody reads, so the completeness check has nothing to check and costs it a \
             whole second round-trip -- measured at ~718 ms of a ~11 s warm-up whose only useful \
             work is the ~7 s prefill. Gate it on `!request.warmup`. Window:\n{window}"
        );

        // The process-wide setter must not appear in the stream at all.
        for (i, line) in lines.iter().enumerate() {
            assert!(
                !line.contains(".set_goal("),
                "line {i} calls the process-wide `set_goal` (`{}`). One retained agent serves up \
                 to four concurrent chat streams, so that slot puts one member's request text \
                 into another member's turn. Use `set_session_goal`.",
                line.trim()
            );
        }

        // Armed BEFORE the reply that reads it, or the first attempt runs unguarded.
        let reply = lines
            .iter()
            .position(|l| l.contains("agent_clone.reply("))
            .expect("the stream no longer calls agent_clone.reply");
        assert!(
            armed < reply,
            "the goal is armed at line {armed}, after the reply at line {reply} that reads it.",
        );
    }

    // ── context window precedence ─────────────────────────────────────────

    /// The registry pin is what llama.cpp allocated; budgeting past it gets the prompt truncated.
    #[test]
    fn a_pinned_local_context_outranks_a_larger_override() {
        let r =
            GooseAdapter::resolve_window_with("local", "gemma-4-E2B-it", 16384, Some(4096), None);
        assert_eq!(r.tokens, 4096);
        assert_eq!(
            r.source,
            pond_core::models::services::context::context_governor::WindowSource::Registry
        );
    }

    /// macOS/Metal leaves `context_size` unset, so local models are often unpinned.
    #[test]
    fn an_unpinned_local_model_falls_back_to_override_then_ceiling() {
        use pond_core::models::services::context::context_governor::WindowSource;

        let overridden =
            GooseAdapter::resolve_window_with("local", "gemma-4-E2B-it", 16384, None, None);
        assert_eq!(overridden.tokens, 16384);
        assert_eq!(overridden.source, WindowSource::Override);

        let ceiling = GooseAdapter::resolve_window_with("local", "gemma-4-E2B-it", 0, None, None);
        assert_eq!(ceiling.tokens, 32768);
        assert_eq!(ceiling.source, WindowSource::Heuristic);
    }

    #[test]
    fn http_providers_are_unaffected_by_the_registry_rule() {
        assert_eq!(
            GooseAdapter::resolve_window_with("ollama", "gemma4:e2b", 8192, None, None).tokens,
            8192
        );
        assert!(
            GooseAdapter::resolve_window_with("ollama", "gemma4:e2b", 0, None, None).tokens > 0
        );
    }

    #[test]
    fn a_catalog_window_reaches_the_governor_from_the_adapter() {
        use pond_core::models::services::context::context_governor::WindowSource;

        // Without it: the name heuristic, which does not recognise this model.
        let guessed =
            GooseAdapter::resolve_window_with("ollama", "some-unknown-model", 0, None, None);
        assert_eq!(guessed.tokens, 4096);
        assert_eq!(guessed.source, WindowSource::Heuristic);

        // With it: the catalog's number, clamped to UNPINNED_LOCAL_CEILING as ollama runs locally.
        let known = GooseAdapter::resolve_window_with(
            "ollama",
            "some-unknown-model",
            0,
            None,
            Some(131_072),
        );
        assert_eq!(
            known.tokens, 32_768,
            "a declared maximum is not an allocation, and ollama prefills locally"
        );
        assert_eq!(known.source, WindowSource::CatalogRecord);

        // A hosted provider keeps the raw declared window: nothing on this box prefills it.
        let hosted = GooseAdapter::resolve_window_with("openai", "gpt-4o", 0, None, Some(131_072));
        assert_eq!(hosted.tokens, 131_072);
        assert_eq!(hosted.source, WindowSource::CatalogRecord);

        // A registry pin still wins: it is the allocation, the catalog only a declared maximum.
        let pinned = GooseAdapter::resolve_window_with(
            "local",
            "gemma-4-E2B-it",
            0,
            Some(4096),
            Some(131_072),
        );
        assert_eq!(pinned.tokens, 4096);
        assert_eq!(pinned.source, WindowSource::Registry);
    }

    /// A model catalog holding exactly one row, for the wiring test below.
    struct StubCatalog {
        record: Option<ModelRecord>,
        /// Every id the adapter asked for, so the test can check the id's derivation.
        asked: std::sync::Mutex<Vec<String>>,
    }

    impl StubCatalog {
        fn holding(id: &str, context_length: Option<u32>) -> Self {
            Self {
                record: Some(ModelRecord {
                    id: id.to_string(),
                    category: ModelCategory::Ollama,
                    name: "gemma4:e2b".to_string(),
                    filename: None,
                    description: String::new(),
                    size_mb: 0,
                    url: None,
                    hf_id: None,
                    ram_estimate_mb: None,
                    recommended_role: None,
                    context_length,
                    quantization: None,
                    asr_language: None,
                    asr_size: None,
                    tts_engine: None,
                    tts_voice_name: None,
                    config_filename: None,
                    config_url: None,
                    tts_url: None,
                    sample_rate: None,
                    downloaded: true,
                    is_custom: false,
                }),
                asked: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ModelRepository for StubCatalog {
        async fn list_all(&self) -> Result<Vec<ModelRecord>> {
            Ok(self.record.clone().into_iter().collect())
        }
        async fn list_by_category(&self, _c: &ModelCategory) -> Result<Vec<ModelRecord>> {
            Ok(vec![])
        }
        async fn get_by_id(&self, id: &str) -> Result<Option<ModelRecord>> {
            self.asked
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(id.to_string());
            Ok(self.record.as_ref().filter(|r| r.id == id).cloned())
        }
        async fn upsert(&self, _m: &ModelRecord) -> Result<()> {
            Ok(())
        }
        async fn set_downloaded(&self, _id: &str, _d: bool) -> Result<()> {
            Ok(())
        }
        async fn list_assignments(
            &self,
        ) -> Result<Vec<pond_core::models::domain::model_record::ModelRoleAssignment>> {
            Ok(vec![])
        }
        async fn get_assignment(
            &self,
            _r: &str,
        ) -> Result<Option<pond_core::models::domain::model_record::ModelRoleAssignment>> {
            Ok(None)
        }
        async fn set_assignment(&self, _r: &str, _m: &str) -> Result<()> {
            Ok(())
        }
        async fn clear_assignment(&self, _r: &str) -> Result<()> {
            Ok(())
        }
    }

    /// The wiring, not the precedence: it goes through the repository the adapter holds.
    #[tokio::test]
    async fn the_adapter_reads_the_catalog_it_was_given() {
        use pond_core::models::services::context::context_governor::WindowSource;

        // No catalog: the name heuristic answers 128,000 for gemma 4, a typed-in round number.
        let blind = GooseAdapter::resolve_window_from(None, "ollama", "gemma4:e2b", 0, None).await;
        assert_eq!(blind.tokens, 128_000);
        assert_eq!(blind.source, WindowSource::Heuristic);

        // An unrecognised model gets the conservative default; the case rung 3 rescues.
        let unknown =
            GooseAdapter::resolve_window_from(None, "ollama", "some-unknown-model", 0, None).await;
        assert_eq!(unknown.tokens, 4096);
        assert_eq!(unknown.source, WindowSource::Heuristic);

        // With the catalog: read, then clamped to UNPINNED_LOCAL_CEILING (on-device provider).
        // The fallback can't yield 32,768 (its heuristic says 128,000), so this isn't vacuous.
        let catalog: Arc<dyn ModelRepository> =
            Arc::new(StubCatalog::holding("ollama/gemma4:e2b", Some(131_072)));
        let seen =
            GooseAdapter::resolve_window_from(Some(&catalog), "ollama", "gemma4:e2b", 0, None)
                .await;
        assert_eq!(
            seen.tokens, 32_768,
            "the catalog row's context_length never reached the governor"
        );
        assert_eq!(seen.source, WindowSource::CatalogRecord);

        // Keyed by category, not provider; a wrong key silently degrades to the heuristic.
        let stub = Arc::new(StubCatalog::holding("ollama/gemma4:e2b", Some(131_072)));
        let probe: Arc<dyn ModelRepository> = stub.clone();
        let _ =
            GooseAdapter::resolve_window_from(Some(&probe), "ollama", "gemma4:e2b", 0, None).await;
        let _ = GooseAdapter::resolve_window_from(Some(&probe), "local", "gemma-4-E2B-it", 0, None)
            .await;
        assert_eq!(
            stub.asked
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_slice(),
            ["ollama/gemma4:e2b", "gguf/gemma-4-E2B-it"],
            "the catalog id must be derived from the CATEGORY the row is keyed by"
        );

        // A registry pin wins (rung 2), so the catalog row is never fetched.
        let counting = Arc::new(StubCatalog::holding("gguf/gemma-4-E2B-it", Some(131_072)));
        let counting_port: Arc<dyn ModelRepository> = counting.clone();
        let pinned = GooseAdapter::resolve_window_from(
            Some(&counting_port),
            "local",
            "gemma-4-E2B-it",
            0,
            Some(4096),
        )
        .await;
        assert_eq!(pinned.tokens, 4096);
        assert_eq!(pinned.source, WindowSource::Registry);
        assert!(
            counting
                .asked
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty(),
            "a pinned registry size must not pay for a catalog read it will discard"
        );
    }

    /// `GOOSE_CONTEXT_LIMIT` is still written (Ollama's `num_ctx`) but must never be read back.
    #[test]
    fn no_budget_path_reads_the_context_limit_from_the_environment() {
        let src = include_str!("goose_agent.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        assert!(
            !body.contains("env::var(\"GOOSE_CONTEXT_LIMIT\")"),
            "context windows must come from ContextGovernor, not the environment"
        );
    }

    // ── One asymmetric profile per turn ──────────────────────────────────

    /// The ledger's share with no child delegating; named so `0.0` doesn't read as a tolerance.
    const NO_LIVE_CHILD: f32 = 0.0;

    /// The reasoning history of a pond that has measured none; the derivation floors at the anchor.
    const NO_REASONING_HISTORY: &[u32] = &[];

    /// The adapter must pass the anchor as the floor; zero would let the reserve shrink to nothing.
    #[test]
    fn an_unmeasured_pond_gets_exactly_the_profile_it_got_before_pai_5_p5() {
        for window in [4_096usize, 8_192, 32_768] {
            let unmeasured = GooseAdapter::profile_for("local", window, NO_LIVE_CHILD, &[]);
            let anchor = pond_core::models::services::context::context_budget::CompactionProfile::for_windows(
                window,
                ContextGovernor::prompt_window("local", window),
            );
            assert_eq!(
                unmeasured.output_reserve_tokens, anchor.output_reserve_tokens,
                "at window {window} an unmeasured pond's reserve moved. P5 must be inert until                  there is evidence, or every install changes behaviour on upgrade for no reason"
            );
        }
    }

    /// The other direction: without it, a feature that does nothing passes the test above.
    #[test]
    fn a_pond_that_reasons_expensively_gets_a_bigger_reserve_than_the_anchor() {
        let unmeasured = GooseAdapter::profile_for("local", 32_768, NO_LIVE_CHILD, &[]);

        // Sized off the anchor, not a literal: a flat cost can quantise to exactly the anchor.
        let dear: Vec<u32> = vec![unmeasured.output_reserve_tokens as u32; 40];
        let measured = GooseAdapter::profile_for("local", 32_768, NO_LIVE_CHILD, &dear);

        assert!(
            measured.output_reserve_tokens > unmeasured.output_reserve_tokens,
            "40 turns of {}-token reasoning did not move the reserve ({} vs {}). The adapter is \
             not passing the samples through, and that is invisible from pond-core -- the \
             derivation's own tests would all still pass",
            unmeasured.output_reserve_tokens,
            measured.output_reserve_tokens,
            unmeasured.output_reserve_tokens
        );
    }

    /// Pins that the adapter hands `for_windows` its two windows the right way round.
    #[test]
    fn a_local_turn_budgets_history_from_the_window_and_the_preamble_from_the_clamp() {
        // A Mac that resolved 32,768: four times the KV cache of the clamp.
        let big = GooseAdapter::profile_for("local", 32_768, NO_LIVE_CHILD, NO_REASONING_HISTORY);
        let clamped =
            GooseAdapter::profile_for("local", 8_192, NO_LIVE_CHILD, NO_REASONING_HISTORY);

        // Preamble: frozen at the clamp's allowance; it is the KV prefix, so it sets TTFT.
        assert_eq!(big.system_prompt_budget, clamped.system_prompt_budget);
        assert_eq!(big.memory_token_budget, clamped.memory_token_budget);
        assert_eq!(big.max_memory_fragments, clamped.max_memory_fragments);
        assert!(
            big.use_compact_prompt(),
            "a 32K KV cache selected the verbose prompt tier on a local provider"
        );

        // History: scaled with the real window, taking what the preamble may not.
        assert_eq!(big.context_window_tokens, 32_768);
        assert_eq!(big.history_token_budget, 24_000);
        assert!(big.history_token_budget > clamped.history_token_budget);
    }

    /// Scaling the window instead would re-derive the preamble, costing the parent a re-prefill.
    #[test]
    fn a_live_child_shrinks_the_parents_history_and_leaves_its_prefix_alone() {
        let alone = GooseAdapter::profile_for("local", 32_768, NO_LIVE_CHILD, NO_REASONING_HISTORY);
        let sharing = GooseAdapter::profile_for("local", 32_768, 0.5, NO_REASONING_HISTORY);

        assert!(
            sharing.history_token_budget < alone.history_token_budget,
            "a live child did not shrink the parent's history budget: {} vs {}",
            sharing.history_token_budget,
            alone.history_token_budget
        );
        assert_eq!(
            sharing.context_window_tokens, alone.context_window_tokens,
            "the resolved window moved, so the reservation was applied by scaling the window - \
             which re-derives every preamble allowance and moves the KV prefix"
        );
        assert_eq!(
            sharing.prompt_window_tokens, alone.prompt_window_tokens,
            "the prompt-side clamp moved under a reservation"
        );
        assert_eq!(
            sharing.system_prompt_budget, alone.system_prompt_budget,
            "the system prompt allowance moved under a reservation, so the preamble is rebuilt \
             at a different size and the parent pays a re-prefill for having delegated"
        );
        assert_eq!(
            sharing.memory_token_budget, alone.memory_token_budget,
            "the memory allowance moved under a reservation"
        );
        assert_eq!(
            sharing.use_compact_prompt(),
            alone.use_compact_prompt(),
            "the prompt tier flipped under a reservation"
        );
    }

    /// Reservations are keyed by GIAP session id; any derived key silently reads 0.0.
    #[test]
    fn a_parents_budget_shrinks_for_its_own_sessions_children_and_for_nobody_elses() {
        let ledger = Arc::new(crate::orchestrator::DeviceLedger::default());
        let _child = ledger.reserve("sess-A", 0.5);

        let delegating = GooseAdapter::profile_for_session(
            &ledger,
            "local",
            32_768,
            "sess-A",
            NO_REASONING_HISTORY,
        );
        let bystander = GooseAdapter::profile_for_session(
            &ledger,
            "local",
            32_768,
            "sess-B",
            NO_REASONING_HISTORY,
        );

        assert!(
            delegating.history_token_budget < bystander.history_token_budget,
            "a session with a live child budgeted {} history tokens and a session with none \
             budgeted {}; the reservation is being looked up under a key nothing writes, so \
             every parent on this pond reads 0.0 whatever its children are holding",
            delegating.history_token_budget,
            bystander.history_token_budget
        );
        assert_eq!(
            bystander.history_token_budget,
            GooseAdapter::profile_for("local", 32_768, NO_LIVE_CHILD, NO_REASONING_HISTORY)
                .history_token_budget,
            "a session with no live child of its own was charged for somebody else's, so the \
             lookup is not keyed by session at all"
        );
    }

    /// HTTP providers pay no local prefill, so they get the symmetric profile.
    #[test]
    fn an_http_turn_is_not_clamped_at_all() {
        let p = GooseAdapter::profile_for("ollama", 32_768, NO_LIVE_CHILD, NO_REASONING_HISTORY);
        assert_eq!(p.system_prompt_budget, 6_000);
        assert_eq!(p.memory_token_budget, 1_500);
        assert_eq!(p.history_token_budget, 20_000);
        assert!(!p.use_compact_prompt());
    }

    // ── F1: image attachment onto the user message ───────────────────────

    fn img(data: &str, mime: &str) -> pond_core::models::domain::message::ImageAttachment {
        pond_core::models::domain::message::ImageAttachment {
            data: data.to_string(),
            mime_type: mime.to_string(),
        }
    }

    /// Pull out (base64, mime) for every image part, in order.
    fn image_parts(msg: &Message) -> Vec<(String, String)> {
        msg.content
            .iter()
            .filter_map(|c| match c {
                goose::conversation::message::MessageContent::Image(i) => {
                    Some((i.data.clone(), i.mime_type.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_text_only_turn_gets_no_image_parts() {
        let msg = attach_images(Message::user().with_text("hello"), &[]);
        assert!(image_parts(&msg).is_empty());
        assert_eq!(msg.as_concat_text(), "hello");
    }

    #[test]
    fn every_image_is_attached_and_order_is_preserved() {
        let images = vec![
            img("AAAA", "image/png"),
            img("BBBB", "image/jpeg"),
            img("CCCC", "image/webp"),
        ];
        let msg = attach_images(Message::user().with_text("look"), &images);
        assert_eq!(
            image_parts(&msg),
            vec![
                ("AAAA".to_string(), "image/png".to_string()),
                ("BBBB".to_string(), "image/jpeg".to_string()),
                ("CCCC".to_string(), "image/webp".to_string()),
            ]
        );
    }

    /// The text part stays first and unmodified: the envelope strippers match on it.
    #[test]
    fn the_text_envelope_is_untouched_by_attachment() {
        let envelope =
            "<system-context>\n<turn-budget/>\n</system-context>\n<user-message>\nhi\n</user-message>";
        let msg = attach_images(
            Message::user().with_text(envelope),
            &[img("AAAA", "image/png")],
        );
        assert_eq!(msg.as_concat_text(), envelope);
        assert!(matches!(
            msg.content.first(),
            Some(goose::conversation::message::MessageContent::Text(_))
        ));
        assert_eq!(image_parts(&msg).len(), 1);
    }

    // ── F3: the vision-capability prompt section ─────────────────────────

    /// A text-only model told it can see will describe an image that was never there.
    #[test]
    fn a_text_only_model_never_gets_the_vision_section() {
        for compact in [false, true] {
            let out =
                GooseAdapter::apply_vision_section("<identity>x</identity>".into(), false, compact);
            assert_eq!(out, "<identity>x</identity>");
            assert!(!out.contains("<vision>"));
        }
    }

    #[test]
    fn a_vision_model_gets_exactly_one_vision_section_at_the_tier_it_pays_for() {
        for compact in [false, true] {
            let out =
                GooseAdapter::apply_vision_section("<identity>x</identity>".into(), true, compact);
            assert!(out.starts_with("<identity>x</identity>"));
            assert_eq!(out.matches("<vision>").count(), 1);
            assert!(out.contains(pond_core::prompts::vision_capability_section(compact)));
        }
        // The compact tier must not pay for the verbose wording.
        let verbose = GooseAdapter::apply_vision_section(String::new(), true, false);
        let compact = GooseAdapter::apply_vision_section(String::new(), true, true);
        assert!(compact.len() < verbose.len());
    }

    /// Registry mmproj presence, not the `gemma-4*` name, decides: E1B has no vision encoder.
    #[test]
    fn local_vision_capability_comes_from_the_mmproj_registry() {
        for provider in ["local", "gguf"] {
            assert!(GooseAdapter::model_supports_vision(
                provider,
                "gemma-4-E2B-it"
            ));
            assert!(GooseAdapter::model_supports_vision(
                provider,
                "gemma-4-E4B-it-Q4_K_M"
            ));
            assert!(
                !GooseAdapter::model_supports_vision(provider, "gemma-4-E1B-it"),
                "E1B declares no mmproj — the name heuristic would wrongly say yes"
            );
            assert!(!GooseAdapter::model_supports_vision(
                provider,
                "Llama-3.2-3B-Instruct"
            ));
        }
    }

    /// With no registry, the name rule must reach the registry's verdict, E1B exclusion included.
    #[test]
    fn http_vision_capability_matches_the_registry_and_covers_real_ollama_tags() {
        for provider in ["ollama", "llamafile", "openai"] {
            assert!(
                !GooseAdapter::model_supports_vision(provider, "gemma-4-E1B-it"),
                "{provider}: E1B has no vision encoder on ANY provider"
            );
            assert!(
                !GooseAdapter::model_supports_vision(provider, "gemma3n:e1b"),
                "{provider}: same model, Ollama's spelling"
            );
            for model in [
                "gemma-4-E4B-it",
                "gemma3n:e4b",
                "llama3.2-vision:11b",
                "qwen2.5-vl:7b",
                "minicpm-v:8b",
                "pixtral-12b",
            ] {
                assert!(
                    GooseAdapter::model_supports_vision(provider, model),
                    "{provider}/{model} accepts images"
                );
            }
            assert!(!GooseAdapter::model_supports_vision(
                provider,
                "Llama-3.2-3B-Instruct"
            ));
            assert!(!GooseAdapter::model_supports_vision(
                provider,
                "llama3.2:3b"
            ));
        }
    }

    #[test]
    fn voice_mode_suppresses_the_vision_section_just_as_capabilities_does() {
        let vision_model = ("ollama", "gemma-4-E4B-it");
        assert!(GooseAdapter::vision_section_applies(
            vision_model.0,
            vision_model.1,
            false
        ));
        assert!(
            !GooseAdapter::vision_section_applies(vision_model.0, vision_model.1, true),
            "voice mode reports vision=false; the prompt must not claim otherwise"
        );
        // A text-only model stays off in both modes.
        for voice in [false, true] {
            assert!(!GooseAdapter::vision_section_applies(
                "ollama",
                "Llama-3.2-3B-Instruct",
                voice
            ));
        }
    }

    /// Small on-device models mangle digits-to-words themselves, so voice gets spoken time.
    #[test]
    fn voice_mode_renders_spoken_time_text_mode_keeps_digits() {
        use chrono::TimeZone;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 8, 3, 5, 23, 0)
            .unwrap();

        let voice = GooseAdapter::format_current_time(now, true);
        assert_eq!(voice, "five twenty-three in the morning");

        let text = GooseAdapter::format_current_time(now, false);
        assert_eq!(text, "05:23");
    }

    // ── F2 live half: the image cap on the in-turn trimmer ───────────────

    fn trim_msg(
        index: usize,
        text: &str,
    ) -> pond_core::models::services::context::turn_trimmer::TrimMessage {
        pond_core::models::services::context::turn_trimmer::TrimMessage {
            index,
            role: pond_core::models::services::context::turn_trimmer::TrimRole::User,
            text: text.to_string(),
            is_summary: false,
            // The image cap is age-blind; its policy lives in `image_history`.
            age_secs: None,
        }
    }

    fn user_with_images(text: &str, images: &[&str]) -> Message {
        images.iter().fold(Message::user().with_text(text), |m, d| {
            m.with_image(*d, "image/png")
        })
    }

    /// KV invariant: with no images `dropped` is 0, and only a nonzero `dropped` rewrites.
    #[test]
    fn a_text_only_conversation_plans_no_image_change() {
        let source = vec![
            Message::user().with_text("hi"),
            Message::assistant().with_text("hello"),
            Message::user().with_text("bye"),
        ];
        let (had, keep, dropped) = plan_image_cap(&source);
        assert_eq!(had, vec![0, 0, 0]);
        assert_eq!(keep, vec![0, 0, 0]);
        assert_eq!(dropped, 0);
    }

    /// One image is inside the budget: still nothing to rewrite.
    #[test]
    fn a_single_historical_image_is_left_alone() {
        let source = vec![user_with_images("look", &["AAAA"])];
        let (_, _, dropped) = plan_image_cap(&source);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn only_the_newest_image_bearing_turn_keeps_pixels() {
        let source = vec![
            user_with_images("first", &["AAAA"]),
            Message::assistant().with_text("ok"),
            user_with_images("second", &["BBBB", "CCCC"]),
        ];
        let (had, keep, dropped) = plan_image_cap(&source);
        assert_eq!(had, vec![1, 0, 2]);
        assert_eq!(keep, vec![0, 0, 1], "newest-first, leading image kept");
        assert_eq!(dropped, 2);
    }

    /// Providers reject a broken request/response pair, so the cap must not even count these.
    #[test]
    fn messages_carrying_tool_parts_are_never_capped() {
        let request = Message::assistant().with_tool_request(
            "call-1",
            Ok(rmcp::model::CallToolRequestParams::new(
                "look_at_camera_snapshot".to_string(),
            )),
        );
        // Camera tools do return frames in the response; the cap must still leave it alone.
        let response = tool_response_message("call-1", "front-door, person");
        assert!(has_tool_parts(&request));
        assert!(has_tool_parts(&response));

        let source = vec![
            user_with_images("first", &["AAAA"]),
            request,
            response,
            user_with_images("second", &["BBBB"]),
        ];
        let (had, keep, dropped) = plan_image_cap(&source);
        assert_eq!(
            &had[1..3],
            &[0, 0],
            "tool messages contribute nothing to the plan"
        );
        assert_eq!(&keep[1..3], &[0, 0]);
        // The two plain image turns are still capped normally around them.
        assert_eq!(dropped, 1);
    }

    #[test]
    fn a_partially_capped_message_does_not_deny_the_image_it_still_shows() {
        use pond_core::models::services::context::image_history::{
            HISTORY_IMAGE_PLACEHOLDER, HISTORY_IMAGE_PLACEHOLDER_MARKER,
            HISTORY_IMAGE_PLACEHOLDER_PARTIAL,
        };
        let original = user_with_images("look at these", &["AAAA", "BBBB", "CCCC"]);
        let capped = cap_message_images(&original, 1, "look at these");
        assert_eq!(
            image_parts(&capped),
            vec![("AAAA".into(), "image/png".into())]
        );
        let text = capped.as_concat_text();
        assert!(text.contains("look at these"));
        assert!(text.contains(HISTORY_IMAGE_PLACEHOLDER_PARTIAL));
        assert!(
            !text.contains(HISTORY_IMAGE_PLACEHOLDER),
            "the all-dropped wording contradicts the surviving image: {text}"
        );
        assert_eq!(text.matches(HISTORY_IMAGE_PLACEHOLDER_MARKER).count(), 1);
    }

    #[test]
    fn a_fully_stripped_image_turn_still_says_an_image_was_there() {
        let original = user_with_images("what colour is this?", &["AAAA"]);
        let capped = cap_message_images(&original, 0, "what colour is this?");
        assert!(image_parts(&capped).is_empty());
        assert!(capped.as_concat_text().contains(
            pond_core::models::services::context::image_history::HISTORY_IMAGE_PLACEHOLDER
        ));
    }

    #[test]
    fn capping_preserves_message_identity() {
        let mut original = user_with_images("look", &["AAAA", "BBBB"]);
        original.id = Some("msg-7".into());
        original.created = 1_234_567;
        let capped = cap_message_images(&original, 1, "look");
        assert_eq!(capped.id.as_deref(), Some("msg-7"));
        assert_eq!(capped.created, 1_234_567);
        assert_eq!(capped.role, original.role);
    }

    /// The text-only rewrite never reaches image turns, so the cap must strip their stale envelope.
    #[test]
    fn capping_applies_the_trimmed_text_to_an_image_turn() {
        let stale = "<system-context>\nToday is Tuesday\n</system-context>\n<user-message>look</user-message>";
        let original = user_with_images(stale, &["AAAA"]);
        let trimmed_text =
            pond_core::models::services::context::turn_trimmer::strip_system_context(stale)
                .into_owned();
        let capped = cap_message_images(&original, 0, &trimmed_text);
        assert!(!capped.as_concat_text().contains("<system-context>"));
        assert!(capped
            .as_concat_text()
            .contains("<user-message>look</user-message>"));
    }

    /// A fully capped message: no images left, so a second pass plans nothing.
    #[test]
    fn capping_is_idempotent() {
        let original = user_with_images("look", &["AAAA", "BBBB"]);
        let once = cap_message_images(&original, 0, "look");
        let (had, keep, dropped) = plan_image_cap(std::slice::from_ref(&once));
        assert_eq!(had, vec![0]);
        assert_eq!(keep, vec![0]);
        assert_eq!(dropped, 0, "nothing left to drop on a second pass");
    }

    /// Stage 2's text already holds the pass-1 placeholder, so nothing else stops a second one.
    #[test]
    fn staged_capping_converges_to_exactly_one_placeholder() {
        use pond_core::models::services::context::image_history::{
            HISTORY_IMAGE_PLACEHOLDER, HISTORY_IMAGE_PLACEHOLDER_MARKER,
            HISTORY_IMAGE_PLACEHOLDER_PARTIAL,
        };

        // Stage 1: newest turn, budget 1: keep the leading image, one placeholder.
        let original = user_with_images("look at these", &["AAAA", "BBBB"]);
        let (had, keep, dropped) = plan_image_cap(std::slice::from_ref(&original));
        assert_eq!((had[0], keep[0], dropped), (2, 1, 1));
        let stage1 = cap_message_images(&original, keep[0], &original.as_concat_text());
        assert_eq!(image_parts(&stage1).len(), 1);
        assert_eq!(
            stage1
                .as_concat_text()
                .matches(HISTORY_IMAGE_PLACEHOLDER_MARKER)
                .count(),
            1
        );

        // Stage 2: a newer image turn takes the budget; the text still includes the placeholder.
        let newer = user_with_images("and this one", &["CCCC"]);
        let source = vec![stage1.clone(), newer];
        let trimmed = vec![
            trim_msg(0, &source[0].as_concat_text()),
            trim_msg(1, &source[1].as_concat_text()),
        ];
        let (had, keep, dropped) = plan_image_cap(&source);
        assert_eq!((had[0], keep[0]), (1, 0), "the older turn loses its image");
        assert_eq!((had[1], keep[1]), (1, 1), "the newest turn keeps its own");
        assert_eq!(dropped, 1);

        let stage2 = cap_message_images(&source[0], keep[0], &trimmed[0].text);
        let text = stage2.as_concat_text();
        assert!(image_parts(&stage2).is_empty());
        assert_eq!(
            text.matches(HISTORY_IMAGE_PLACEHOLDER_MARKER).count(),
            1,
            "one placeholder for the message's state, not one per pass: {text}"
        );
        assert!(
            text.contains(HISTORY_IMAGE_PLACEHOLDER),
            "no image survives now, so the all-dropped wording is the true one: {text}"
        );
        assert!(
            !text.contains(HISTORY_IMAGE_PLACEHOLDER_PARTIAL),
            "the stale partial wording claims an image is still shown: {text}"
        );
        assert!(
            text.contains("look at these"),
            "the user's own text survives"
        );

        // Stage 3: a third pass changes nothing further.
        let stage3 = cap_message_images(&stage2, 0, &text);
        assert_eq!(stage3.as_concat_text(), text);
    }

    // ── B3: the Goose cap-message coupling ───────────────────────────────

    /// Canary: upstream's constant is private, so a reworded copy silently breaks cap detection.
    #[test]
    fn goose_cap_message_is_still_verbatim() {
        let agent_rs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../goose/crates/goose/src/agents/agent.rs");
        let Ok(source) = std::fs::read_to_string(&agent_rs) else {
            // Submodule not initialised; skip rather than fail on a checkout without the fork.
            eprintln!("skipping: {} unavailable", agent_rs.display());
            return;
        };
        assert!(
            source.contains(&format!(
                "MAX_TURNS_MESSAGE: &str = \"{GOOSE_MAX_TURNS_MESSAGE}\""
            )),
            "Goose's MAX_TURNS_MESSAGE no longer matches GOOSE_MAX_TURNS_MESSAGE — \
             update the constant in goose_agent.rs or turn-limit detection is dead"
        );
    }

    /// Canary for [`GOOSE_GOAL_NOTIFICATION_PREFIX`], which the repetition guard counts.
    #[test]
    fn goose_still_announces_a_completeness_recheck_as_a_goal_notification() {
        let agent_rs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../goose/crates/goose/src/agents/agent.rs");
        let Ok(source) = std::fs::read_to_string(&agent_rs) else {
            eprintln!("skipping: {} unavailable", agent_rs.display());
            return;
        };
        assert!(
            source.contains("format!(\"Goal: {goal}\")"),
            "Goose no longer announces a completeness re-check as `Goal: {{goal}}` — \
             update GOOSE_GOAL_NOTIFICATION_PREFIX or the repetition guard counts nothing"
        );
    }

    #[test]
    fn a_repeated_identical_tool_call_is_the_same_fingerprint_whatever_the_key_order() {
        let mut a = serde_json::Map::new();
        a.insert("location".into(), serde_json::json!("Nairobi"));
        a.insert("units".into(), serde_json::json!("metric"));
        let mut b = serde_json::Map::new();
        b.insert("units".into(), serde_json::json!("metric"));
        b.insert("location".into(), serde_json::json!("Nairobi"));
        assert_eq!(
            canonical_args_fingerprint(Some(&a)),
            canonical_args_fingerprint(Some(&b)),
            "key order changed the fingerprint — has serde_json/preserve_order been enabled?"
        );
    }

    #[test]
    fn two_calls_with_different_arguments_are_different_fingerprints() {
        let mut a = serde_json::Map::new();
        a.insert("location".into(), serde_json::json!("Nairobi"));
        let mut b = serde_json::Map::new();
        b.insert("location".into(), serde_json::json!("Mombasa"));
        assert_ne!(
            canonical_args_fingerprint(Some(&a)),
            canonical_args_fingerprint(Some(&b))
        );
    }

    /// No-argument tools (e.g. `music__status`) can only be re-called identically; count them.
    #[test]
    fn a_tool_that_takes_no_arguments_fingerprints_the_same_every_time() {
        assert_eq!(
            canonical_args_fingerprint(None),
            canonical_args_fingerprint(None)
        );
        let empty = serde_json::Map::new();
        assert_eq!(
            canonical_args_fingerprint(Some(&empty)),
            canonical_args_fingerprint(Some(&empty))
        );
    }

    #[test]
    fn nested_arguments_canonicalise_at_every_depth() {
        let a: serde_json::Value = serde_json::json!({"q": {"x": 1, "y": 2}});
        let b: serde_json::Value = serde_json::json!({"q": {"y": 2, "x": 1}});
        assert_eq!(
            canonical_args_fingerprint(a.as_object()),
            canonical_args_fingerprint(b.as_object())
        );
    }

    #[test]
    fn the_repetition_budget_is_per_tool_and_not_shared() {
        let mut counts: std::collections::HashMap<(String, u64), usize> =
            std::collections::HashMap::new();
        let fp = canonical_args_fingerprint(None);
        for tool in [
            "giap-weather__get_current_weather",
            "music__status",
            "giap-device__get_user_profile",
        ] {
            *counts.entry((tool.to_string(), fp)).or_insert(0) += 1;
        }
        assert_eq!(
            counts.len(),
            3,
            "three different tools collapsed into one budget"
        );
        assert!(
            counts
                .values()
                .all(|n| *n <= MAX_IDENTICAL_TOOL_CALLS_PER_TURN),
            "one call each must never trip the guard"
        );
    }

    #[test]
    fn the_repetition_guard_names_no_particular_tool() {
        let body = stream_body_code();
        let guard_region: String = body
            .iter()
            .skip_while(|l| !l.contains("canonical_args_fingerprint("))
            .take(40)
            .cloned()
            .collect::<Vec<String>>()
            .join("\n");
        assert!(
            !guard_region.is_empty(),
            "the repetition backstop is gone from the stream body (stream_body_code \
             strips comments, so this anchors on the fingerprint call itself)"
        );
        for named in ["weather", "music__", "get_current_weather", "status"] {
            assert!(
                !guard_region.contains(named),
                "the repetition guard special-cases {named:?}; it must key on whatever \
                 tool the model called so it covers every tool, including future ones"
            );
        }
    }

    #[test]
    fn the_repetition_budgets_stay_well_inside_the_turn_cap() {
        assert!(
            MAX_GOAL_RECHECKS_PER_TURN >= 2,
            "too tight to allow a real multi-tool turn"
        );
        assert!(
            MAX_GOAL_RECHECKS_PER_TURN <= 4,
            "a completeness check this patient is a loop"
        );
        assert!(
            MAX_IDENTICAL_TOOL_CALLS_PER_TURN >= 2,
            "one retry after a transient failure is legitimate"
        );
        assert!(
            MAX_IDENTICAL_TOOL_CALLS_PER_TURN <= 5,
            "nothing a household asks needs this many identical calls"
        );
    }

    #[test]
    fn only_the_cap_sentence_counts_as_a_turn_limit() {
        let hit = |t: &str| t.trim() == GOOSE_MAX_TURNS_MESSAGE;
        assert!(hit(GOOSE_MAX_TURNS_MESSAGE));
        assert!(hit(&format!("\n{GOOSE_MAX_TURNS_MESSAGE}\n")));
        assert!(!hit("I've reached the maximum number of actions."));
        assert!(!hit(&format!(
            "{GOOSE_MAX_TURNS_MESSAGE} Also here is more."
        )));
        assert!(!hit("Would you like me to continue?"));
        assert!(!hit(""));
    }

    // ── Env-knob decision table ──────────────────────────────────────────

    fn knob(knobs: &[(&'static str, Option<String>)], key: &str) -> Option<String> {
        knobs
            .iter()
            .find(|(k, _)| *k == key)
            .and_then(|(_, v)| v.clone())
    }

    /// Both compaction knobs stay unset: goose, not GIAP, owns context management.
    #[test]
    fn goose_owns_its_own_compaction_now() {
        for provider in ["local", "gguf", "ollama", "llamafile"] {
            for ctx in [4096usize, 8192] {
                let knobs = goose_env_knobs(provider, ctx);
                assert!(
                    knob(&knobs, "GOOSE_AUTO_COMPACT_THRESHOLD").is_none(),
                    "{provider} ctx {ctx}"
                );
                assert!(
                    knob(&knobs, "GOOSE_TOOL_PAIR_SUMMARIZATION").is_none(),
                    "{provider} ctx {ctx}"
                );
            }
        }

        for ctx in [4096usize, 8192] {
            let knobs = goose_env_knobs("local", ctx);
            assert_eq!(
                knob(&knobs, "GOOSE_CONTEXT_LIMIT").as_deref(),
                Some(ctx.to_string().as_str()),
                "the window is still ours to declare — goose defaults to 128K"
            );
            assert!(
                knob(&knobs, "GOOSE_AUTO_COMPACT_THRESHOLD").is_none(),
                "unset, so goose's own 0.8 applies (ctx {ctx})"
            );
            assert!(
                knob(&knobs, "GOOSE_TOOL_PAIR_SUMMARIZATION").is_none(),
                "unset, so goose's background tool-pair summaries run (ctx {ctx})"
            );
        }
    }

    /// At goose's 200,000-char default, one MCP result stalls the turn in reactive compaction.
    #[test]
    fn a_single_tool_result_cannot_blow_the_local_window() {
        for ctx in [4096usize, 8192, 16384] {
            let cap: usize = knob(
                &goose_env_knobs("local", ctx),
                "GOOSE_MAX_TOOL_RESPONSE_SIZE",
            )
            .expect("the local engine caps tool responses")
            .parse()
            .expect("a byte count");

            // A quarter of the budget, expressed in bytes at ~4 chars/token.
            assert_eq!(cap, ctx, "ctx {ctx}");
            assert!(
                cap / 4 < ctx,
                "a result at the cap is {} tokens against a {ctx}-token budget — it would \
                 still overflow the window it is meant to protect",
                cap / 4
            );
        }

        // Goose's own default is what this exists to displace.
        assert!(
            knob(
                &goose_env_knobs("local", 8192),
                "GOOSE_MAX_TOOL_RESPONSE_SIZE"
            )
            .map(|v| v.parse::<usize>().unwrap() < 200_000)
            .unwrap_or(false),
            "the cap must be below goose's 200,000-char default or it changes nothing"
        );
    }

    /// An HTTP provider's window is not ours to ration.
    #[test]
    fn http_providers_keep_gooses_own_tool_response_limit() {
        for provider in ["ollama", "llamafile"] {
            assert!(
                knob(
                    &goose_env_knobs(provider, 32768),
                    "GOOSE_MAX_TOOL_RESPONSE_SIZE"
                )
                .is_none(),
                "{provider}"
            );
        }
    }

    /// Goose's retry resends an unchanged conversation, which a local model answers identically.
    #[test]
    fn goose_never_retries_empty_turns_itself() {
        for provider in ["local", "gguf", "ollama", "llamafile"] {
            for hybrid in [true, false] {
                assert_eq!(
                    knob(
                        &goose_env_knobs(provider, 4096),
                        "GOOSE_MAX_EMPTY_TURN_RETRIES"
                    )
                    .as_deref(),
                    Some("0"),
                    "{provider} hybrid={hybrid}"
                );
            }
        }
    }

    #[test]
    fn empty_turn_steer_is_non_empty_and_distinct_from_the_user_text() {
        let user_text = "any news on the expressway toll?";
        let steered = format!("{user_text}\n\n{EMPTY_TURN_STEER}");
        assert_ne!(steered, user_text);
        assert!(
            steered.starts_with(user_text),
            "the original ask must survive"
        );
        assert!(!EMPTY_TURN_STEER.trim().is_empty());
    }

    /// Matched verbatim, so upstream rewording would silently disable recovery.
    #[test]
    fn goose_empty_turn_sentinel_is_pinned() {
        assert_eq!(
            GOOSE_EMPTY_TURN_MESSAGE,
            "The model returned an empty response. Please resend your message to continue."
        );
        assert_ne!(GOOSE_EMPTY_TURN_MESSAGE, EMPTY_TURN_EXHAUSTED_MESSAGE);
    }

    /// Verbatim from a mesh-borrowed model that echoed the wrapper tag instead of answering.
    #[test]
    fn a_leaked_answer_contract_tag_is_recognised() {
        assert!(looks_like_leaked_scaffold(
            "<answer-contract> When is today? </answer-contract>"
        ));
    }

    #[test]
    fn every_injected_scaffold_tag_is_recognised() {
        for tag in SCAFFOLD_TAGS {
            let leaked = format!("some preamble {tag} and more");
            assert!(looks_like_leaked_scaffold(&leaked), "tag: {tag}");
        }
    }

    /// Only GIAP's bracketed tag names trip this, not the same words in prose.
    #[test]
    fn an_ordinary_answer_is_not_mistaken_for_leaked_scaffolding() {
        assert!(!looks_like_leaked_scaffold(
            "The sky is blue on a clear day."
        ));
        assert!(!looks_like_leaked_scaffold(
            "Today's context and your message history look fine."
        ));
    }

    /// The knob set doubles as the change signature that gates `set_var`.
    #[test]
    fn knob_signature_changes_only_when_a_setting_changes() {
        let sig = |p, ctx| {
            goose_env_knobs(p, ctx)
                .iter()
                .map(|(k, v)| format!("{k}={}", v.as_deref().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(";")
        };
        assert_eq!(sig("local", 4096), sig("local", 4096));
        assert_ne!(sig("local", 4096), sig("local", 8192));
        assert_ne!(sig("local", 4096), sig("ollama", 4096));
        // No hybrid-compaction axis: it governs no knob.
    }

    fn tool_response_message(id: &str, body: &str) -> goose::conversation::message::Message {
        goose::conversation::message::Message::user().with_tool_response(
            id,
            Ok(rmcp::model::CallToolResult::success(vec![
                rmcp::model::Content::text(body.to_string()),
            ])),
        )
    }

    #[test]
    fn quant_spelling_collapses_to_display_stem() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("gemma-4-E2B-it-Q4_K_M.gguf"), b"gguf").unwrap();
        assert_eq!(
            canonical_model_stem("gemma-4-E2B-it-Q4_K_M", tmp.path()),
            "gemma-4-E2B-it"
        );
        // The display spelling is already canonical.
        assert_eq!(
            canonical_model_stem("gemma-4-E2B-it", tmp.path()),
            "gemma-4-E2B-it"
        );
        // Both spellings now share one registry id.
    }

    #[test]
    fn explicit_quant_pin_keeps_its_identity_when_ambiguous() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("gemma-4-E4B-it-Q4_K_M.gguf"), b"gguf").unwrap();
        std::fs::write(tmp.path().join("gemma-4-E4B-it-Q4_K_S.gguf"), b"gguf").unwrap();
        // The stem resolves to Q4_K_M (lexicographic), so a Q4_K_S pin keeps its own id.
        assert_eq!(
            canonical_model_stem("gemma-4-E4B-it-Q4_K_S", tmp.path()),
            "gemma-4-E4B-it-Q4_K_S"
        );
        // The matching pin collapses.
        assert_eq!(
            canonical_model_stem("gemma-4-E4B-it-Q4_K_M", tmp.path()),
            "gemma-4-E4B-it"
        );
    }

    /// The pond's QAT weights are spelled `...-qat-UD-Q4_K_XL`; one file must get one id.
    #[test]
    fn an_unsloth_dynamic_quant_tag_is_one_tag() {
        assert!(looks_like_quant_tag("UD-Q4_K_XL"));
        assert!(looks_like_quant_tag("UD-IQ4_XS"));
        // `UD` alone is not a quant, and the prefix must not rescue a name continuation.
        assert!(!looks_like_quant_tag("UD"));
        assert!(!looks_like_quant_tag("UD-it"));

        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf");
        touch(tmp.path(), "gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf");
        for size in ["E2B", "E4B"] {
            let full = format!("gemma-4-{size}-it-qat-UD-Q4_K_XL");
            let stem = format!("gemma-4-{size}-it-qat");
            assert_eq!(
                canonical_model_stem(&full, tmp.path()),
                stem,
                "the whole compound tag must come off, leaving one id per file"
            );
            // And the collapsed stem still finds its file, or the collapse would strand it.
            assert_eq!(
                resolve_gguf_filename(&stem, tmp.path()),
                format!("{full}.gguf")
            );
        }
        // Siblings do not cross-resolve.
        assert_eq!(
            resolve_gguf_filename("gemma-4-E2B-it-qat", tmp.path()),
            "gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf"
        );
    }

    #[test]
    fn missing_file_and_non_quant_tails_are_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            canonical_model_stem("gemma-4-E2B-it-Q4_K_M", tmp.path()),
            "gemma-4-E2B-it-Q4_K_M"
        );
        assert_eq!(
            canonical_model_stem("llama-3.2-3b-instruct", tmp.path()),
            "llama-3.2-3b-instruct"
        );
    }

    fn touch(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), b"gguf").unwrap();
    }

    #[test]
    fn exact_match_is_preferred() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "gemma-4-E2B-it.gguf");
        touch(tmp.path(), "gemma-4-E2B-it-Q4_K_M.gguf");
        assert_eq!(
            resolve_gguf_filename("gemma-4-E2B-it", tmp.path()),
            "gemma-4-E2B-it.gguf"
        );
    }

    #[test]
    fn display_name_resolves_to_its_quant_file() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "gemma-4-E2B-it-Q4_K_M.gguf");
        assert_eq!(
            resolve_gguf_filename("gemma-4-E2B-it", tmp.path()),
            "gemma-4-E2B-it-Q4_K_M.gguf"
        );
    }

    #[test]
    fn an_explicit_gguf_name_is_taken_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_gguf_filename("whatever-Q8_0.gguf", tmp.path()),
            "whatever-Q8_0.gguf"
        );
    }

    #[test]
    fn a_bare_prefix_does_not_match() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "gemma-4-E2B-it-Q4_K_M.gguf");
        // The -it- file is a different model, so fall back to the naive name.
        assert_eq!(
            resolve_gguf_filename("gemma-4-E2B", tmp.path()),
            "gemma-4-E2B.gguf"
        );
    }

    #[test]
    fn missing_file_falls_back_to_the_naive_name() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_gguf_filename("not-installed", tmp.path()),
            "not-installed.gguf"
        );
    }

    #[test]
    fn variant_choice_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "gemma-4-E4B-it-Q4_K_S.gguf");
        touch(tmp.path(), "gemma-4-E4B-it-Q4_K_M.gguf");
        // Lexicographically first: ...Q4_K_M before ...Q4_K_S.
        assert_eq!(
            resolve_gguf_filename("gemma-4-E4B-it", tmp.path()),
            "gemma-4-E4B-it-Q4_K_M.gguf"
        );
    }

    #[test]
    fn quant_tags_are_told_apart_from_name_continuations() {
        for q in [
            "Q4_K_M", "Q6_K", "Q8_0", "Q4_0", "IQ4_XS", "F16", "F32", "BF16",
        ] {
            assert!(looks_like_quant_tag(q), "{q} should read as a quant tag");
        }
        for not in ["it", "instruct", "it-Q4_K_M", "chat", ""] {
            assert!(!looks_like_quant_tag(not), "{not} is not a quant tag");
        }
    }

    #[test]
    fn sibling_models_do_not_cross_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        touch(tmp.path(), "gemma-4-E2B-it-Q4_K_M.gguf");
        touch(tmp.path(), "gemma-4-E4B-it-Q4_K_M.gguf");
        assert_eq!(
            resolve_gguf_filename("gemma-4-E2B-it", tmp.path()),
            "gemma-4-E2B-it-Q4_K_M.gguf"
        );
        assert_eq!(
            resolve_gguf_filename("gemma-4-E4B-it", tmp.path()),
            "gemma-4-E4B-it-Q4_K_M.gguf"
        );
    }

    #[tokio::test]
    #[ignore = "requires llamafile at http://127.0.0.1:8080"]
    async fn test_goose_adapter_chat_stream_live() {
        let adapter = GooseAdapter::with_llamafile(None).await.unwrap();
        let request = AgentRequest {
            message: "Say hello and nothing else".to_string(),
            session_id: "test-session".to_string(),
            model_role: "chat".to_string(),
            images: Vec::new(),
            voice_mode: false,
            canvas_mode: false,
            profile_scope: ProfileScope::Household,
            profile_context: None,
            tool_group_allowlist: None,
            warmup: false,
        };

        let mut stream = adapter.chat_stream(request).await.unwrap();
        let mut saw_text = false;

        while let Some(event_result) = stream.next().await {
            let event = event_result.unwrap();
            match event {
                AgentStreamEvent::Text { .. } => saw_text = true,
                AgentStreamEvent::Done { .. } => break,
                _ => {}
            }
        }
        assert!(saw_text);
    }
}
