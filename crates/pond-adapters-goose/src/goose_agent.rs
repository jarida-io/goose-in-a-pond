use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use goose::agents::{Agent as GooseAgent, AgentConfig, ExtensionConfig, GoosePlatform};
use goose::config::GooseMode;
use goose::conversation::message::Message;
use goose::providers::base::Provider;
use goose::session::SessionManager;
use pond_core::mcp::ports::tools::tool_registry::ToolRegistryPort;
use pond_core::models::ports::agent::{
    Agent as AgentPort, AgentRequest, AgentResponse, AgentStreamEvent,
};
use pond_core::models::services::prompt_builder::build_prompt_partition;
use pond_core::prompts::PromptState;
use pond_core::user_data::ports::device_registry::DeviceRegistry;
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

/// Minimal hard-coded fallback — used only when the DB has no template for the
/// current `prompt_style`. Not a full system prompt: just enough to be safe.
const FALLBACK_PROMPT: &str = "You are {{assistant_name}}, a privacy-first local AI copilot. \
     No data leaves this device. Be concise and practical. \
     Help with everyday tasks, research, writing, coding, and home control. \
     No Markdown. Never emit pipeline control tokens. \
     IMPORTANT: Only use tools listed in your schema. \
     Never use shell, bash, python, curl, or any execution tool. \
     If a service is unavailable, tell the user directly.\n\n\
     ## Tools\n\
     You have tools for weather, scheduling, memory, device management, knowledge lookup, \
     and system operations. Tool schemas describe each one. Use them when the user's request \
     matches — do not guess answers that tools could provide accurately.\n\
     When unsure about something, check your tools first. No matching tool? Tell the user honestly.\n\n\
     ## Memory\n\
     When the user shares personal information, save it immediately with save_memory. \
     Check recall_memories before knowledge lookups. \
     Corrections are highest priority.\n\n\
     ## Output Quality\n\
     Never fabricate URLs, statistics, dates, or quotes. Use a tool or say you don't know. \
     Keep responses concise. Synthesize tool results — do not parrot raw output.\n\n\
     ## Per-Turn Context\n\
     User messages use XML tags: <system-context> has date/time and <memories>. \
     <user-message> has the actual request. Only respond to <user-message>.";

/// Adapter: GooseAdapter
///
/// Full-capability implementation of the `Agent` port using the Goose framework.
///
/// On every `chat()` call the adapter:
/// 1. Loads `Settings` from DB and fetches the active prompt template.
/// 2. Calls `agent.override_system_prompt()` with the rendered GIAP prompt.
/// 3. Injects active `PromptExtra` records and user `Skill` content as keyed extras.
/// 4. Optionally injects recent memory fragments when `agent_memory_inject = true`.
/// 5. Hot-swaps the Goose provider when `chat_provider` / `chat_model` changes.
/// 6. Auto-loads the `"giap-*"` builtin MCP extensions (once per session).
/// 7. Runs Goose's full agentic loop and returns aggregated text + tool-call metadata.
pub struct GooseAdapter {
    agent: Arc<GooseAgent>,
    session_manager: Arc<SessionManager>,
    settings_repo: Arc<dyn SettingsRepository>,
    template_repo: Arc<dyn PromptTemplateRepository>,
    extras_repo: Arc<dyn PromptExtraRepository>,
    skill_repo: Arc<dyn UserSkillRepository>,
    memory_repo: Arc<dyn MemoryRepository>,
    /// Device registry — queried per turn to populate PromptState for Jinja2 rendering.
    device_repo: Arc<dyn DeviceRegistry>,
    llamafile_url: String,
    /// GIAP data directory — used to resolve GGUF model paths under
    /// `$data_dir/models/gguf/` for the in-process LocalInferenceProvider.
    data_dir: Option<PathBuf>,
    /// Shared manager for extensions.
    extension_manager: Arc<GiapGooseExtensionManager>,
    /// Tracks the last "chat_provider:chat_model" key we wired into Goose.
    last_provider_key: Mutex<String>,
    /// Maps GIAP session IDs → Goose session IDs (Goose auto-generates its own IDs).
    goose_session_map: Mutex<HashMap<String, String>>,
    /// Goose sessions that have already had GIAP builtin extensions loaded.
    /// Extensions are loaded once per session on first use.
    loaded_sessions: Mutex<HashSet<String>>,
    /// Dynamic tool registry — provides tool descriptions for the system prompt.
    /// When `None`, falls back to the static `giap_tool_description_lines()`.
    tool_registry: Option<Arc<dyn ToolRegistryPort>>,
    /// Tracks which extensions the user explicitly added via the REST API.
    /// These are preserved across turns (not stripped in the extension cleanup loop).
    user_extensions: Arc<tokio::sync::RwLock<HashSet<String>>>,
    /// When true, prompt templates include voice-mode instructions (keep responses
    /// short, conversational, no formatting). Set by the CLI when `--input whisper`.
    voice_mode: std::sync::atomic::AtomicBool,
    /// Runtime capabilities of the currently loaded model.
    model_capabilities: Mutex<pond_core::models::domain::model_capabilities::ModelCapabilities>,
    /// Hash of the last static prefix sent via `override_system_prompt()`.
    /// When the current partition's `prefix_hash` matches this value, the static
    /// prefix has not changed and we skip `override_system_prompt()` — allowing
    /// local inference providers to reuse their KV-cache for the stable portion.
    last_prefix_hash: Mutex<u64>,
    /// Cached tool set from the last list_tools() call. Invalidated when
    /// extensions are added/removed. Avoids re-querying all MCP servers every turn.
    cached_tools: tokio::sync::RwLock<Option<std::collections::HashSet<String>>>,
    /// Whether the Goose default extensions have been stripped for this session.
    /// Only needs to happen once, not every turn.
    defaults_stripped: Mutex<HashSet<String>>,
}

