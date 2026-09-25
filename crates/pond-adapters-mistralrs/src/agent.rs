//! `MistralRsAgent`: a turn served straight on mistral.rs, reusing only GIAP's (goose-free)
//! system prompt. No review, delegation, compaction, memory, vision or recipes: a measurement
//! instrument, not a replacement; see `docs/developer/mistralrs-direct-agent.md`.

use crate::provider::{MistralRsProvider, MrEvent};
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use pond_core::mcp::ports::tools::tool_dispatcher::ToolDispatcher;
use pond_core::models::domain::message::{ChatMessage, Role, ToolCallRecord};
use pond_core::models::domain::model_capabilities::ModelCapabilities;
use pond_core::models::ports::agent::Agent;
use pond_core::models::ports::inference::{InferenceOptions, InferenceProvider, ToolDefinition};
use pond_core::models::services::context::context_budget::{
    available_history_chars, CompactionProfile,
};
use pond_core::models::services::context::context_governor::{ContextGovernor, ContextInputs};
use pond_core::models::services::history_manager::HistoryManager;
use pond_core::models::services::prompt_builder;
use pond_core::prompts;
use pond_core::shared::domain::agent::{AgentRequest, AgentResponse, AgentStreamEvent};
use pond_core::shared::domain::turn_stats::TurnStats;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::prompt_extra::PromptExtraRepository;
use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::skill::UserSkillRepository;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Tool rounds before the loop gives up (as in `PondAgent`); unbounded would hang on-device.
const MAX_TOOL_ROUNDS: u32 = 10;

/// Messages fetched before budgeting trims them.
const HISTORY_LIMIT: usize = 60;

/// Shortest decode window worth a rate: a tool-call delta arrives at once, giving absurd rates.
const MIN_DECODE_WINDOW_MS: u64 = 100;

pub struct MistralRsAgent {
    provider: Arc<MistralRsProvider>,
    settings_repo: Arc<dyn SettingsRepository>,
    template_repo: Option<Arc<dyn PromptTemplateRepository>>,
    extras_repo: Option<Arc<dyn PromptExtraRepository>>,
    skill_repo: Option<Arc<dyn UserSkillRepository>>,
    session_storage: Arc<dyn SessionStorage>,
    tools: Option<Arc<dyn ToolDispatcher>>,
}

impl MistralRsAgent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<MistralRsProvider>,
        settings_repo: Arc<dyn SettingsRepository>,
        template_repo: Option<Arc<dyn PromptTemplateRepository>>,
        extras_repo: Option<Arc<dyn PromptExtraRepository>>,
        skill_repo: Option<Arc<dyn UserSkillRepository>>,
        session_storage: Arc<dyn SessionStorage>,
        tools: Option<Arc<dyn ToolDispatcher>>,
    ) -> Self {
        Self {
            provider,
            settings_repo,
            template_repo,
            extras_repo,
            skill_repo,
            session_storage,
            tools,
        }
    }

    /// Assemble GIAP's system prompt, with `native_tools_json`: tools travel in the request body,
    /// and also listing them in prose makes a model describe tools instead of calling them.
    async fn build_system_prompt(
        &self,
        settings: &Settings,
        request: &AgentRequest,
        tools_offered: bool,
    ) -> String {
        let caps = self.provider.capabilities();
        let thinking_enabled = thinking_enabled(settings, &caps);

        let now = chrono::Local::now();
        let state = prompts::PromptState {
            current_date: now.format("%A, %-d %B %Y").to_string(),
            current_time: now.format("%H:%M").to_string(),
            voice_mode: request.voice_mode,
            // Empty because the tools go in the request body, not the prose.
            available_tools: Vec::new(),
            thinking_enabled,
            compact_prompt: false,
            native_tools_json: true,
            tools_offered,
            prefix_hash: None,
        };

        let db_template = match self.template_repo {
            Some(ref repo) => repo
                .get(&settings.prompt_style)
                .await
                .ok()
                .flatten()
                .map(|t| t.content),
            None => None,
        };
        let template = db_template
            .as_deref()
            .unwrap_or_else(|| prompt_builder::resolve_builtin_template(settings));

        let partition = prompt_builder::build_prompt_partition(
            settings,
            request.profile_context.as_ref(),
            &state,
            template,
        );

        let mut prompt = partition.static_prefix;
        if !partition.dynamic_suffix.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&partition.dynamic_suffix);
        }

        if let Some(ref repo) = self.extras_repo {
            if let Ok(extras) = repo.list_active().await {
                for extra in &extras {
                    prompt.push_str(&format!(
                        "\n\n<extension-notes name=\"{}\">\n{}\n</extension-notes>",
                        extra.key, extra.instruction
                    ));
                }
            }
        }

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

    /// This turn's tools; `tools_json` passes through as-is, since a conversion could alter it.
    async fn turn_tools(&self) -> (Vec<ToolDefinition>, Option<String>) {
        // Same switch as the goose path, so it means one thing on both backends.
        if pond_core::mcp::domain::tool_group::no_tools_env_set() {
            tracing::warn!("GIAP_NO_TOOLS is set — this turn is offered no tools");
            return (Vec::new(), None);
        }
        let Some(ref disp) = self.tools else {
            return (Vec::new(), None);
        };
        let json = disp.tools_json().await;
        let defs = disp
            .available_tool_definitions()
            .await
            .into_iter()
            .map(|(name, description, parameters_schema)| ToolDefinition {
                name,
                description,
                parameters_schema,
            })
            .collect();
        (defs, json)
    }
}

