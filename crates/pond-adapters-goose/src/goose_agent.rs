use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use goose::agents::{Agent as GooseAgent, AgentConfig, ExtensionConfig, GoosePlatform};
use goose::config::GooseMode;
use goose::conversation::message::Message;
use goose::providers::base::Provider;
use goose::session::SessionManager;
use pond_core::ports::agent::{Agent as AgentPort, AgentRequest, AgentResponse, AgentStreamEvent};
use pond_core::ports::device_registry::DeviceRegistry;
use pond_core::ports::memory_repository::MemoryRepository;
use pond_core::ports::prompt_extra::PromptExtraRepository;
use pond_core::ports::prompt_template::PromptTemplateRepository;
use pond_core::ports::settings::SettingsRepository;
use pond_core::ports::skill::UserSkillRepository;
use pond_core::prompts::{build_system_prompt_from_template_full, PromptState};
use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::extension_manager::GiapGooseExtensionManager;

/// Strip thinking-token preambles from model output.
///
/// Handles:
/// - Gemma 4: `<|channel>thought … <channel|>ACTUAL REPLY`
/// - Qwen3 / DeepSeek-R1 / QwQ: `<think>…</think>ACTUAL REPLY`
fn strip_thinking_tokens(text: &str) -> String {
    // Gemma 4 format — everything after last <channel|>
    const CHANNEL_CLOSE: &str = "<channel|>";
    if let Some(pos) = text.rfind(CHANNEL_CLOSE) {
        let cleaned = text[pos + CHANNEL_CLOSE.len()..].trim();
        if !cleaned.is_empty() {
            return cleaned.to_string();
        }
    }

    // <think>…</think> format — strip all blocks
    if text.contains("<think>") {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        loop {
            if let Some(start) = rest.find("<think>") {
                out.push_str(&rest[..start]);
                if let Some(end) = rest[start..].find("</think>") {
                    rest = &rest[start + end + "</think>".len()..];
                } else {
                    break; // unclosed — discard tail
                }
            } else {
                out.push_str(rest);
                break;
            }
        }
        let trimmed = out.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    text.to_string()
}

/// Minimal hard-coded fallback — used only when the DB has no template for the
/// current `prompt_style`. Not a full system prompt: just enough to be safe.
const FALLBACK_PROMPT: &str =
    "You are {{assistant_name}}, a privacy-first local AI copilot. \
     No data leaves this device. Be concise and practical. \
     Help with everyday tasks, research, writing, coding, and home control. \
     No Markdown. Never emit pipeline control tokens. \
     IMPORTANT: Only use tools listed in your schema. \
     Never use shell, bash, python, curl, or any execution tool. \
     If a service is unavailable, tell the user directly.";


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
/// 6. Auto-loads the `"giap"` builtin MCP extension (once per session).
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
    /// Sessions that have already had the "giap" builtin extension loaded.
    loaded_extensions: Mutex<HashSet<String>>,
    /// Maps GIAP session IDs → Goose session IDs (Goose auto-generates its own IDs).
    goose_session_map: Mutex<HashMap<String, String>>,
    /// When true, prompt templates include voice-mode instructions (keep responses
    /// short, conversational, no formatting). Set by the CLI when `--input whisper`.
    voice_mode: std::sync::atomic::AtomicBool,
    /// Runtime capabilities of the currently loaded model.
    model_capabilities: Mutex<pond_core::domain::model_capabilities::ModelCapabilities>,
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
        // Initialize extension manager with an empty session_id (it will be updated per call or we'll need to rethink its session_id binding)
        // Actually, the ExtensionManagerPort trait doesn't take session_id, so the manager must be bound to one, or we change the trait.
        // Looking at ExtensionManagerPort, it doesn't have session_id in methods.
        // This means GIAP currently assumes a single session or the manager is per-session.
        let extension_manager = Arc::new(GiapGooseExtensionManager::new(agent.clone(), "default".to_string()));

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
            loaded_extensions: Mutex::new(HashSet::new()),
            goose_session_map: Mutex::new(HashMap::new()),
            voice_mode: std::sync::atomic::AtomicBool::new(false),
            model_capabilities: Mutex::new(pond_core::domain::model_capabilities::ModelCapabilities::default()),
        })
    }

    /// Enable voice mode — prompt templates will include instructions for
    /// short, conversational, TTS-friendly responses.
    pub fn set_voice_mode(&self, enabled: bool) {
        self.voice_mode.store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    /// Convenience factory for non-server use (tests, CLI one-shots).
    /// Uses mock repos and connects to llamafile at `host`.
    pub async fn with_llamafile(host: Option<&str>) -> Result<Self> {
        use pond_core::services::mock_device_registry::MockDeviceRegistry;
        use pond_core::services::mock_memory::MockMemoryRepository;
        use pond_core::services::mock_prompt_extra::MockPromptExtraRepository;
        use pond_core::services::mock_prompt_template::MockPromptTemplateRepository;
        use pond_core::services::mock_settings::MockSettingsRepository;
        use pond_core::services::mock_skill::MockSkillRepository;

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
        ).await
    }

    /// Returns an `GiapGooseExtensionManager` for managing Goose extensions on a session.
    pub fn extension_manager(&self) -> Arc<GiapGooseExtensionManager> {
        self.extension_manager.clone()
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
        if let Some(gid) = self.goose_session_map.lock().unwrap().get(giap_sid).cloned() {
            return gid;
        }
        // Try using the GIAP session_id as-is (e.g. if Goose already stored it).
        if self.session_manager.get_session(giap_sid, false).await.is_ok() {
            self.goose_session_map.lock().unwrap()
                .insert(giap_sid.to_string(), giap_sid.to_string());
            return giap_sid.to_string();
        }
        // Create a brand-new Goose session; use the GIAP id as the human name.
        match self.session_manager.create_session(
            std::env::current_dir().unwrap_or_default(),
            giap_sid.to_string(),
            goose::session::session_manager::SessionType::User,
            GooseMode::Auto,
        ).await {
            Ok(session) => {
                let gid = session.id.clone();
                self.goose_session_map.lock().unwrap()
                    .insert(giap_sid.to_string(), gid.clone());
                gid
            }
            Err(e) => {
                tracing::warn!("Failed to create Goose session for '{}': {e}", giap_sid);
                giap_sid.to_string()
            }
        }
    }

    /// Hot-swap the Goose provider when `chat_provider` / `chat_model` in settings changes.
    async fn ensure_provider_current(
        &self,
        settings: &pond_core::domain::settings::Settings,
        session_id: &str,
    ) -> Result<()> {
        let key = format!("{}:{}", settings.chat_provider, settings.chat_model);
        {
            let last = self.last_provider_key.lock().unwrap();
            if *last == key {
                println!("[model-switch] provider already current: {}", key);
                return Ok(());
            }
            println!("[model-switch] provider change detected: {:?} -> {}", *last, key);
        }

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
                let cfg = goose::model::ModelConfig::new_or_fail(&model_name);
                println!("[model-switch] building LocalInferenceProvider for '{}'...", model_name);
                match goose::providers::local_inference::LocalInferenceProvider::from_env(cfg, vec![]).await {
                    Ok(p) => {
                        println!("[model-switch] LocalInferenceProvider ready for '{}'", model_name);
                        tracing::info!("Built LocalInferenceProvider for model '{}'", model_name);
                        Some(Arc::new(p))
                    }
                    Err(e) => {
                        println!("[model-switch] FAILED to build LocalInferenceProvider for '{}': {e}", model_name);
                        tracing::warn!("Failed to build local inference provider for '{}': {e}", model_name);
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
                println!("[model-switch] building llamafile OllamaProvider for '{}'...", model_name);
                match goose::providers::ollama::OllamaProvider::from_env(cfg).await {
                    Ok(p) => {
                        println!("[model-switch] llamafile provider ready for '{}'", model_name);
                        Some(Arc::new(p))
                    }
                    Err(e) => {
                        println!("[model-switch] FAILED to build llamafile provider for '{}': {e}", model_name);
                        tracing::warn!("Failed to build llamafile provider: {e}");
                        None
                    }
                }
            }
            "ollama" => {
                std::env::set_var("OLLAMA_TIMEOUT", "600");
                let model_name = if settings.chat_model.is_empty() {
                    "llama3.2".to_string()
                } else {
                    settings.chat_model.clone()
                };
                println!("[model-switch] building Ollama provider for '{}'...", model_name);
                let cfg = goose::model::ModelConfig::new_or_fail(&model_name);
                match goose::providers::ollama::OllamaProvider::from_env(cfg).await {
                    Ok(p) => {
                        println!("[model-switch] Ollama provider ready for '{}'", model_name);
                        Some(Arc::new(p))
                    }
                    Err(e) => {
                        println!("[model-switch] FAILED to build Ollama provider for '{}': {e}", model_name);
                        tracing::warn!("Failed to build ollama provider: {e}");
                        None
                    }
                }
            }
            _ => {
                println!("[model-switch] unknown provider '{}', keeping current", settings.chat_provider);
                None
            }
        };

        if let Some(p) = provider {
            println!("[model-switch] swapping Goose provider to {}:{} for session {}", settings.chat_provider, settings.chat_model, session_id);
            tracing::info!(
                "Switching Goose provider to {}:{} for session {}",
                settings.chat_provider, settings.chat_model, session_id
            );
            self.agent.update_provider(p, session_id).await?;
            *self.last_provider_key.lock().unwrap() = key.clone();

            // Update model capabilities from the new model name
            let caps = pond_core::domain::model_capabilities::ModelCapabilities::from_model_name(&settings.chat_model);
            println!("[model-switch] capabilities: thinking={}, vision={}, context={}k",
                caps.thinking, caps.vision, caps.context_window_tokens / 1000);
            *self.model_capabilities.lock().unwrap() = caps;

            println!("[model-switch] swap complete, key={}", key);
        } else {
            println!("[model-switch] no provider built for {}:{}", settings.chat_provider, settings.chat_model);
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
                    let entry = LocalModelEntry {
                        id:           stem.clone(),
                        repo_id:      format!("local/{}", stem),
                        filename:     filename.clone(),
                        quantization: String::new(),
                        local_path,
                        source_url:   String::new(),
                        settings,
                        size_bytes:   0,
                    };
                    match registry.add_model(entry) {
                        Ok(_) => tracing::info!("Registered GGUF model '{}' in local registry", stem),
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

        // Goose maintains its own sessions.db with auto-generated IDs.
        let goose_sid = self.resolve_goose_session(&session_id).await;

        // ── 1-4. System prompt, extras, skills, memory — fetched in parallel ─
        let memory_limit = if settings.agent_memory_inject {
            Some(settings.agent_memory_limit as usize)
        } else {
            None
        };

        let (template_result, devices_result, extras_result, skills_result, memories_result) = tokio::join!(
            self.template_repo.get(&settings.prompt_style),
            self.device_repo.list_devices(),
            self.extras_repo.list_active(),
            self.skill_repo.list_active(),
            async {
                match memory_limit {
                    Some(limit) => self.memory_repo.search_recent(None, limit).await,
                    None => Ok(vec![]),
                }
            },
        );

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
            // Use cached tool description lines (avoids 6 format!() allocations per turn)
            let available_tools: Vec<String> = pond_core::prompts::giap_tool_description_lines()
                .to_vec();

            // Resolve thinking mode from settings + capabilities
            let caps = self.model_capabilities.lock().unwrap().clone();
            let thinking_enabled = match settings.thinking_mode.as_str() {
                "on"  => true,
                "off" => false,
                _     => caps.thinking, // "auto" — enable when model supports it
            };

            PromptState {
                current_date: now.format("%A, %-d %B %Y").to_string(),
                current_time: now.format("%H:%M").to_string(),
                device_count,
                has_home_devices,
                online_device_names,
                voice_mode: self.voice_mode.load(std::sync::atomic::Ordering::Relaxed),
                available_tools,
                thinking_enabled,
            }
        };

        let system_prompt = build_system_prompt_from_template_full(
            &settings,
            None,
            Some(&prompt_state),
            &template_content,
        );
        self.agent.override_system_prompt(system_prompt).await;

        if let Ok(extras) = extras_result {
            for extra in extras {
                self.agent.extend_system_prompt(extra.key, extra.instruction).await;
            }
        }

        if let Ok(skills) = skills_result {
            for skill in skills {
                self.agent
                    .extend_system_prompt(format!("skill:{}", skill.name), skill.content)
                    .await;
            }
        }

        if let Ok(memories) = memories_result {
            if !memories.is_empty() {
                let block = memories
                    .iter()
                    .map(|m| format!("- {}", m.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.agent
                    .extend_system_prompt(
                        "memories".to_string(),
                        format!("Relevant memories:\n{block}"),
                    )
                    .await;
            }
        }

        // ── 5. Provider hot-swap ──────────────────────────────────────────────
        if let Err(e) = self.ensure_provider_current(&settings, &goose_sid).await {
            tracing::warn!("Provider update failed (continuing with current provider): {e}");
        }

        // ── 6. Tool-free mode ─────────────────────────────────────────────────
        // GIAP tools (Wikipedia, weather, etc.) are handled by the Tool Agent
        // pre-processor in routes.rs BEFORE the main LLM runs. The Goose agent
        // operates with ZERO tools — no MCP extensions loaded, no tool schemas
        // in the prompt, no tool-call formatting required from the model.
        // This eliminates tool-call argument failures and model-swap overhead.
        //
        // Remove any extensions that may have bled in from prior sessions or
        // Goose's default config.
        for ext in &[
            "giap", "developer", "computercontroller", "extensionmanager",
            "todo", "apps", "analyze", "summon", "summarize",
            "orchestrator", "tom",
        ] {
            self.agent.remove_extension(ext, &goose_sid).await.ok();
        }

        // ── 7. GooseMode from model_role ──────────────────────────────────────
        let goose_mode = GooseMode::Auto;
        self.agent
            .update_goose_mode(goose_mode, &goose_sid)
            .await
            .ok();

        // ── 8. Run the agentic loop ───────────────────────────────────────────
        // Stash the user message so MCP tools can use it as fallback when the
        // model calls a tool with empty parameters (common with small local models).
        pond_mcp_server::set_last_user_message(&request.message).await;

        let user_msg = Message::user().with_text(&request.message);
        let session_cfg = goose::agents::types::SessionConfig {
            id: goose_sid.clone(),
            schedule_id: None,
            max_turns: Some(settings.agent_max_turns as u32),
            retry_config: None,
        };

        let agent_clone = self.agent.clone();

        // Build the set of valid tool names before the stream starts.
        // Any ToolCall event whose name isn't in this set is a hallucination
        // and must be suppressed before it reaches the UI / SSE serialiser.
        let allowed_tools: std::collections::HashSet<String> = agent_clone
            .list_tools(&goose_sid, None)
            .await
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        
        tracing::info!(target: "pond_adapters_goose::goose_agent", "Allowed tools for turn: {:?}", allowed_tools);

        let stream = async_stream::stream! {
            yield Ok(AgentStreamEvent::Status { content: "Agent working...".to_string() });

            let mut goose_stream = match agent_clone.reply(user_msg, session_cfg, None).await {
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

                                            yield Ok(AgentStreamEvent::ToolResult {
                                                id: tr.id.clone(),
                                                tool: String::new(), // Goose ToolResponse doesn't store tool name directly in new version
                                                content: content_text,
                                            });
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            // Emit text — strip <think> blocks and Gemma 4 channel tags
                            // before yielding so all consumers (CLI, HTTP SSE, voice TTS)
                            // receive clean text without internal reasoning.
                            let raw_text = msg.as_concat_text();
                            if !raw_text.is_empty() {
                                let text = strip_thinking_tokens(&raw_text);
                                if !text.is_empty() {
                                    yield Ok(AgentStreamEvent::Text { content: text });
                                }
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
            yield Ok(AgentStreamEvent::Done { session_id, model_role });
        };

        Ok(Box::pin(stream))
    }
}


#[async_trait]
impl AgentPort for GooseAdapter {
    fn capabilities(&self) -> pond_core::domain::model_capabilities::ModelCapabilities {
        self.model_capabilities.lock().unwrap().clone()
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
