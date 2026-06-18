//! PondAgent — the core agent loop with sustained tool calling.
//!
//! Implements the [`Agent`] trait from `pond-core`. The agent:
//!
//! 1. Loads settings and hot-swaps the provider if the model changed
//! 2. Builds a system prompt (via `PromptBuilder` or a simple default)
//! 3. Loads conversation history from `SessionStorage`
//! 4. Enters a streaming loop: call LLM, emit text tokens, detect tool calls
//! 5. When tool calls are detected, emits ToolCall/ToolResult events and
//!    appends results to the message history, then loops back to step 4
//! 6. Terminates when the model produces a final text answer (no tool calls)
//!    or the iteration guard fires (max 10 rounds)

use crate::history;
use crate::ollama_provider::OllamaInferenceProvider;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use pond_core::domain::agent::{AgentRequest, AgentResponse, AgentStreamEvent};
use pond_core::domain::message::ChatMessage;
use pond_core::domain::model_capabilities::ModelCapabilities;
use pond_core::domain::settings::Settings;
use pond_core::ports::agent::Agent;
use pond_core::ports::device_registry::DeviceRegistry;
use pond_core::ports::inference::{InferenceOptions, InferenceProvider, ToolDefinition};
use pond_core::ports::prompt_extra::PromptExtraRepository;
use pond_core::ports::prompt_template::PromptTemplateRepository;
use pond_core::ports::provider::UsageStats;
use pond_core::ports::session_storage::SessionStorage;
use pond_core::ports::settings::SettingsRepository;
use pond_core::ports::skill::UserSkillRepository;
use pond_core::ports::tool_dispatcher::ToolDispatcher;
use pond_core::prompts;
use pond_core::services::prompt_builder;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// Maximum tool-calling loop iterations before the agent gives up.
const MAX_TOOL_ITERATIONS: u32 = 10;

/// Maximum conversation history turns to include per request.
const HISTORY_LIMIT: usize = 30;

/// Maximum characters for the history XML block (~tokens * 4).
/// Keeps history from consuming too much of the context window.
const HISTORY_CHAR_BUDGET: usize = 4000;

/// The core GIAP agent with sustained tool-calling support.
///
/// Wraps an `InferenceProvider` (hot-swappable via `RwLock`) and executes
/// a multi-turn tool loop against it. Uses `PromptBuilder` for system
/// prompt construction when template repositories are available.
pub struct PondAgent {
    /// The active inference provider (hot-swappable at runtime).
    provider: RwLock<Arc<dyn InferenceProvider>>,
    /// Tool definitions cached from the MCP server at init time.
    tool_definitions: Vec<ToolDefinition>,
    /// Settings persistence.
    settings_repo: Arc<dyn SettingsRepository>,
    /// Prompt template persistence (optional -- falls back to built-in).
    template_repo: Option<Arc<dyn PromptTemplateRepository>>,
    /// Prompt extras persistence (optional).
    extras_repo: Option<Arc<dyn PromptExtraRepository>>,
    /// User skills persistence (optional).
    skill_repo: Option<Arc<dyn UserSkillRepository>>,
    /// Device registry for prompt context.
    device_repo: Arc<dyn DeviceRegistry>,
    /// Session message persistence (for cross-session history if needed).
    session_storage: Arc<dyn SessionStorage>,
    /// MCP tool dispatcher — routes tool calls to the correct MCP server.
    tool_dispatcher: Option<Arc<dyn ToolDispatcher>>,
    /// Tracks the current provider key for hot-swap detection.
    last_provider_key: Mutex<String>,
    /// In-memory conversation history per session (current server lifetime only).
    /// Each session_id maps to accumulated (user, assistant) message pairs.
    /// Sent to the model as `<history>` XML in the user message.
    session_history:
        Arc<tokio::sync::Mutex<std::collections::HashMap<String, Vec<(String, String)>>>>,
}