/// Environment variable that turns on payload capture.
const DUMP_DIR_ENV: &str = "GIAP_MISTRALRS_DUMP_DIR";

/// Write this turn's first request to `$GIAP_MISTRALRS_DUMP_DIR` as `system.txt`, `tools.json`
/// and `body.json`, for bake-off replay. Failures are logged, never fatal to the turn.
async fn dump_payload(
    provider: &MistralRsProvider,
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    options: &InferenceOptions,
) {
    let Ok(dir) = std::env::var(DUMP_DIR_ENV) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let body = match provider
        .request_body_json(system_prompt, messages, tools, options, false)
        .await
    {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "could not build the payload dump");
            return;
        }
    };
    let tools_json = body
        .get("tools")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));

    let writes: [(&str, String); 3] = [
        ("system.txt", system_prompt.to_string()),
        (
            "tools.json",
            serde_json::to_string_pretty(&tools_json).unwrap_or_default(),
        ),
        (
            "body.json",
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        ),
    ];
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(dir = %dir.display(), error = %e, "could not create the dump directory");
        return;
    }
    for (name, content) in writes {
        if let Err(e) = std::fs::write(dir.join(name), content) {
            tracing::warn!(file = %name, error = %e, "could not write the payload dump");
            return;
        }
    }
    tracing::info!(
        dir = %dir.display(),
        system_chars = system_prompt.len(),
        tools = tools.len(),
        "wrote the turn payload"
    );
}

/// Resolve `thinking_mode`, which is a three-state setting and not a boolean.
fn thinking_enabled(settings: &Settings, caps: &ModelCapabilities) -> bool {
    match settings.thinking_mode.as_str() {
        "on" => true,
        "off" => false,
        _ => caps.thinking,
    }
}

