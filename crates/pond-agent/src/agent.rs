//! `PondAgent`: an [`Agent`] that streams LLM and tool rounds until a final text answer.

use crate::ollama_provider::OllamaInferenceProvider;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher;
use pond_core::models::domain::message::{ChatMessage, ToolCallRecord};
use pond_core::models::domain::model_capabilities::ModelCapabilities;
use pond_core::models::ports::agent::Agent;
use pond_core::models::ports::inference::{InferenceOptions, InferenceProvider, ToolDefinition};
use pond_core::models::ports::provider::UsageStats;
use pond_core::models::services::context::context_governor::{ContextGovernor, ContextInputs};
use pond_core::models::services::context_budget::{available_history_chars, CompactionProfile};
use pond_core::models::services::history_manager::HistoryManager;
use pond_core::models::services::prompt_builder;
use pond_core::prompts;
use pond_core::shared::domain::agent::{AgentRequest, AgentResponse, AgentStreamEvent};
use pond_core::user_data::domain::session::SessionMessage;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::prompt_extra::PromptExtraRepository;
use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::skill::UserSkillRepository;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// Maximum tool-calling loop iterations before the agent gives up.
const MAX_TOOL_ITERATIONS: u32 = 10;

/// Fetch cap before `CompactionProfile::history_token_budget` does the real cut-off.
const HISTORY_LIMIT: usize = 60;

/// The GIAP agent: a hot-swappable `InferenceProvider` driving a multi-turn tool loop.
pub struct PondAgent {
    /// The active inference provider (hot-swappable at runtime).
    provider: RwLock<Arc<dyn InferenceProvider>>,
    /// Tool definitions cached from the MCP server at init time.
    tool_definitions: Vec<ToolDefinition>,
    settings_repo: Arc<dyn SettingsRepository>,
    /// Prompt template persistence (optional -- falls back to built-in).
    template_repo: Option<Arc<dyn PromptTemplateRepository>>,
    extras_repo: Option<Arc<dyn PromptExtraRepository>>,
    skill_repo: Option<Arc<dyn UserSkillRepository>>,
    session_storage: Arc<dyn SessionStorage>,
    /// MCP tool dispatcher — routes tool calls to the correct MCP server.
    tool_dispatcher: Option<Arc<dyn ToolDispatcher>>,
    /// Tracks the current provider key for hot-swap detection.
    last_provider_key: Mutex<String>,
}