impl GooseAdapter {
    /// Primary factory — all repos are injected by `pond-server/main.rs`.
    pub async fn new(
        settings_repo: Arc<dyn SettingsRepository>,
        template_repo: Arc<dyn PromptTemplateRepository>,
        extras_repo: Arc<dyn PromptExtraRepository>,
        skill_repo: Arc<dyn UserSkillRepository>,
        memory_repo: Arc<dyn MemoryRepository>,
        device_repo: Arc<dyn DeviceRegistry>,
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
            false,
            GoosePlatform::GooseCli,
        );

        let agent = Arc::new(GooseAgent::with_config(config));

        // Ensure a Goose session exists for extension management.
        // Extensions are added/removed on this session; chat sessions inherit them.
        // Try to reuse an existing session, or create a new one.
        let ext_session_id = {
            let existing = session_manager.list_sessions().await.unwrap_or_default();
            if let Some(session) = existing.first() {
                session.id.clone()
            } else {
                match session_manager
                    .create_session(
                        std::env::current_dir().unwrap_or_default(),
                        "giap-extensions".to_string(),
                        goose::session::session_manager::SessionType::User,
                        GooseMode::Auto,
                    )
                    .await
                {
                    Ok(session) => session.id,
                    Err(e) => {
                        tracing::warn!("Failed to create extension session: {e}");
                        "giap-extensions".to_string()
                    }
                }
            }
        };
        tracing::info!("Extension manager bound to session: {ext_session_id}");

        let extension_manager = Arc::new(GiapGooseExtensionManager::new(
            agent.clone(),
            ext_session_id,
        ));