#[async_trait]
impl Agent for MistralRsAgent {
    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
        let mut stream = self.chat_stream(request).await?;
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            match event {
                Ok(AgentStreamEvent::Text { content }) => text.push_str(&content),
                Ok(AgentStreamEvent::Error { content }) => {
                    return Err(anyhow::anyhow!("{}", content))
                }
                Ok(AgentStreamEvent::Done { .. }) => break,
                _ => {}
            }
        }
        Ok(AgentResponse {
            text,
            metadata: HashMap::new(),
        })
    }

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
        let settings = self.settings_repo.get().await?;
        // Tools first: the prompt's tool guidance is only included when there are tools.
        let (tool_defs, tools_json) = self.turn_tools().await;
        let system_prompt = self
            .build_system_prompt(&settings, &request, !tool_defs.is_empty())
            .await;

        let caps = self.provider.capabilities();
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
        let history_budget = available_history_chars(
            &profile,
            system_prompt.len(),
            tools_json.as_ref().map(|j| j.len()).unwrap_or(0),
        );

        let stored = self
            .session_storage
            .get_recent_messages(&request.session_id, HISTORY_LIMIT)
            .await
            .unwrap_or_default();
        let mut messages = HistoryManager::new(history_budget).build_history(&stored);
        // `ChatService::persist_user_message` runs before the agent, so this turn's message may
        // already be the newest row; don't show the model the question twice.
        if messages
            .last()
            .is_some_and(|m| m.role == Role::User && m.content == request.message)
        {
            messages.pop();
        }
        messages.push(ChatMessage::user(request.message.clone()));

        tracing::info!(
            session = %request.session_id,
            system_chars = system_prompt.len(),
            tools = tool_defs.len(),
            tools_json_chars = tools_json.as_ref().map(|j| j.len()).unwrap_or(0),
            history_messages = messages.len() - 1,
            context_tokens,
            "mistralrs turn start"
        );

        let options = InferenceOptions {
            max_tokens: Some(prompts::estimate_response_budget(
                &request.message,
                settings.llm_max_tokens,
            )),
            temperature: Some(settings.llm_temperature),
            enable_thinking: thinking_enabled(&settings, &caps) && !request.voice_mode,
            tools_json_override: tools_json,
            compact_tools_json_override: None,
        };

        // Before the first request, so a failing turn still leaves its payload.
        dump_payload(
            &self.provider,
            &system_prompt,
            &messages,
            &tool_defs,
            &options,
        )
        .await;

        // Everything the loop needs, owned: the stream outlives `&self`.
        let provider = self.provider.clone();
        let dispatcher = self.tools.clone();
        let session_id = request.session_id.clone();
        let model_role = request.model_role.clone();
        let context_limit = caps.context_window_tokens;

        Ok(Box::pin(async_stream::stream! {
            // Nothing else on this path sets it; unset, tool egress is anonymous in the audit log.
            pond_core::shared::services::egress::set_current_session_id(&session_id);

            let mut stats = TurnStats {
                context_limit_tokens: Some(context_limit),
                ..Default::default()
            };
            let mut round = 0u32;

            loop {
                round += 1;
                stats.inference_count = round;

                let sent_at = Instant::now();
                let mut first_token_at: Option<Instant> = None;
                let mut answer = String::new();
                let mut calls: Vec<ToolCallRecord> = Vec::new();

                let mut stream = match provider
                    .stream_raw(&system_prompt, &messages, &tool_defs, &options)
                    .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        yield Ok(AgentStreamEvent::Error { content: e.to_string() });
                        return;
                    }
                };

                while let Some(item) = stream.next().await {
                    match item {
                        Ok(MrEvent::Text(t)) => {
                            first_token_at.get_or_insert_with(Instant::now);
                            answer.push_str(&t);
                            yield Ok(AgentStreamEvent::Text { content: t });
                        }
                        Ok(MrEvent::Thinking(t)) => {
                            first_token_at.get_or_insert_with(Instant::now);
                            yield Ok(AgentStreamEvent::Thinking { content: t });
                        }
                        Ok(MrEvent::ToolCall { id, name, arguments }) => {
                            first_token_at.get_or_insert_with(Instant::now);
                            calls.push(ToolCallRecord {
                                id,
                                name,
                                arguments: arguments.to_string(),
                            });
                        }
                        Ok(MrEvent::Usage(u)) => {
                            // The final prompt is the real context load: overwrite, don't sum.
                            stats.prompt_tokens = u.prompt_tokens;
                            stats.context_used_tokens = Some(u.prompt_tokens);
                            stats.completion_tokens += u.completion_tokens;
                        }
                        Err(e) => {
                            // Includes a 200 with an error body, invisible at the HTTP layer.
                            yield Ok(AgentStreamEvent::Error { content: e.to_string() });
                            return;
                        }
                    }
                }

                // Wall-clock: mistral.rs's OpenAI surface reports no prefill timing, so
                // `prefill_ms` stays None.
                if let Some(first) = first_token_at {
                    if stats.ttft_ms.is_none() {
                        stats.ttft_ms = Some(first.duration_since(sent_at).as_millis() as u64);
                    }
                    let window = first.elapsed().as_millis() as u64;
                    if window >= MIN_DECODE_WINDOW_MS {
                        stats.decode_ms = Some(stats.decode_ms.unwrap_or(0) + window);
                    }
                }

                if calls.is_empty() {
                    break;
                }

                let Some(ref disp) = dispatcher else {
                    yield Ok(AgentStreamEvent::Error {
                        content: "the model asked for a tool but no dispatcher is wired".into(),
                    });
                    return;
                };

                messages.push(ChatMessage::assistant_with_tool_calls(
                    answer.clone(),
                    calls.clone(),
                ));

                for call in &calls {
                    let args: serde_json::Value = serde_json::from_str(&call.arguments)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    yield Ok(AgentStreamEvent::ToolCall {
                        id: call.id.clone(),
                        tool: call.name.clone(),
                        input: Some(args.clone()),
                    });

                    pond_core::shared::services::egress::set_current_tool(&call.name);
                    let content = match disp.dispatch(&call.name, args).await {
                        Ok(r) => r.content,
                        // A failed tool is a result the model can react to, not a dead turn.
                        Err(e) => format!("Tool '{}' failed: {}", call.name, e),
                    };
                    pond_core::shared::services::egress::set_current_tool("");

                    yield Ok(AgentStreamEvent::ToolResult {
                        id: call.id.clone(),
                        tool: call.name.clone(),
                        content: content.clone(),
                    });
                    messages.push(ChatMessage::tool_result(content, call.id.clone()));
                }

                if round >= MAX_TOOL_ROUNDS {
                    tracing::warn!(rounds = round, "mistralrs tool loop hit its guard");
                    yield Ok(AgentStreamEvent::TurnLimitReached { max_turns: MAX_TOOL_ROUNDS });
                    break;
                }
            }

            stats.finalize_rates();
            let usage = pond_core::models::ports::provider::UsageStats {
                prompt_tokens: stats.prompt_tokens,
                completion_tokens: stats.completion_tokens,
                reasoning_tokens: None,
            };
            tracing::info!(
                ttft_ms = ?stats.ttft_ms,
                decode_tok_per_sec = ?stats.decode_tok_per_sec,
                prompt_tokens = stats.prompt_tokens,
                inference_count = stats.inference_count,
                "mistralrs turn end"
            );
            yield Ok(AgentStreamEvent::Done {
                session_id,
                model_role,
                usage: Some(usage),
                stats: Some(stats),
            });
        }))
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.provider.capabilities()
    }

    async fn call_tool(
        &self,
        _session_id: &str,
        tool_name: &str,
        args_json: &str,
    ) -> Result<String> {
        let Some(ref disp) = self.tools else {
            return Err(anyhow::anyhow!("no tool dispatcher is wired"));
        };
        let args = serde_json::from_str(args_json).unwrap_or_else(|_| serde_json::json!({}));
        Ok(disp.dispatch(tool_name, args).await?.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(thinking: bool) -> ModelCapabilities {
        ModelCapabilities {
            thinking,
            ..Default::default()
        }
    }

    fn settings_with_mode(mode: &str) -> Settings {
        Settings {
            thinking_mode: mode.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn thinking_mode_on_and_off_override_the_model() {
        assert!(thinking_enabled(&settings_with_mode("on"), &caps(false)));
        assert!(!thinking_enabled(&settings_with_mode("off"), &caps(true)));
    }

    #[test]
    fn thinking_mode_auto_defers_to_the_model() {
        assert!(thinking_enabled(&settings_with_mode("auto"), &caps(true)));
        assert!(!thinking_enabled(&settings_with_mode("auto"), &caps(false)));
    }

    #[test]
    fn the_current_message_is_not_repeated_when_storage_already_holds_it() {
        let message = "what time is it?";
        let mut messages = vec![
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi"),
            ChatMessage::user(message),
        ];
        if messages
            .last()
            .is_some_and(|m| m.role == Role::User && m.content == message)
        {
            messages.pop();
        }
        messages.push(ChatMessage::user(message.to_string()));
        assert_eq!(messages.len(), 3);
        assert_eq!(messages.iter().filter(|m| m.content == message).count(), 1);
    }

    /// Only the last message can be this turn's, so only it is a dedupe candidate.
    #[test]
    fn an_identical_question_from_an_earlier_turn_survives() {
        let message = "what time is it?";
        let mut messages = vec![ChatMessage::user(message), ChatMessage::assistant("01:31")];
        if messages
            .last()
            .is_some_and(|m| m.role == Role::User && m.content == message)
        {
            messages.pop();
        }
        messages.push(ChatMessage::user(message.to_string()));
        assert_eq!(messages.len(), 3);
        assert_eq!(messages.iter().filter(|m| m.content == message).count(), 2);
    }

    #[test]
    fn a_decode_window_shorter_than_the_floor_yields_no_rate() {
        let mut stats = TurnStats {
            completion_tokens: 12,
            decode_ms: None,
            ..Default::default()
        };
        stats.finalize_rates();
        assert!(stats.decode_tok_per_sec.is_none());
    }
}