impl PondAgent {
    /// Create a new PondAgent.
    ///
    /// The `provider` is the initial inference provider. It will be
    /// hot-swapped if `settings.chat_provider:chat_model` changes.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn InferenceProvider>,
        tool_definitions: Vec<ToolDefinition>,
        settings_repo: Arc<dyn SettingsRepository>,
        template_repo: Option<Arc<dyn PromptTemplateRepository>>,
        extras_repo: Option<Arc<dyn PromptExtraRepository>>,
        skill_repo: Option<Arc<dyn UserSkillRepository>>,
        device_repo: Arc<dyn DeviceRegistry>,
        session_storage: Arc<dyn SessionStorage>,
        tool_dispatcher: Option<Arc<dyn ToolDispatcher>>,
    ) -> Self {
        tracing::info!(
            tool_count = tool_definitions.len(),
            provider = %provider.model_name(),
            tool_calling = provider.capabilities().tool_calling,
            "PondAgent initialized"
        );
        let initial_key = format!("initial:{}", provider.model_name());
        Self {
            provider: RwLock::new(provider),
            tool_definitions,
            settings_repo,
            template_repo,
            extras_repo,
            skill_repo,
            device_repo,
            session_storage,
            tool_dispatcher,
            last_provider_key: Mutex::new(initial_key),
            session_history: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Check if the provider needs to be swapped based on current settings.
    ///
    /// Compares `chat_provider:chat_model` against the last known key.
    /// If changed, creates a new `OllamaInferenceProvider` and swaps it in.
    async fn ensure_provider_current(&self, settings: &Settings) -> Result<()> {
        let key = format!("{}:{}", settings.chat_provider, settings.chat_model);

        {
            let last = self.last_provider_key.lock().await;
            if *last == key {
                return Ok(());
            }
        }

        // If the provider is not configured yet (empty string from defaults),
        // keep the injected provider as-is. The caller is responsible for
        // providing a valid initial provider.
        if settings.chat_provider.is_empty() {
            return Ok(());
        }

        let new_provider: Arc<dyn InferenceProvider> = match settings.chat_provider.as_str() {
            "ollama" => {
                let url = std::env::var("GIAP_OLLAMA_URL")
                    .unwrap_or_else(|_| "http://localhost:11434".to_string());
                Arc::new(OllamaInferenceProvider::new(&url, &settings.chat_model))
            }
            "llamafile" => {
                let url = std::env::var("GIAP_LLAMAFILE_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
                // Llamafile uses an OpenAI-compatible API, but we can use the
                // same Ollama provider structure for streaming.
                Arc::new(OllamaInferenceProvider::new(&url, &settings.chat_model))
            }
            // "local" / "gguf" providers are injected at startup via
            // build_pond_agent() or swap_provider(). Keep using whatever
            // is currently loaded — the model was already loaded at init.
            "local" | "gguf" => {
                tracing::debug!(
                    "provider '{}' is managed externally — keeping current provider",
                    settings.chat_provider
                );
                *self.last_provider_key.lock().await = key;
                return Ok(());
            }
            other => {
                return Err(anyhow!(
                    "Unsupported provider '{}'. Use 'ollama', 'llamafile', 'local', or 'gguf'.",
                    other
                ));
            }
        };

        tracing::info!(
            provider = %settings.chat_provider,
            model = %settings.chat_model,
            "Hot-swapping inference provider"
        );

        *self.provider.write().await = new_provider;
        *self.last_provider_key.lock().await = key;
        Ok(())
    }

    /// Swap the inference provider externally (e.g. from `rebuild_llm_provider`).
    ///
    /// Used when the caller creates a provider (e.g. local GGUF) that
    /// `ensure_provider_current` cannot construct internally.
    pub async fn swap_provider(&self, provider: Arc<dyn InferenceProvider>) {
        let key = format!("external:{}", provider.model_name());
        *self.provider.write().await = provider;
        *self.last_provider_key.lock().await = key;
    }

    /// Build the system prompt from settings, device state, and templates.
    ///
    /// Uses `PromptBuilder::build_prompt_partition()` when a template repo
    /// is available. Falls back to a simple default otherwise.
    async fn build_system_prompt(&self, settings: &Settings, request: &AgentRequest) -> String {
        // Gather device state for prompt context.
        let devices = self.device_repo.list_devices().await.unwrap_or_default();
        let device_count = devices.len();
        let has_home_devices = !devices.is_empty();
        let online_device_names: String = devices
            .iter()
            .filter(|d| d.is_online)
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        // Determine thinking mode and tool calling capability.
        let provider = self.provider.read().await;
        let caps = provider.capabilities();

        // When the model supports native tool calling, tools are passed through
        // the Jinja chat template as structured definitions — NOT described in
        // the system prompt. Including them in both places confuses the model
        // into responding with text about tools instead of calling them.
        let available_tools: Vec<String> = if caps.tool_calling {
            vec![] // Tools handled by the chat template
        } else {
            // Fallback: describe tools in the system prompt for models
            // without native tool calling support.
            self.tool_definitions
                .iter()
                .map(|t| format!("{} -- {}", t.name, t.description))
                .collect()
        };
        let thinking_enabled = match settings.thinking_mode.as_str() {
            "on" => true,
            "off" => false,
            _ => caps.thinking, // "auto"
        };

        // Build temporal context.
        let now = chrono::Local::now();
        let current_date = now.format("%A, %-d %B %Y").to_string();
        let current_time = now.format("%H:%M").to_string();

        let state = prompts::PromptState {
            current_date,
            current_time,
            device_count,
            has_home_devices,
            online_device_names,
            voice_mode: request.voice_mode,
            canvas_mode: request.canvas_mode,
            available_tools,
            thinking_enabled,
            compact_prompt: false,
            prefix_hash: None,
        };

        // Try to load template from DB.
        let template_content = if let Some(ref repo) = self.template_repo {
            repo.get(&settings.prompt_style)
                .await
                .ok()
                .flatten()
                .map(|t| t.content)
        } else {
            None
        };

        let template = template_content
            .as_deref()
            .unwrap_or_else(|| prompt_builder::resolve_builtin_template(settings));

        let partition = prompt_builder::build_prompt_partition(settings, None, &state, template);

        // Combine static prefix + dynamic suffix.
        let mut prompt = partition.static_prefix;
        if !partition.dynamic_suffix.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&partition.dynamic_suffix);
        }

        // Append prompt extras from DB.
        if let Some(ref repo) = self.extras_repo {
            if let Ok(extras) = repo.list_active().await {
                for extra in &extras {
                    prompt.push_str("\n\n");
                    prompt.push_str(&extra.instruction);
                }
            }
        }

        // Append user skills.
        if let Some(ref repo) = self.skill_repo {
            if let Ok(skills) = repo.list_active().await {
                for skill in &skills {
                    prompt.push_str("\n\n## Skill: ");
                    prompt.push_str(&skill.name);
                    prompt.push('\n');
                    prompt.push_str(&skill.content);
                }
            }
        }

        prompt
    }
}