impl PondAgent {
    /// Starts on `provider`; hot-swapped when `settings.chat_provider:chat_model` changes.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn InferenceProvider>,
        tool_definitions: Vec<ToolDefinition>,
        settings_repo: Arc<dyn SettingsRepository>,
        template_repo: Option<Arc<dyn PromptTemplateRepository>>,
        extras_repo: Option<Arc<dyn PromptExtraRepository>>,
        skill_repo: Option<Arc<dyn UserSkillRepository>>,
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
            session_storage,
            tool_dispatcher,
            last_provider_key: Mutex::new(initial_key),
        }
    }

    /// Swap in a new provider when `chat_provider:chat_model` differs from the last key.
    async fn ensure_provider_current(&self, settings: &Settings) -> Result<()> {
        let key = format!("{}:{}", settings.chat_provider, settings.chat_model);

        {
            let last = self.last_provider_key.lock().await;
            if *last == key {
                return Ok(());
            }
        }

        // Unconfigured (empty default): keep the injected provider.
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
                // Reuses the Ollama provider although llamafile's API is OpenAI-compatible.
                Arc::new(OllamaInferenceProvider::new(&url, &settings.chat_model))
            }
            // "local"/"gguf" providers are injected via `swap_provider`; keep the loaded one.
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

    /// Swap in a provider `ensure_provider_current` can't build itself (e.g. local GGUF).
    pub async fn swap_provider(&self, provider: Arc<dyn InferenceProvider>) {
        let key = format!("external:{}", provider.model_name());
        *self.provider.write().await = provider;
        *self.last_provider_key.lock().await = key;
    }

    /// Build the system prompt from settings, device state, and templates.
    async fn build_system_prompt(&self, settings: &Settings, request: &AgentRequest) -> String {
        let provider = self.provider.read().await;
        let caps = provider.capabilities();

        // Native tool calling gets tools via the chat template only: also describing them here
        // makes the model talk about tools instead of calling them.
        let available_tools: Vec<String> = if caps.tool_calling {
            vec![]
        } else {
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

        let now = chrono::Local::now();
        let current_date = now.format("%A, %-d %B %Y").to_string();
        let current_time = now.format("%H:%M").to_string();

        let state = prompts::PromptState {
            current_date,
            current_time,
            voice_mode: request.voice_mode,
            available_tools,
            thinking_enabled,
            compact_prompt: false,
            // TODO: derive from the provider as GooseAdapter does ("local" | "gguf").
            native_tools_json: false,
            tools_offered: self.tool_dispatcher.is_some() || !self.tool_definitions.is_empty(),
            prefix_hash: None,
        };

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

        let mut prompt = partition.static_prefix;
        if !partition.dynamic_suffix.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&partition.dynamic_suffix);
        }

        if let Some(ref repo) = self.extras_repo {
            if let Ok(extras) = repo.list_active().await {
                for extra in &extras {
                    prompt.push_str("\n\n");
                    prompt.push_str(&extra.instruction);
                }
            }
        }

        // Skills: name + description only; full instructions load via giap-device__load_skill.
        if let Some(ref repo) = self.skill_repo {
            if let Ok(skills) = repo.list_active().await {
                if !skills.is_empty() {
                    prompt.push_str(
                        "\n\n<extension-notes name=\"skills\">\nActive user-defined skills, as \
                         \"name: description\". When one looks relevant to what the user is \
                         asking, call giap-device__load_skill(name) to get its full \
                         instructions before acting on it.",
                    );
                    for skill in &skills {
                        prompt.push_str(&format!("\n- {}: {}", skill.name, skill.description));
                    }
                    prompt.push_str("\n</extension-notes>");
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
        let settings = self.settings_repo.get().await?;

        self.ensure_provider_current(&settings).await?;

        let system_prompt = self.build_system_prompt(&settings, &request).await;

        // Query the dispatcher live each turn: its JSON matches Goose's `format_tools()` exactly.
        let provider = self.provider.read().await;
        let caps = provider.capabilities();
        let (tools, tools_json_override, compact_json_override) = if caps.tool_calling {
            if let Some(ref disp) = self.tool_dispatcher {
                let full_json = disp.tools_json().await;
                let compact_json = disp.compact_tools_json().await;
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

        // Precedence lives in `ContextGovernor` so this loop can't drift from the live path.
        let context_tokens = ContextGovernor::resolve(&ContextInputs {
            provider: &settings.chat_provider,
            model: &settings.chat_model,
            override_tokens: settings.context_window_override,
            registry_pinned: None,
            catalog_context_length: None,
            engine_reported: None,
            capability_window: Some(caps.context_window_tokens),
        })
        .tokens;
        let profile = CompactionProfile::from_context_window(context_tokens);
        let tool_schema_chars = tools_json_override.as_ref().map(|j| j.len()).unwrap_or(0);
        let history_budget =
            available_history_chars(&profile, system_prompt.len(), tool_schema_chars);

        // `HistoryManager` trims whole turns newest-first, so a tool call keeps its result.
        let stored_messages = self
            .session_storage
            .get_recent_messages(request.session_id.as_str(), HISTORY_LIMIT)
            .await
            .unwrap_or_default();
        let history_mgr = HistoryManager::new(history_budget);
        let mut messages: Vec<ChatMessage> = history_mgr.build_history(&stored_messages);
        let history_messages_len = messages.len();

        tracing::debug!(
            history_budget_chars = history_budget,
            stored_messages = stored_messages.len(),
            kept_history_messages = history_messages_len,
            profile_history_tokens = profile.history_token_budget,
            "history loaded from session storage"
        );

        messages.push(ChatMessage::user(request.message.clone()));

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

        let provider = Arc::clone(&*provider);
        let session_id = request.session_id.clone();
        let model_role = request.model_role.clone();
        let dispatcher = self.tool_dispatcher.clone();
        let storage = self.session_storage.clone();

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

                let mut stream = provider.stream_chat(&system_prompt, &messages, &tools, &options);

                let mut text_buf = String::new();
                let mut tool_calls: Vec<(String, String, serde_json::Value)> = Vec::new();
                // Stream text at once so the SSE layer's ThoughtFilter sees thinking live; any
                // preamble before a tool call is harmless.
                let mut streamed_any_text = false;

                while let Some(event) = stream.next().await {
                    match event {
                        Ok(pond_core::models::ports::inference::ChatEvent::Text(t)) => {
                            text_buf.push_str(&t);
                            let _ = tx.send(Ok(AgentStreamEvent::Text { content: t })).await;
                            streamed_any_text = true;
                        }
                        Ok(pond_core::models::ports::inference::ChatEvent::ToolCall {
                            id,
                            name,
                            arguments,
                        }) => {
                            tool_calls.push((id, name, arguments));
                        }
                        Ok(pond_core::models::ports::inference::ChatEvent::Usage(u)) => {
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

                for (id, name, args) in &tool_calls {
                    let _ = tx
                        .send(Ok(AgentStreamEvent::ToolCall {
                            id: id.clone(),
                            tool: name.clone(),
                            input: Some(args.clone()),
                        }))
                        .await;
                }

                let tool_call_records: Vec<ToolCallRecord> = tool_calls
                    .iter()
                    .map(|(id, name, args)| ToolCallRecord {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: args.to_string(),
                    })
                    .collect();
                messages.push(ChatMessage::assistant_with_tool_calls(
                    text_buf.clone(),
                    tool_call_records,
                ));

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

                for (id, name, result_text) in &results {
                    let _ = tx
                        .send(Ok(AgentStreamEvent::ToolResult {
                            id: id.clone(),
                            tool: name.clone(),
                            content: result_text.clone(),
                        }))
                        .await;

                    messages.push(ChatMessage::tool_result(result_text.clone(), id.clone()));
                }

                // Small models (Gemma 4 E4B/E2B) need a nudge to answer after role:tool messages.
                messages.push(ChatMessage::user(
                    "Using the tool results above, provide a helpful answer to the user's question.",
                ));
            }

            // Persist the turn, minus the synthesis nudge (it would show as a user bubble).
            // Fire-and-forget so persistence never blocks SSE.
            const SYNTHESIS_NUDGE: &str =
                "Using the tool results above, provide a helpful answer to the user's question.";
            let turn_messages: Vec<ChatMessage> = messages[history_messages_len..]
                .iter()
                .filter(|m| m.content.trim() != SYNTHESIS_NUDGE)
                .cloned()
                .collect();
            let storage_ref = storage.clone();
            let session_id_persist = session_id.clone();
            tokio::spawn(async move {
                for msg in turn_messages {
                    let sm = SessionMessage {
                        id: uuid::Uuid::new_v4().to_string(),
                        session_id: session_id_persist.clone(),
                        message: msg,
                        created_at: chrono::Utc::now(),
                        prompt_tokens: None,
                        completion_tokens: None,
                        reasoning_tokens: None,
                        liked: None,
                    };
                    if let Err(e) = storage_ref
                        .add_message(session_id_persist.clone(), sm)
                        .await
                    {
                        tracing::warn!(error = %e, session_id = %session_id_persist, "failed to persist turn message");
                    }
                }
            });

            let _ = tx
                .send(Ok(AgentStreamEvent::Done {
                    session_id,
                    model_role,
                    usage: Some(total_usage),
                    stats: None,
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
    use pond_core::models::ports::inference::ChatEventStream;
    use pond_core::user_data::domain::profile::ProfileScope;
    use pond_core::user_data::domain::session::{Session, SessionMessage};
    use pond_core::user_data::ports::device_registry::{Device, RegisterDeviceRequest};
    use pond_core::user_data::ports::session_storage::SessionStorageError;
    use pond_core::user_data::ports::settings::SettingsRepository;

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
                yield Ok(pond_core::models::ports::inference::ChatEvent::Text(reply));
                yield Ok(pond_core::models::ports::inference::ChatEvent::Usage(UsageStats {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    reasoning_tokens: None,
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
                // Descriptive only: this loop never reads the profile fields.
                profile_scope: ProfileScope::Household,
                profile_context: None,
                tool_group_allowlist: None,
                warmup: false,
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
                profile_scope: ProfileScope::Household,
                profile_context: None,
                tool_group_allowlist: None,
                warmup: false,
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

    #[tokio::test]
    async fn system_prompt_injects_skill_description_but_not_content() {
        use pond_core::user_data::domain::skill::UserSkill;
        use pond_core::user_data::mocks::mock_skill::MockSkillRepository;

        let skill_repo = MockSkillRepository::new();
        skill_repo
            .create(&UserSkill {
                id: "skill-1".to_string(),
                name: "Morning Briefing".to_string(),
                description: "Summarise weather and today's schedule".to_string(),
                icon: "sparkles".to_string(),
                content: "SECRET_FULL_INSTRUCTIONS_SHOULD_NOT_APPEAR_EVERY_TURN".to_string(),
                active: true,
                created_at: "2026-01-01T00:00:00Z".to_string(),
            })
            .await
            .unwrap();

        let agent = PondAgent::new(
            Arc::new(EchoProvider),
            vec![],
            Arc::new(MockSettingsRepo),
            None,
            None,
            Some(Arc::new(skill_repo)),
            Arc::new(MockSessionStorage),
            None,
        );

        let request = AgentRequest {
            message: "hello".to_string(),
            session_id: "test-session".to_string(),
            model_role: "chat".to_string(),
            images: vec![],
            voice_mode: false,
            canvas_mode: false,
            profile_scope: ProfileScope::Household,
            profile_context: None,
            tool_group_allowlist: None,
            warmup: false,
        };
        let prompt = agent
            .build_system_prompt(&Settings::default(), &request)
            .await;

        assert!(
            prompt.contains("Morning Briefing: Summarise weather and today's schedule"),
            "expected name + description in prompt, got: {prompt}"
        );
        assert!(prompt.contains("giap-device__load_skill"));
        assert!(
            !prompt.contains("SECRET_FULL_INSTRUCTIONS_SHOULD_NOT_APPEAR_EVERY_TURN"),
            "full skill content must not be injected every turn, got: {prompt}"
        );
    }
}