        Ok(Self {
            agent,
            session_manager,
            settings_repo,
            template_repo,
            extras_repo,
            skill_repo,
            memory_repo,
            device_repo,
            llamafile_url,
            data_dir,
            extension_manager,
            last_provider_key: Mutex::new(String::new()),
            goose_session_map: Mutex::new(HashMap::new()),
            loaded_sessions: Mutex::new(HashSet::new()),
            tool_registry,
            user_extensions: Arc::new(tokio::sync::RwLock::new(HashSet::new())),
            voice_mode: std::sync::atomic::AtomicBool::new(false),
            model_capabilities: Mutex::new(
                pond_core::models::domain::model_capabilities::ModelCapabilities::default(),
            ),
            last_prefix_hash: Mutex::new(0),
            cached_tools: tokio::sync::RwLock::new(None),
            defaults_stripped: Mutex::new(HashSet::new()),
        })
    }

    /// Enable voice mode — prompt templates will include instructions for
    /// short, conversational, TTS-friendly responses.
    pub fn set_voice_mode(&self, enabled: bool) {
        self.voice_mode
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Convenience factory for non-server use (tests, CLI one-shots).
    /// Uses mock repos and connects to llamafile at `host`.
    pub async fn with_llamafile(host: Option<&str>) -> Result<Self> {
        use pond_core::user_data::mocks::mock_device_registry::MockDeviceRegistry;
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
            Arc::new(MockDeviceRegistry),
            url,
            None,
            None, // tool_registry — falls back to static giap_tool_description_lines()
        )
        .await
    }

    /// Returns an `GiapGooseExtensionManager` for managing Goose extensions on a session.
    pub fn extension_manager(&self) -> Arc<GiapGooseExtensionManager> {
        self.extension_manager.clone()
    }

    /// Returns the dynamic tool registry, if one was injected.
    pub fn tool_registry(&self) -> Option<Arc<dyn ToolRegistryPort>> {
        self.tool_registry.clone()
    }

    /// Mark an extension name as user-added so it survives the per-turn extension strip.
    pub async fn track_user_extension(&self, name: &str) {
        self.user_extensions.write().await.insert(name.to_string());
        // Invalidate tool cache — new extension means new tools available.
        *self.cached_tools.write().await = None;
    }

    /// Remove an extension from the user-tracking set.
    pub async fn untrack_user_extension(&self, name: &str) {
        self.user_extensions.write().await.remove(name);
        // Invalidate tool cache — removed extension means tools changed.
        *self.cached_tools.write().await = None;
    }

    /// Add a named builtin extension to a Goose session (idempotent).
    pub async fn add_builtin_extension(&self, name: &str, session_id: &str) -> Result<()> {
        let config = ExtensionConfig::Builtin {
            name: name.to_string(),
            description: String::new(),
            display_name: None,
            timeout: Some(600),
            bundled: Some(false),
            available_tools: vec![],
        };

        // Also register it with the extension manager so it can be re-enabled if disabled
        self.extension_manager
            .register_config(name.to_string(), config.clone())
            .await;

        self.agent
            .add_extension(config, session_id)
            .await
            .map_err(|e| anyhow!("Failed to add builtin extension '{}': {}", name, e))
    }

    /// Resolve (and create if needed) the Goose-internal session for a given GIAP session ID.
    ///
    /// Goose uses its own SQLite sessions.db with auto-generated IDs (`YYYYMMDD_N`).
    /// A GIAP session ID (UUID or arbitrary string) won't exist there unless we create it.
    /// Returns the Goose session ID to use for all subsequent `agent.*` calls.
    async fn resolve_goose_session(&self, giap_sid: &str) -> String {
        // Fast path: already mapped this session.
        if let Some(gid) = self
            .goose_session_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(giap_sid)
            .cloned()
        {
            return gid;
        }
        // Try using the GIAP session_id as-is (e.g. if Goose already stored it).
        if self
            .session_manager
            .get_session(giap_sid, false)
            .await
            .is_ok()
        {
            self.goose_session_map
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(giap_sid.to_string(), giap_sid.to_string());
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
                self.goose_session_map
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(giap_sid.to_string(), gid.clone());
                gid
            }
            Err(e) => {
                tracing::warn!("Failed to create Goose session for '{}': {e}", giap_sid);
                giap_sid.to_string()
            }
        }
    }

    /// Determine the context limit for Goose's compaction logic.
    ///
    /// For local/GGUF: returns a generous ceiling. The actual KV-cache allocation
    /// is dynamically sized per-request by `estimate_max_context_for_memory()`
    /// inside Goose's inference engine, based on available RAM and the model's
    /// KV cache cost per token. This value just prevents Goose from targeting
    /// its default 128K compaction threshold (unreachable on local models).
    ///
    /// For HTTP providers (Ollama, llamafile): uses the model's reported context
    /// window from capabilities (model name heuristics).
    fn effective_context_window(provider: &str, model: &str) -> usize {
        match provider {
            "local" | "gguf" => {
                // Generous ceiling — the actual allocation is constrained by
                // available memory at inference time, not this value.
                // Jetson (8GB): memory estimation yields ~3-6K depending on model.
                // macOS M4 (18GB): yields ~16-40K depending on model.
                #[cfg(feature = "cuda")]
                {
                    8192 // conservative ceiling for 8GB Jetson
                }
                #[cfg(not(feature = "cuda"))]
                {
                    32768 // generous ceiling — memory estimation constrains further
                }
            }
            _ => {
                // HTTP providers — use model-reported context window.
                let caps =
                    pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
                        model,
                    );
                caps.context_window_tokens as usize
            }
        }
    }

    /// Hot-swap the Goose provider when `chat_provider` / `chat_model` in settings changes.
    async fn ensure_provider_current(
        &self,
        settings: &pond_core::user_data::domain::settings::Settings,
        session_id: &str,
    ) -> Result<()> {
        let key = format!("{}:{}", settings.chat_provider, settings.chat_model);
        {
            let last = self
                .last_provider_key
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if *last == key {
                println!("[model-switch] provider already current: {}", key);
                return Ok(());
            }
            println!(
                "[model-switch] provider change detected: {:?} -> {}",
                *last, key
            );
        }

        // ── Sync GOOSE_CONTEXT_LIMIT with the actual KV-cache / provider limit ─
        //
        // Critical fix: without this, Goose's ModelConfig defaults context_limit
        // to 128K. Its auto-compaction triggers at ~80% of that (102K tokens),
        // but the local llama-cpp KV cache is only 8K (macOS) or 3K (Jetson).
        // The model hits ContextLengthExceeded long before 102K and falls into
        // the expensive emergency compaction path. Setting this env var BEFORE
        // ModelConfig::new_or_fail() ensures Goose sees the real limit.
        let effective_ctx =
            Self::effective_context_window(&settings.chat_provider, &settings.chat_model);
        // SAFETY: set_var is unsafe in multi-threaded programs per Rust 1.66+,
        // but Goose already calls set_var for OLLAMA_HOST/OLLAMA_TIMEOUT in the
        // same code path, so we follow the existing pattern.
        #[allow(unused_unsafe)]
        unsafe {
            std::env::set_var("GOOSE_CONTEXT_LIMIT", effective_ctx.to_string());
        }
        tracing::info!(
            provider = %settings.chat_provider,
            model = %settings.chat_model,
            effective_ctx,
            "Set GOOSE_CONTEXT_LIMIT to match actual KV-cache / provider limit"
        );

        let provider: Option<Arc<dyn Provider>> = match settings.chat_provider.as_str() {
            // In-process GGUF inference via llama.cpp — no HTTP server needed.
            // Registers the model in Goose's local_model_registry so
            // LocalInferenceProvider can locate the .gguf file on disk.
            "local" | "gguf" => {
                let model_name = if settings.chat_model.is_empty() {
                    "llamafile".to_string()
                } else {
                    settings.chat_model.clone()
                };
                // Register the GGUF model path in Goose's global registry
                if let Some(ref dd) = self.data_dir {
                    Self::register_gguf_model(&model_name, dd);
                }
                // Registry key is the stem (no ".gguf") — ModelConfig must match.
                let registry_key = model_name.trim_end_matches(".gguf");
                let cfg = goose::model::ModelConfig::new_or_fail(registry_key);
                println!(
                    "[model-switch] building LocalInferenceProvider for '{}'...",
                    model_name
                );
                match goose::providers::local_inference::LocalInferenceProvider::from_env(
                    cfg,
                    vec![],
                )
                .await
                {
                    Ok(p) => {
                        println!(
                            "[model-switch] LocalInferenceProvider ready for '{}'",
                            model_name
                        );
                        tracing::info!("Built LocalInferenceProvider for model '{}'", model_name);
                        Some(Arc::new(p))
                    }
                    Err(e) => {
                        println!(
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
                let cfg = goose::model::ModelConfig::new_or_fail(&model_name);
                println!(
                    "[model-switch] building llamafile OllamaProvider for '{}'...",
                    model_name
                );
                match goose::providers::ollama::OllamaProvider::from_env(cfg).await {
                    Ok(p) => {
                        println!(
                            "[model-switch] llamafile provider ready for '{}'",
                            model_name
                        );
                        Some(Arc::new(p))
                    }
                    Err(e) => {
                        println!(
                            "[model-switch] FAILED to build llamafile provider for '{}': {e}",
                            model_name
                        );
                        tracing::warn!("Failed to build llamafile provider: {e}");
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
                println!(
                    "[model-switch] building Ollama provider for '{}'...",
                    model_name
                );
                let cfg = goose::model::ModelConfig::new_or_fail(&model_name);
                match goose::providers::ollama::OllamaProvider::from_env(cfg).await {
                    Ok(p) => {
                        println!("[model-switch] Ollama provider ready for '{}'", model_name);
                        Some(Arc::new(p))
                    }
                    Err(e) => {
                        println!(
                            "[model-switch] FAILED to build Ollama provider for '{}': {e}",
                            model_name
                        );
                        tracing::warn!("Failed to build ollama provider: {e}");
                        None
                    }
                }
            }
            _ => {
                println!(
                    "[model-switch] unknown provider '{}', keeping current",
                    settings.chat_provider
                );
                None
            }
        };

        if let Some(p) = provider {
            println!(
                "[model-switch] swapping Goose provider to {}:{} for session {}",
                settings.chat_provider, settings.chat_model, session_id
            );
            tracing::info!(
                target: "giap::trace",
                kind = "provider_swap",
                session_id = %session_id,
                provider = %settings.chat_provider,
                model = %settings.chat_model,
                "Switching Goose provider"
            );
            self.agent.update_provider(p, session_id).await?;
            *self
                .last_provider_key
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = key.clone();

            // Update model capabilities from the new model name
            let caps =
                pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
                    &settings.chat_model,
                );
            println!(
                "[model-switch] capabilities: thinking={}, vision={}, context={}k",
                caps.thinking,
                caps.vision,
                caps.context_window_tokens / 1000
            );
            *self
                .model_capabilities
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = caps;

            // Reset prefix hash so the system prompt is rebuilt with the new model's
            // capabilities on the next turn. KV-cache is invalidated by the provider
            // swap anyway — no cache to preserve.
            *self
                .last_prefix_hash
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = 0;

            println!("[model-switch] swap complete, key={}", key);
        } else {
            println!(
                "[model-switch] no provider built for {}:{}",
                settings.chat_provider, settings.chat_model
            );
        }
        Ok(())
    }

    /// Register a GGUF model in Goose's global `local_model_registry` so that
    /// `LocalInferenceProvider` can find the file at `$data_dir/models/gguf/`.
    ///
    /// Handles two formats:
    /// - Bare stem: `"qwen2.5-3b-instruct-q4_k_m"` → looks for `{stem}.gguf`
    /// - Raw filename: `"model.gguf"` → uses as-is
    ///
    /// Idempotent: skips registration if the model is already known.
    fn register_gguf_model(model_name: &str, data_dir: &std::path::Path) {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, LocalModelEntry, ModelSettings,
        };

        let gguf_dir = data_dir.join("models").join("gguf");

        // Derive filename and registry key from the model name
        let (stem, filename) = if model_name.ends_with(".gguf") {
            let s = model_name.trim_end_matches(".gguf").to_string();
            (s, model_name.to_string())
        } else {
            (model_name.to_string(), format!("{}.gguf", model_name))
        };

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
                if !registry.has_model(&stem) {
                    let mut settings = ModelSettings::default();
                    settings.native_tool_calling = true;
                    settings.use_jinja = true;
                    let entry = LocalModelEntry {
                        id: stem.clone(),
                        repo_id: format!("local/{}", stem),
                        filename: filename.clone(),
                        quantization: String::new(),
                        local_path,
                        source_url: String::new(),
                        settings,
                        size_bytes: 0,
                        mmproj_path: None,
                        mmproj_source_url: None,
                        mmproj_size_bytes: 0,
                        shard_files: vec![],
                    };
                    match registry.add_model(entry) {
                        Ok(_) => {
                            tracing::info!("Registered GGUF model '{}' in local registry", stem)
                        }
                        Err(e) => tracing::warn!("Could not register GGUF model '{}': {}", stem, e),
                    }
                } else if let Some(entry) = registry.get_model(&stem) {
                    let mut s = entry.settings.clone();
                    if !s.native_tool_calling {
                        s.native_tool_calling = true;
                        let _ = registry.update_model_settings(&stem, s);
                    }
                }
            }
            Err(e) => tracing::warn!("GGUF registry lock poisoned: {}", e),
        }
    }

    pub async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<AgentStreamEvent>>> {
        let settings = self.settings_repo.get().await.unwrap_or_default();
        let session_id = request.session_id.clone();
        let model_role = request.model_role.clone();

        // Stash the user message and session ID so MCP tool handlers can read
        // them for ToolCaller param generation and outbound HTTP trace events.
        pond_mcp_server::set_last_user_message(&request.message);
        pond_mcp_server::set_current_session_id(&session_id);

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
                println!(
                    "[goose-adapter] Loading {} GIAP extensions into session {}",
                    extensions.len(),
                    goose_sid
                );
                for ext_name in extensions {
                    println!("[goose-adapter] Loading extension: {ext_name}");
                    match self.add_builtin_extension(ext_name, &goose_sid).await {
                        Ok(()) => println!("[goose-adapter]   ✓ {ext_name} loaded"),
                        Err(e) => println!("[goose-adapter]   ✗ {ext_name} FAILED: {e}"),
                    }
                }
                self.loaded_sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(goose_sid.clone());

                // List discovered tools to verify extensions are working
                let tools = self.agent.list_tools(&goose_sid, None).await;
                println!("[goose-adapter] Discovered {} tools:", tools.len());
                for t in &tools {
                    println!("[goose-adapter]   - {}", t.name);
                }
            }
        }

        // ── 1-4. System prompt, extras, skills, memory — fetched in parallel ─
        let memory_limit = if settings.agent_memory_inject {
            Some(settings.agent_memory_limit as usize)
        } else {
            None
        };

        // Extract keywords from user message for relevance-based memory search.
        // Simple approach: split on whitespace, keep words ≥3 chars, lowercase.
        let memory_keywords: Vec<String> = request
            .message
            .split_whitespace()
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .filter(|w| w.len() >= 3)
            .collect();

        let (
            template_result,
            devices_result,
            extras_result,
            skills_result,
            recent_memories,
            relevant_memories,
        ) = tokio::join!(
            self.template_repo.get(&settings.prompt_style),
            self.device_repo.list_devices(),
            self.extras_repo.list_active(),
            self.skill_repo.list_active(),
            // Recent memories (recency-based)
            async {
                match memory_limit {
                    Some(limit) => self.memory_repo.search_recent(None, limit).await,
                    None => Ok(vec![]),
                }
            },
            // Relevant memories (content keyword match — surfaces old but topical memories)
            async {
                match memory_limit {
                    Some(limit) if !memory_keywords.is_empty() => {
                        self.memory_repo
                            .search_by_content(&memory_keywords, None, limit)
                            .await
                    }
                    _ => Ok(vec![]),
                }
            },
        );

        // Merge recent + relevant, deduplicate by ID
        let memories_result: Result<Vec<pond_core::user_data::domain::memory::MemoryFragment>> = {
            let mut merged = recent_memories.unwrap_or_default();
            let relevant = relevant_memories.unwrap_or_default();
            let seen: std::collections::HashSet<String> =
                merged.iter().map(|m| m.id.clone()).collect();
            for m in relevant {
                if !seen.contains(&m.id) {
                    merged.push(m);
                }
            }
            Ok(merged)
        };

        let template_content = template_result
            .ok()
            .flatten()
            .map(|t| t.content)
            .unwrap_or_else(|| FALLBACK_PROMPT.to_string());

        let prompt_state = {
            use chrono::Local;
            let now = Local::now();
            let devices = devices_result.unwrap_or_default();
            let device_count = devices.len();
            let has_home_devices = device_count > 0;
            let online_device_names = devices
                .iter()
                .filter(|d| d.is_online)
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            // Resolve thinking mode from settings + capabilities.
            // Voice mode always disables thinking — reasoning tokens waste TTS
            // time and leak as spoken text if any filter layer misses them.
            // Check both the instance-level flag (CLI --input whisper) and the
            // per-request flag (desktop voice pipeline sends voice_mode: true).
            let is_voice =
                self.voice_mode.load(std::sync::atomic::Ordering::Relaxed) || request.voice_mode;
            let caps = self
                .model_capabilities
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let thinking_enabled = if is_voice {
                false
            } else {
                match settings.thinking_mode.as_str() {
                    "on" => true,
                    "off" => false,
                    _ => caps.thinking, // "auto" — enable when model supports it
                }
            };

            // Derive compact_prompt from the effective context window.
            // On small-context platforms (Jetson 3K, macOS Metal 8K), verbose
            // tool descriptions and detailed instructions waste precious tokens.
            let effective_ctx =
                Self::effective_context_window(&settings.chat_provider, &settings.chat_model);
            let compact_prompt =
                pond_core::models::services::context_budget::CompactionProfile::from_context_window(
                    effective_ctx,
                )
                .use_compact_prompt();

            // Tool description lines: dynamic from registry, static fallback.
            let available_tools: Vec<String> = match &self.tool_registry {
                Some(registry) => registry.prompt_description_lines(compact_prompt).await,
                None => pond_core::prompts::giap_tool_description_lines().to_vec(),
            };

            PromptState {
                current_date: now.format("%A, %-d %B %Y").to_string(),
                current_time: now.format("%H:%M").to_string(),
                device_count,
                has_home_devices,
                online_device_names,
                voice_mode: is_voice,
                canvas_mode: request.canvas_mode,
                available_tools,
                thinking_enabled,
                compact_prompt,
                prefix_hash: None, // filled by build_prompt_partition below
            }
        };

        // Per-turn dynamic context (date/time, profile). Moved from system
        // prompt to user message to keep system+tools prefix token-stable.
        let mut dynamic_suffix_for_user_msg = String::new();

        // ── Partitioned prompt: static prefix + dynamic suffix ──────────
        // When prefix_cache_prompt is enabled (default), the system prompt is
        // split into a stable static prefix and a per-turn dynamic suffix.
        // The static prefix is only rebuilt when its hash changes (settings
        // update, device change, model switch), allowing local inference
        // providers to reuse their KV-cache for the stable portion.
        //
        // When disabled, falls back to rebuilding the full system prompt every
        // turn (legacy behavior, useful for debugging or HTTP-only providers
        // where KV-cache reuse doesn't apply).
        if settings.prefix_cache_prompt {
            let partition = build_prompt_partition(
                &settings,
                None, // ProfileContext — TODO: wire when profile port is available
                &prompt_state,
                &template_content,
            );

            // Check whether the static prefix changed. Drop the MutexGuard
            // before any `.await` to keep the future `Send`.
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
                let mut last_hash = self
                    .last_prefix_hash
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *last_hash = partition.prefix_hash;
            } else {
                tracing::debug!(
                    hash = %partition.prefix_hash,
                    "Static prefix unchanged — skipping override_system_prompt (KV-cache reuse)"
                );
            }

            // Dynamic suffix (date/time, profile) goes into <system-context> in the
            // user message — NOT the system prompt. Keeps prefix token-stable.
            dynamic_suffix_for_user_msg = partition.dynamic_suffix;
        } else {
            // Legacy path: rebuild full system prompt every turn
            let system_prompt = pond_core::prompts::build_system_prompt_from_template_full(
                &settings,
                None,
                Some(&prompt_state),
                &template_content,
            );
            self.agent.override_system_prompt(system_prompt).await;
        }

        if let Ok(extras) = extras_result {
            for extra in extras {
                self.agent
                    .extend_system_prompt(extra.key, extra.instruction)
                    .await;
            }
        }

        if let Ok(skills) = skills_result {
            for skill in skills {
                self.agent
                    .extend_system_prompt(format!("skill:{}", skill.name), skill.content)
                    .await;
            }
        }

        // ── Token-budgeted memory injection ──────────────────────────────
        // Memories go into <system-context> in the user message (not the system
        // prompt) to keep the prefix token-stable for KV cache reuse.
        let mut memory_block_for_user_msg = String::new();
        //
        // Derive a CompactionProfile from the effective context window so
        // memory injection doesn't eat into the already-tight KV cache on
        // small-context platforms (Jetson 3K, macOS Metal 8K).
        let effective_ctx =
            Self::effective_context_window(&settings.chat_provider, &settings.chat_model);
        let compaction_profile =
            pond_core::models::services::context_budget::CompactionProfile::from_context_window(
                effective_ctx,
            );

        if let Ok(mut memories) = memories_result {
            if !memories.is_empty() {
                // Sort by importance (highest first) so the most valuable
                // memories survive the budget cut.
                memories.sort_by(|a, b| {
                    b.importance
                        .partial_cmp(&a.importance)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

                // Apply fragment count limit from the compaction profile.
                memories.truncate(compaction_profile.max_memory_fragments);

                // Apply token budget: estimate tokens per fragment using the
                // chars/4 heuristic, keep fragments until the budget is spent.
                let token_budget = compaction_profile.memory_token_budget;
                let mut tokens_used: usize = 0;
                let mut budgeted: Vec<&pond_core::user_data::domain::memory::MemoryFragment> =
                    Vec::new();
                for m in &memories {
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
                        fragments_available = memories.len(),
                        tokens_used,
                        token_budget,
                        "Memory injection (budget from CompactionProfile ctx={})",
                        effective_ctx,
                    );

                    memory_block_for_user_msg = block;

                    // Record access for decay tracking — fire-and-forget in background
                    // to avoid blocking the inference hot path with sequential DB writes.
                    let ids: Vec<String> = budgeted.iter().map(|m| m.id.clone()).collect();
                    let repo = self.memory_repo.clone();
                    tokio::spawn(async move {
                        for id in ids {
                            let _ = repo.record_access(&id).await;
                        }
                    });
                }
            }
        }

        // ── 4b. Upcoming schedules context ──────────────────────────────────
        // TODO: inject schedule context once GooseAdapter has a SchedulerPort ref.
        // The old global-state path (registry.rs) has been removed.

        // ── 5. Provider hot-swap ──────────────────────────────────────────────
        if let Err(e) = self.ensure_provider_current(&settings, &goose_sid).await {
            tracing::warn!("Provider update failed (continuing with current provider): {e}");
        }

        // ── 6. Extension cleanup ──────────────────────────────────────────────
        // Strip Goose default extensions that would pollute the prompt.
        // Only do this once per session — subsequent turns skip the strip loop.
        {
            let already_stripped = self
                .defaults_stripped
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&goose_sid);
            if !already_stripped {
                let strip_list: &[&str] = &[
                    "developer",
                    "computercontroller",
                    "extensionmanager",
                    "todo",
                    "apps",
                    "analyze",
                    "summon",
                    "summarize",
                    "orchestrator",
                    "tom",
                ];
                let user_exts = self.user_extensions.read().await;
                for ext in strip_list {
                    if !user_exts.contains(*ext) {
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
        // Use cached tools when available — only re-query MCP servers when the
        // cache has been invalidated (extensions added/removed/defaults stripped).
        let allowed_tools = {
            let cache = self.cached_tools.read().await;
            if let Some(cached) = cache.as_ref() {
                cached.clone()
            } else {
                drop(cache);
                // Cache miss — query all tools and rebuild.
                let all_tools = self.agent.list_tools(&goose_sid, None).await;
                let tools_set: std::collections::HashSet<String> =
                    all_tools.iter().map(|t| t.name.to_string()).collect();

                // Group tools by extension prefix and inject external extension
                // descriptions so the agent knows about MCP tools.
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

                // Filter out built-in GIAP extensions (already covered by the
                // available_tools section in the prompt) and Goose defaults.
                let is_builtin = |name: &str| {
                    registered_extensions().iter().any(|e| e == name)
                        || matches!(
                            name,
                            "default"
                                | "developer"
                                | "computercontroller"
                                | "extensionmanager"
                                | "todo"
                                | "apps"
                                | "analyze"
                                | "summon"
                                | "summarize"
                                | "orchestrator"
                                | "tom"
                                | "suggestions"
                        )
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

                    self.agent
                        .extend_system_prompt("extensions".to_string(), ext_description)
                        .await;
                }

                *self.cached_tools.write().await = Some(tools_set.clone());
                tools_set
            }
        };

        tracing::info!(target: "pond_adapters_goose::goose_agent", "Allowed tools for turn: {:?}", allowed_tools);

        // ── 7. GooseMode from model_role ──────────────────────────────────────
        let goose_mode = GooseMode::Auto;
        self.agent
            .update_goose_mode(goose_mode, &goose_sid)
            .await
            .ok();

        // ── 8. Run the agentic loop ───────────────────────────────────────────
        // Build <system-context> block with per-turn dynamic data (date/time,
        // memories). This keeps the system prompt + tool tokens stable across
        // turns, enabling KV cache prefix reuse in the local inference engine.
        let has_context =
            !dynamic_suffix_for_user_msg.is_empty() || !memory_block_for_user_msg.is_empty();
        let user_text = {
            let mut msg = String::with_capacity(512 + request.message.len());
            if has_context {
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
                msg.push_str("</system-context>\n");
            }
            msg.push_str("<user-message>\n");
            msg.push_str(&request.message);
            msg.push_str("\n</user-message>");
            msg
        };
        let user_msg = Message::user().with_text(&user_text);
        let session_cfg = goose::agents::types::SessionConfig {
            id: goose_sid.clone(),
            schedule_id: None,
            max_turns: Some(settings.agent_max_turns as u32),
            retry_config: None,
        };

        let agent_clone = self.agent.clone();
        let session_mgr = self.session_manager.clone();
        let goose_sid_for_usage = goose_sid.clone();

        let user_msg_len = request.message.len();
        let turn_start = std::time::Instant::now();

        // Cancellation token: when the stream is dropped (e.g. voice interrupt),
        // the DropGuard fires and cancels the token.  Goose's agent loop checks
        // `is_token_cancelled()` at each turn boundary and exits early, so
        // interruption propagates faster than waiting for the channel-drop path
        // through spawn_blocking.
        let cancel_token = CancellationToken::new();
        let cancel_guard = cancel_token.clone().drop_guard();

        let stream = async_stream::stream! {
            // Hold the guard — dropped when the stream is dropped → cancels token.
            let _guard = cancel_guard;

            yield Ok(AgentStreamEvent::Status { content: "Agent working...".to_string() });
            let mut total_output_chars: usize = 0;
            // Track tool call ID → tool name so ToolResult events carry the tool name.
            let mut tool_id_to_name: HashMap<String, String> = HashMap::new();
            // Wall-clock start per tool call (keyed by Goose tool-call ID) for latency.
            let mut tool_call_starts: HashMap<String, std::time::Instant> = HashMap::new();

            tracing::info!(
                target: "giap::trace",
                kind = "turn_start",
                session_id = %session_id,
                model = %settings.chat_model,
                provider = %settings.chat_provider,
                message_len = user_msg_len,
            );

            let mut goose_stream = match agent_clone.reply(user_msg, session_cfg, Some(cancel_token)).await {
                Ok(s) => s,
                Err(e) => {
                    yield Ok(AgentStreamEvent::Error { content: e.to_string() });
                    return;
                }
            };

            while let Some(event_result) = goose_stream.next().await {
                match event_result {
                    Ok(event) => match event {
                        goose::agents::AgentEvent::Message(msg) => {
                            // Emit tool call and result events
                            for content in &msg.content {
                                match content {
                                    goose::conversation::message::MessageContent::ToolRequest(tr) => {
                                        if let Ok(tool_call) = &tr.tool_call {
                                            let tool_name = tool_call.name.to_string();
                                            // Guard: suppress tool calls not in the validated schema.
                                            // An empty allowed_tools set means no extensions loaded —
                                            // every call is a hallucination and must be blocked.
                                            if !allowed_tools.contains(&tool_name) {
                                                tracing::warn!(
                                                    tool = %tool_name,
                                                    allowed = ?allowed_tools,
                                                    "Blocked unauthorized tool call (not in schema or no tools loaded)",
                                                );
                                                continue;
                                            }
                                            tool_id_to_name.insert(tr.id.clone(), tool_name.clone());
                                            tool_call_starts.insert(tr.id.clone(), std::time::Instant::now());
                                            tracing::info!(
                                                target: "giap::trace",
                                                kind = "tool_call",
                                                session_id = %session_id,
                                                tool = %tool_name,
                                                tool_id = %tr.id,
                                            );
                                            yield Ok(AgentStreamEvent::ToolCall {
                                                id: tr.id.clone(),
                                                tool: tool_name,
                                                input: tool_call.arguments.clone().map(serde_json::Value::Object),
                                            });
                                        }
                                    }
                                    goose::conversation::message::MessageContent::ToolResponse(tr) => {
                                        if let Ok(tool_result) = &tr.tool_result {
                                            let content_text = tool_result
                                                .content
                                                .iter()
                                                .filter_map(|c| match c.deref() {
                                                    rmcp::model::RawContent::Text(t) => Some(t.text.clone()),
                                                    _ => None,
                                                })
                                                .collect::<Vec<_>>()
                                                .join("\n");

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
                                            );
                                            yield Ok(AgentStreamEvent::ToolResult {
                                                id: tr.id.clone(),
                                                tool: tool_name,
                                                content: content_text,
                                            });
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            // Emit raw text — the SSE layer's stateful ThoughtFilter
                            // handles stripping of <think>, <thought>, and
                            // <|channel>thought...<channel|> tags across chunk
                            // boundaries.  A per-chunk strip here interferes with
                            // the stateful filter (it eats close tags the filter
                            // is waiting for, causing answer text to be swallowed).
                            let raw_text = msg.as_concat_text();
                            if !raw_text.is_empty() {
                                total_output_chars += raw_text.len();
                                yield Ok(AgentStreamEvent::Text { content: raw_text });
                            }
                        }
                        goose::agents::AgentEvent::HistoryReplaced(_) => {
                            yield Ok(AgentStreamEvent::Status { content: "Compacting context...".to_string() });
                        }
                        _ => {}
                    },
                    Err(e) => {
                        yield Ok(AgentStreamEvent::Error { content: e.to_string() });
                    }
                }
            }
            // Read real token usage from Goose's session metrics (tracked by
            // the provider during inference). Fall back to chars/4 heuristic
            // if the session isn't available or counts are missing.
            let usage = match session_mgr.get_session(&goose_sid_for_usage, false).await {
                Ok(goose_session) => {
                    let input = goose_session.accumulated_input_tokens
                        .map(|t| t.max(0) as u32)
                        .unwrap_or((user_msg_len / 4).max(1) as u32);
                    let output = goose_session.accumulated_output_tokens
                        .map(|t| t.max(0) as u32)
                        .unwrap_or((total_output_chars / 4).max(1) as u32);
                    pond_core::models::ports::provider::UsageStats {
                        prompt_tokens: input,
                        completion_tokens: output,
                    }
                }
                Err(_) => pond_core::models::ports::provider::UsageStats {
                    prompt_tokens: (user_msg_len / 4).max(1) as u32,
                    completion_tokens: (total_output_chars / 4).max(1) as u32,
                },
            };
            let total_latency_ms = turn_start.elapsed().as_millis() as u64;
            tracing::info!(
                target: "giap::trace",
                kind = "turn_end",
                session_id = %session_id,
                prompt_tokens = usage.prompt_tokens,
                completion_tokens = usage.completion_tokens,
                total_latency_ms,
            );
            yield Ok(AgentStreamEvent::Done { session_id, model_role, usage: Some(usage) });
        };

        Ok(Box::pin(stream))
    }
}

#[async_trait]
impl AgentPort for GooseAdapter {
    fn capabilities(&self) -> pond_core::models::domain::model_capabilities::ModelCapabilities {
        let mut caps = self
            .model_capabilities
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // Voice mode disables expensive/leaky capabilities: thinking tokens
        // waste TTS time, vision/audio inputs aren't used in voice flow.
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

        // Parse the JSON args into the Map that rmcp expects.
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
                // ToolCallResult.result is a Future — await it to get the actual result.
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