#[async_trait]
impl Agent for PondAgent {
    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
        let mut stream = self.chat_stream(request).await?;
        let mut text = String::new();

        while let Some(event) = stream.next().await {
            match event {
                Ok(AgentStreamEvent::Text { content }) => text.push_str(&content),
                Ok(AgentStreamEvent::Error { content }) => {
                    return Err(anyhow!("{}", content));
                }
                Ok(AgentStreamEvent::Done { .. }) => break,
                _ => {} // skip tool call / status events
            }
        }

        Ok(AgentResponse {
            text,
            metadata: std::collections::HashMap::new(),
        })
    }

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
        // 1. Load settings.
        let settings = self.settings_repo.get().await?;

        // 2. Hot-swap provider if needed.
        self.ensure_provider_current(&settings).await?;

        // 3. Build system prompt.
        let system_prompt = self.build_system_prompt(&settings, &request).await;

        // 4. Build user message with in-memory session history as XML.
        // History is scoped to current server lifetime (not cross-session DB).
        // Format: <history><user>...</user><assistant>...</assistant></history>
        //         <user-message>current request</user-message>
        let history_xml = {
            let hist = self.session_history.lock().await;
            if let Some(turns) = hist.get(request.session_id.as_str()) {
                if turns.is_empty() {
                    String::new()
                } else {
                    // Build history from most recent turns, respecting char budget.
                    // Older turns are dropped first to fit within context window.
                    let mut entries: Vec<String> = Vec::new();
                    let mut total_chars = 0usize;
                    for (user_msg, asst_msg) in turns.iter().rev().take(HISTORY_LIMIT / 2) {
                        let entry = format!(
                            "<user>{}</user>\n<assistant>{}</assistant>\n",
                            user_msg, asst_msg
                        );
                        if total_chars + entry.len() > HISTORY_CHAR_BUDGET {
                            break; // Budget exhausted — drop older turns
                        }
                        total_chars += entry.len();
                        entries.push(entry);
                    }
                    if entries.is_empty() {
                        String::new()
                    } else {
                        entries.reverse(); // Chronological order
                        format!("<history>\n{}</history>\n", entries.join(""))
                    }
                }
            } else {
                String::new()
            }
        };

        // 5. Build messages array with history embedded in the user message.
        let user_content = if history_xml.is_empty() {
            request.message.clone()
        } else {
            format!(
                "{}<user-message>\n{}\n</user-message>",
                history_xml, request.message
            )
        };
        let mut messages = vec![ChatMessage::user(user_content)];

        // 6. Determine if tools should be offered.
        //    Query the dispatcher LIVE each turn for pre-formatted JSON.
        //    This produces the EXACT same format as Goose's format_tools() —
        //    no intermediate conversion through ToolDefinition objects.
        let provider = self.provider.read().await;
        let caps = provider.capabilities();
        let (tools, tools_json_override, compact_json_override) = if caps.tool_calling {
            if let Some(ref disp) = self.tool_dispatcher {
                // Get pre-formatted JSON directly from dispatcher (same as Goose's format_tools)
                let full_json = disp.tools_json().await;
                let compact_json = disp.compact_tools_json().await;
                // Also get ToolDefinition vec for the provider interface
                let defs = disp.available_tool_definitions().await;
                let tool_defs: Vec<ToolDefinition> = defs
                    .into_iter()
                    .map(|(name, desc, schema)| ToolDefinition {
                        name,
                        description: desc,
                        parameters_schema: schema,
                    })
                    .collect();
                tracing::debug!(
                    tool_calling = true,
                    tools_count = tool_defs.len(),
                    full_json_len = full_json.as_ref().map(|j| j.len()).unwrap_or(0),
                    "tool schemas fetched live from dispatcher"
                );
                (tool_defs, full_json, compact_json)
            } else {
                (self.tool_definitions.clone(), None, None)
            }
        } else {
            (vec![], None, None)
        };

        let thinking_enabled = match settings.thinking_mode.as_str() {
            "on" => true,
            "off" => false,
            _ => provider.capabilities().thinking, // "auto"
        };

        let options = InferenceOptions {
            max_tokens: Some(settings.llm_max_tokens),
            temperature: Some(settings.llm_temperature),
            enable_thinking: thinking_enabled,
            tools_json_override: tools_json_override.clone(),
            compact_tools_json_override: compact_json_override.clone(),
        };

        // 7. Clone what we need for the spawned task.
        let provider = Arc::clone(&*provider);
        let session_id = request.session_id.clone();
        let user_message = request.message.clone();
        let model_role = request.model_role.clone();
        let dispatcher = self.tool_dispatcher.clone();
        let session_history = self.session_history.clone();

        // 8. Spawn the tool loop on a channel.
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<AgentStreamEvent>>(64);

        tokio::spawn(async move {
            let mut iteration = 0u32;
            let mut total_usage = UsageStats::default();
            let mut last_tool_call: Option<String> = None;

            loop {
                iteration += 1;
                if iteration > MAX_TOOL_ITERATIONS {
                    let _ = tx
                        .send(Ok(AgentStreamEvent::Error {
                            content: format!(
                                "Tool loop exceeded maximum iterations ({})",
                                MAX_TOOL_ITERATIONS
                            ),
                        }))
                        .await;
                    break;
                }

                if iteration > 1 {
                    let _ = tx
                        .send(Ok(AgentStreamEvent::Status {
                            content: format!("Thinking... ({}/{})", iteration, MAX_TOOL_ITERATIONS),
                        }))
                        .await;
                }

                // Stream from the provider.
                let mut stream = provider.stream_chat(&system_prompt, &messages, &tools, &options);

                let mut text_buf = String::new();
                let mut tool_calls: Vec<(String, String, serde_json::Value)> = Vec::new();
                // Stream text tokens immediately so the ThoughtFilter in the SSE layer
                // can capture thinking blocks in real-time. If tool calls follow, the
                // text was preamble (e.g. "Let me check...") — harmless since the
                // ThoughtFilter strips reasoning markup and the UI handles it.
                let mut streamed_any_text = false;

                while let Some(event) = stream.next().await {
                    match event {
                        Ok(pond_core::ports::inference::ChatEvent::Text(t)) => {
                            text_buf.push_str(&t);
                            // Stream immediately so ThoughtFilter sees tokens in real-time
                            let _ = tx.send(Ok(AgentStreamEvent::Text { content: t })).await;
                            streamed_any_text = true;
                        }
                        Ok(pond_core::ports::inference::ChatEvent::ToolCall {
                            id,
                            name,
                            arguments,
                        }) => {
                            tool_calls.push((id, name, arguments));
                        }
                        Ok(pond_core::ports::inference::ChatEvent::Usage(u)) => {
                            total_usage.prompt_tokens += u.prompt_tokens;
                            total_usage.completion_tokens += u.completion_tokens;
                        }
                        Err(e) => {
                            let _ = tx
                                .send(Ok(AgentStreamEvent::Error {
                                    content: e.to_string(),
                                }))
                                .await;
                            break;
                        }
                    }
                }

                // Repetition detection: if the model calls the same tool again, stop looping.
                if !tool_calls.is_empty() {
                    let current_calls: Vec<&str> = tool_calls
                        .iter()
                        .map(|(_, name, _)| name.as_str())
                        .collect();
                    let calls_key = current_calls.join(",");
                    if last_tool_call.as_deref() == Some(&calls_key) {
                        // Same tool(s) called twice in a row — break to prevent infinite loop.
                        if !text_buf.is_empty() {
                            let _ = tx
                                .send(Ok(AgentStreamEvent::Text {
                                    content: text_buf.clone(),
                                }))
                                .await;
                        }
                        messages.push(ChatMessage::assistant(&text_buf));
                        break;
                    }
                    last_tool_call = Some(calls_key);
                }

                // If no tool calls, this is the final response — text already streamed.
                if tool_calls.is_empty() {
                    messages.push(ChatMessage::assistant(&text_buf));
                    break;
                }

                // Tool calls detected — dispatch ALL concurrently, then inject results.
                // Parallel execution saves latency when multiple tools are called
                // (e.g. weather + time, or multiple lookups).
                let mut tool_context = text_buf.clone();

                // Emit all tool_call events immediately.
                for (id, name, args) in &tool_calls {
                    let _ = tx
                        .send(Ok(AgentStreamEvent::ToolCall {
                            id: id.clone(),
                            tool: name.clone(),
                            input: Some(args.clone()),
                        }))
                        .await;
                }

                // Dispatch all tools in parallel.
                let dispatch_futures: Vec<_> = tool_calls
                    .iter()
                    .map(|(id, name, args)| {
                        let disp = dispatcher.clone();
                        let name = name.clone();
                        let args = args.clone();
                        let id = id.clone();
                        async move {
                            let result_text = if let Some(ref d) = disp {
                                match d.dispatch(&name, args).await {
                                    Ok(result) => result.content,
                                    Err(e) => format!("Tool '{}' failed: {}", name, e),
                                }
                            } else {
                                format!("Tool '{}' not available (no dispatcher)", name)
                            };
                            (id, name, result_text)
                        }
                    })
                    .collect();

                let results = futures::future::join_all(dispatch_futures).await;

                // Emit results and build context (in original call order).
                for (id, name, result_text) in &results {
                    let _ = tx
                        .send(Ok(AgentStreamEvent::ToolResult {
                            id: id.clone(),
                            tool: name.clone(),
                            content: result_text.clone(),
                        }))
                        .await;

                    tool_context.push_str(&format!("\n[Tool {} returned]: {}", name, result_text));
                }

                // Append as assistant message (model sees its own tool usage + results).
                messages.push(ChatMessage::assistant(&tool_context));

                // Inject a user nudge so the model knows to synthesize a final answer
                // from the tool results rather than calling tools again.
                messages.push(ChatMessage::user(
                    "Now provide a helpful answer based on the tool results above. \
                     Do not call tools again."
                        .to_string(),
                ));

                // Loop back for next LLM call with tool results.
            }

            // ── Persist turn to in-memory session history ──────────────────
            // Store (user_message, assistant_response) for XML injection on
            // subsequent turns. Scoped to current server lifetime only.
            if let Some(last_assistant) = messages
                .iter()
                .rev()
                .find(|m| m.role == pond_core::domain::message::Role::Assistant)
            {
                let mut hist = session_history.lock().await;
                hist.entry(session_id.clone())
                    .or_insert_with(Vec::new)
                    .push((user_message.clone(), last_assistant.content.clone()));
            }

            // Emit done event.
            let _ = tx
                .send(Ok(AgentStreamEvent::Done {
                    session_id,
                    model_role,
                    usage: Some(total_usage),
                }))
                .await;
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    fn capabilities(&self) -> ModelCapabilities {
        // Can't await in a non-async fn. Use try_read for best-effort.
        self.provider
            .try_read()
            .map(|p| p.capabilities())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::domain::session::{Session, SessionMessage};
    use pond_core::ports::device_registry::{Device, RegisterDeviceRequest};
    use pond_core::ports::inference::ChatEventStream;
    use pond_core::ports::session_storage::SessionStorageError;
    use pond_core::ports::settings::SettingsRepository;

    // ── Mock implementations ─────────────────────────────────────────────────

    struct MockSettingsRepo;

    #[async_trait]
    impl SettingsRepository for MockSettingsRepo {
        async fn get(&self) -> Result<Settings> {
            Ok(Settings::default())
        }
        async fn update(&self, _settings: &Settings) -> Result<()> {
            Ok(())
        }
        async fn get_key(&self, _key: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn set_key(&self, _key: &str, _value: String) -> Result<()> {
            Ok(())
        }
    }

    struct MockDeviceRegistry;

    #[async_trait]
    impl DeviceRegistry for MockDeviceRegistry {
        async fn register(&self, _req: RegisterDeviceRequest) -> Result<Device> {
            unimplemented!()
        }
        async fn list_devices(&self) -> Result<Vec<Device>> {
            Ok(vec![])
        }
        async fn get_device(&self, _id: &str) -> Result<Option<Device>> {
            Ok(None)
        }
        async fn unregister(&self, _id: &str) -> Result<()> {
            Ok(())
        }
        async fn heartbeat(&self, _id: &str) -> Result<()> {
            Ok(())
        }
    }

    struct MockSessionStorage;

    #[async_trait]
    impl SessionStorage for MockSessionStorage {
        async fn create_session(&self, id: String) -> Result<Session, SessionStorageError> {
            Ok(Session::new(id))
        }
        async fn get_session(&self, id: &str) -> Result<Session, SessionStorageError> {
            Ok(Session::new(id.to_string()))
        }
        async fn add_message(
            &self,
            _sid: String,
            msg: SessionMessage,
        ) -> Result<SessionMessage, SessionStorageError> {
            Ok(msg)
        }
        async fn get_messages(
            &self,
            _sid: &str,
        ) -> Result<Vec<SessionMessage>, SessionStorageError> {
            Ok(vec![])
        }
        async fn update_title(
            &self,
            _sid: &str,
            _title: String,
        ) -> Result<(), SessionStorageError> {
            Ok(())
        }
        async fn delete_session(&self, _sid: &str) -> Result<(), SessionStorageError> {
            Ok(())
        }
        async fn list_sessions(&self) -> Result<Vec<Session>, SessionStorageError> {
            Ok(vec![])
        }
        async fn get_messages_paginated(
            &self,
            _sid: &str,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<SessionMessage>, SessionStorageError> {
            Ok(vec![])
        }
        async fn get_recent_messages(
            &self,
            _sid: &str,
            _limit: usize,
        ) -> Result<Vec<SessionMessage>, SessionStorageError> {
            Ok(vec![])
        }
    }

    /// Mock provider that echoes the user message.
    struct EchoProvider;

    impl InferenceProvider for EchoProvider {
        fn stream_chat(
            &self,
            _system_prompt: &str,
            messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &InferenceOptions,
        ) -> ChatEventStream {
            let last_msg = messages
                .last()
                .map(|m| m.content.clone())
                .unwrap_or_default();
            let reply = format!("Echo: {}", last_msg);

            Box::pin(async_stream::stream! {
                yield Ok(pond_core::ports::inference::ChatEvent::Text(reply));
                yield Ok(pond_core::ports::inference::ChatEvent::Usage(UsageStats {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                }));
            })
        }

        fn model_name(&self) -> String {
            "echo-test".to_string()
        }
    }

    fn make_agent(provider: Arc<dyn InferenceProvider>) -> PondAgent {
        PondAgent::new(
            provider,
            vec![],
            Arc::new(MockSettingsRepo),
            None,
            None,
            None,
            Arc::new(MockDeviceRegistry),
            Arc::new(MockSessionStorage),
            None, // no tool dispatcher in tests
        )
    }

    // ── Tests ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn chat_returns_echo_response() {
        let agent = make_agent(Arc::new(EchoProvider));
        let resp = agent
            .chat(AgentRequest {
                message: "hello".to_string(),
                session_id: "test-session".to_string(),
                model_role: "chat".to_string(),
                images: vec![],
                voice_mode: false,
                canvas_mode: false,
            })
            .await
            .unwrap();

        assert_eq!(resp.text, "Echo: hello");
    }

    #[tokio::test]
    async fn chat_stream_emits_text_and_done() {
        let agent = make_agent(Arc::new(EchoProvider));
        let mut stream = agent
            .chat_stream(AgentRequest {
                message: "world".to_string(),
                session_id: "test-session".to_string(),
                model_role: "chat".to_string(),
                images: vec![],
                voice_mode: false,
                canvas_mode: false,
            })
            .await
            .unwrap();

        let mut saw_text = false;
        let mut saw_done = false;

        while let Some(event) = stream.next().await {
            match event.unwrap() {
                AgentStreamEvent::Text { content } => {
                    assert_eq!(content, "Echo: world");
                    saw_text = true;
                }
                AgentStreamEvent::Done {
                    session_id, usage, ..
                } => {
                    assert_eq!(session_id, "test-session");
                    let u = usage.unwrap();
                    assert_eq!(u.prompt_tokens, 10);
                    assert_eq!(u.completion_tokens, 5);
                    saw_done = true;
                }
                _ => {}
            }
        }

        assert!(saw_text, "expected Text event");
        assert!(saw_done, "expected Done event");
    }

    #[tokio::test]
    async fn capabilities_returns_provider_caps() {
        let agent = make_agent(Arc::new(EchoProvider));
        let caps = agent.capabilities();
        // EchoProvider returns default caps.
        assert!(!caps.thinking);
        assert!(!caps.tool_calling);
    }
}
