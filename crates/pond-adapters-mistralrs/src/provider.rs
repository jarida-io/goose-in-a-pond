//! `MistralRsProvider` — a streaming client for a mistral.rs server's
//! OpenAI-compatible surface.
//!
//! Two stream APIs, one implementation. [`MistralRsProvider::stream_raw`]
//! keeps thinking separate from the answer, which is what
//! [`MistralRsAgent`](crate::MistralRsAgent) wants; the
//! [`InferenceProvider`] impl folds thinking back into the text stream wrapped
//! in Gemma's `<|channel>thought … <channel|>` markers, because that port has
//! no thinking variant and `pond-api`'s `ThoughtFilter` already strips exactly
//! those markers at the edge. Adding a variant to `ChatEvent` would have been
//! the tidier fix and is deliberately not done here: this crate is a
//! checkpoint, and it should be removable without a `pond-core` revert.

use crate::wire::{
    ChatRequest, Delta, ModelsResponse, StreamChunk, StreamOptions, WireMessage, WireUsage,
};
use anyhow::{anyhow, Result};
use futures::stream::StreamExt;
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::models::domain::model_capabilities::ModelCapabilities;
use pond_core::models::ports::inference::{
    ChatEvent, ChatEventStream, InferenceOptions, InferenceProvider, ToolDefinition,
};
use pond_core::models::ports::provider::UsageStats;
use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Mutex;
use tokio::sync::OnceCell;

/// Gemma 4's thinking markers. Repeated here rather than imported because the
/// only other definition is a private constant behind `pond-api`'s filter.
const THOUGHT_OPEN: &str = "<|channel>thought";
const THOUGHT_CLOSE: &str = "<channel|>";

/// One event from a mistral.rs stream, before any port has flattened it.
#[derive(Debug, Clone)]
pub enum MrEvent {
    /// A fragment of the answer.
    Text(String),
    /// A fragment of `reasoning_content` — the model's own thinking.
    Thinking(String),
    /// A complete tool call: fragments are joined before this is emitted.
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// The final usage frame, when `stream_options.include_usage` produced one.
    Usage(UsageStats),
}

/// A pinned stream of [`MrEvent`].
pub type MrEventStream = Pin<Box<dyn futures::Stream<Item = Result<MrEvent>> + Send>>;

pub struct MistralRsProvider {
    client: reqwest::Client,
    base_url: String,
    /// What GIAP calls the model. Used for capability detection and as the
    /// fallback when `/v1/models` cannot be reached.
    configured_model: String,
    /// What the server calls it. Resolved once, lazily.
    served_model: OnceCell<String>,
    /// Reported by the server at resolution time, for `TurnStats`. There is no
    /// context field on `/v1/models`, so this stays whatever GIAP configured.
    context_window: Mutex<u32>,
}

impl MistralRsProvider {
    /// `base_url` is the server root, e.g. `http://127.0.0.1:9002` — no `/v1`.
    pub fn new(base_url: &str, model: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            configured_model: model.to_string(),
            served_model: OnceCell::new(),
            context_window: Mutex::new(
                ModelCapabilities::from_model_name(model).context_window_tokens,
            ),
        }
    }

    /// Declare the context window GIAP resolved for this model. mistral.rs
    /// exposes none, so nothing else can supply it, and a `TurnStats` with a
    /// `context_used` and no limit renders as a percentage of nothing.
    pub fn with_context_window(self, tokens: u32) -> Self {
        if tokens > 0 {
            *self
                .context_window
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = tokens;
        }
        self
    }

    pub fn context_window(&self) -> u32 {
        *self
            .context_window
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Ask the server what it serves.
    ///
    /// mistral.rs answers a name it does not serve with a 400 instead of
    /// falling back to its single loaded model, so sending GIAP's own model id
    /// — which carries a quant tag the server never saw — fails every request.
    /// Resolved once and cached; on any error the configured name is used, so
    /// an unreachable server fails at the chat call with a real message rather
    /// than here with a misleading one.
    pub async fn resolve_model(&self) -> &str {
        self.served_model
            .get_or_init(|| async {
                match self.fetch_served_model().await {
                    Ok(id) => {
                        if id != self.configured_model {
                            tracing::info!(
                                configured = %self.configured_model,
                                served = %id,
                                "mistral.rs serves this model under a different id; using the server's"
                            );
                        }
                        id
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "could not read /v1/models from mistral.rs; using the configured id"
                        );
                        self.configured_model.clone()
                    }
                }
            })
            .await
    }

    async fn fetch_served_model(&self) -> Result<String> {
        let url = format!("{}/v1/models", self.base_url);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow!("GET /v1/models returned {}", resp.status()));
        }
        let models: ModelsResponse = resp.json().await?;
        models
            .data
            .into_iter()
            .map(|m| m.id)
            .find(|id| !id.is_empty())
            .ok_or_else(|| anyhow!("mistral.rs reports no models"))
    }

    /// Build the wire message array. The system prompt leads; everything after
    /// it is GIAP's own history, tool calls and results included, so the model
    /// sees its prior tool usage the way it produced it.
    pub(crate) fn to_wire_messages(
        system_prompt: &str,
        messages: &[ChatMessage],
    ) -> Vec<WireMessage> {
        let mut out = Vec::with_capacity(messages.len() + 1);
        if !system_prompt.is_empty() {
            out.push(WireMessage {
                role: "system",
                content: system_prompt.to_string(),
                tool_calls: Vec::new(),
                tool_call_id: None,
            });
        }
        for m in messages {
            out.push(WireMessage {
                role: match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::System => "system",
                    Role::Tool => "tool",
                },
                content: m.content.clone(),
                tool_calls: m
                    .tool_calls
                    .iter()
                    .map(|tc| crate::wire::WireToolCall {
                        id: tc.id.clone(),
                        kind: "function",
                        function: crate::wire::WireFunctionCall {
                            name: tc.name.clone(),
                            arguments: tc.arguments.clone(),
                        },
                    })
                    .collect(),
                tool_call_id: m.tool_call_id.clone(),
            });
        }
        out
    }

    /// Parse the pre-formatted tools JSON the dispatcher produced. On a parse
    /// failure the turn runs WITHOUT tools rather than failing: a pond that
    /// answers without acting is degraded, one that errors is down.
    fn tools_array(options: &InferenceOptions, tools: &[ToolDefinition]) -> Vec<serde_json::Value> {
        if let Some(ref json) = options.tools_json_override {
            match serde_json::from_str::<Vec<serde_json::Value>>(json) {
                Ok(v) => return v,
                Err(e) => {
                    tracing::warn!(error = %e, "tools_json_override did not parse; falling back")
                }
            }
        }
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters_schema,
                    }
                })
            })
            .collect()
    }

    /// The request body for one completion.
    ///
    /// Factored out so the capture hook writes the bytes that are actually
    /// sent. A dump reassembled from the same inputs by a second code path is
    /// worth nothing: the moment the two drift, the lab replays a prompt this
    /// pond never sent, and every number taken from it is wrong in a way
    /// nothing reports.
    fn build_request<'a>(
        model: &'a str,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &InferenceOptions,
        stream: bool,
    ) -> ChatRequest<'a> {
        ChatRequest {
            model,
            messages: Self::to_wire_messages(system_prompt, messages),
            stream,
            max_tokens: options.max_tokens,
            temperature: options.temperature,
            tools: Self::tools_array(options, tools),
            tool_choice: None,
            stream_options: StreamOptions {
                include_usage: true,
            },
            chat_template_kwargs: serde_json::json!({
                "enable_thinking": options.enable_thinking
            }),
        }
    }

    /// The request body as JSON, ready to POST at `/v1/chat/completions`.
    ///
    /// `stream` is a parameter because a replay harness usually wants
    /// `false` while the live path always wants `true`; everything else is
    /// identical to what [`stream_raw`](Self::stream_raw) sends.
    pub async fn request_body_json(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &InferenceOptions,
        stream: bool,
    ) -> Result<serde_json::Value> {
        let model = self.resolve_model().await.to_string();
        Ok(serde_json::to_value(Self::build_request(
            &model,
            system_prompt,
            messages,
            tools,
            options,
            stream,
        ))?)
    }

    /// Stream one completion, keeping thinking distinct from the answer.
    pub async fn stream_raw(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &InferenceOptions,
    ) -> Result<MrEventStream> {
        let model = self.resolve_model().await.to_string();
        let body = Self::build_request(&model, system_prompt, messages, tools, options, true);

        let url = format!("{}/v1/chat/completions", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("mistral.rs unreachable at {} — is it running? ({})", url, e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("mistral.rs error {}: {}", status, text));
        }

        Ok(Box::pin(async_stream::stream! {
            let mut resp = resp;
            let mut buf = String::new();
            // Keyed by `index` so multiple calls in one turn stay separate;
            // BTreeMap so they are emitted in the order the model produced them.
            let mut pending: BTreeMap<usize, PendingCall> = BTreeMap::new();
            let mut usage: Option<WireUsage> = None;

            loop {
                let chunk = match resp.chunk().await {
                    Ok(Some(c)) => c,
                    Ok(None) => break,
                    Err(e) => {
                        yield Err(anyhow!("mistral.rs stream broke: {}", e));
                        return;
                    }
                };
                buf.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(nl) = buf.find('\n') {
                    let line = buf[..nl].trim().to_string();
                    buf.drain(..=nl);
                    let Some(payload) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let payload = payload.trim();
                    if payload.is_empty() || payload == "[DONE]" {
                        continue;
                    }
                    let parsed: StreamChunk = match serde_json::from_str(payload) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::debug!(error = %e, frame = %payload, "unparsed SSE frame");
                            continue;
                        }
                    };

                    // The 200-with-an-error case. Surfacing it is the whole
                    // reason `WireError` exists — see wire.rs.
                    if let Some(err) = parsed.error {
                        yield Err(anyhow!("mistral.rs reported: {}", err.message));
                        return;
                    }
                    if let Some(u) = parsed.usage {
                        usage = Some(u);
                    }
                    for choice in parsed.choices {
                        for ev in drain_delta(choice.delta, &mut pending) {
                            yield Ok(ev);
                        }
                    }
                }
            }

            // Tool calls are complete only at end of stream: `arguments` arrives
            // in fragments and nothing marks the last one.
            for (_, call) in std::mem::take(&mut pending) {
                if call.name.is_empty() {
                    continue;
                }
                let arguments = serde_json::from_str(&call.arguments)
                    .unwrap_or_else(|_| serde_json::json!({}));
                yield Ok(MrEvent::ToolCall { id: call.id, name: call.name, arguments });
            }

            if let Some(u) = usage {
                yield Ok(MrEvent::Usage(UsageStats {
                    prompt_tokens: u.prompt_tokens,
                    completion_tokens: u.completion_tokens,
                    reasoning_tokens: None,
                }));
            }
        }))
    }
}

/// A tool call being assembled across fragments.
#[derive(Default)]
pub(crate) struct PendingCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Turn one delta into the events it carries, folding tool-call fragments into
/// `pending` rather than emitting them (they are not complete yet).
pub(crate) fn drain_delta(
    delta: Delta,
    pending: &mut BTreeMap<usize, PendingCall>,
) -> Vec<MrEvent> {
    let mut out = Vec::new();
    if let Some(t) = delta.reasoning_content.filter(|s| !s.is_empty()) {
        out.push(MrEvent::Thinking(t));
    }
    if let Some(t) = delta.content.filter(|s| !s.is_empty()) {
        out.push(MrEvent::Text(t));
    }
    for tc in delta.tool_calls {
        let slot = pending.entry(tc.index).or_default();
        if let Some(id) = tc.id {
            if !id.is_empty() {
                slot.id = id;
            }
        }
        if let Some(f) = tc.function {
            if let Some(name) = f.name {
                if !name.is_empty() {
                    slot.name = name;
                }
            }
            if let Some(args) = f.arguments {
                slot.arguments.push_str(&args);
            }
        }
        if slot.id.is_empty() {
            slot.id = format!("call_{}", uuid::Uuid::new_v4().simple());
        }
    }
    out
}

impl InferenceProvider for MistralRsProvider {
    fn stream_chat(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &InferenceOptions,
    ) -> ChatEventStream {
        // Owned, because the stream outlives this borrow.
        let system_prompt = system_prompt.to_string();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let options = options.clone();
        let base_url = self.base_url.clone();
        let model = self.configured_model.clone();
        let ctx = self.context_window();

        Box::pin(async_stream::stream! {
            let provider = MistralRsProvider::new(&base_url, &model).with_context_window(ctx);
            let mut inner = match provider
                .stream_raw(&system_prompt, &messages, &tools, &options)
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };
            // Thinking is re-wrapped in the markers `ThoughtFilter` strips, so a
            // consumer of this port sees the same shape the local engine emits.
            let mut in_thought = false;
            while let Some(item) = inner.next().await {
                match item {
                    Ok(MrEvent::Text(t)) => {
                        if in_thought {
                            in_thought = false;
                            yield Ok(ChatEvent::Text(THOUGHT_CLOSE.to_string()));
                        }
                        yield Ok(ChatEvent::Text(t));
                    }
                    Ok(MrEvent::Thinking(t)) => {
                        if !in_thought {
                            in_thought = true;
                            yield Ok(ChatEvent::Text(THOUGHT_OPEN.to_string()));
                        }
                        yield Ok(ChatEvent::Text(t));
                    }
                    Ok(MrEvent::ToolCall { id, name, arguments }) => {
                        if in_thought {
                            in_thought = false;
                            yield Ok(ChatEvent::Text(THOUGHT_CLOSE.to_string()));
                        }
                        yield Ok(ChatEvent::ToolCall { id, name, arguments });
                    }
                    Ok(MrEvent::Usage(u)) => yield Ok(ChatEvent::Usage(u)),
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
            if in_thought {
                yield Ok(ChatEvent::Text(THOUGHT_CLOSE.to_string()));
            }
        })
    }

    fn model_name(&self) -> String {
        self.configured_model.clone()
    }

    fn capabilities(&self) -> ModelCapabilities {
        let mut caps = ModelCapabilities::from_model_name(&self.configured_model);
        // Not a guess about the model: mistral.rs takes OpenAI `tools` on every
        // model it serves and returns structured `tool_calls`. Whether the model
        // is any good at it is a different question, and one the tool-call
        // reliability workload answers.
        caps.tool_calling = true;
        caps.context_window_tokens = self.context_window();
        // Whatever the name says: nothing on this path reads `request.images`, so a picture
        // would be dropped while the capability told the UI and the prompt it could be seen.
        caps.vision = false;
        caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::message::ToolCallRecord;

    #[test]
    fn the_system_prompt_leads_and_history_follows_in_order() {
        let history = vec![
            ChatMessage::user("what is the weather"),
            ChatMessage::assistant("let me check"),
        ];
        let wire = MistralRsProvider::to_wire_messages("you are a duck", &history);
        assert_eq!(wire.len(), 3);
        assert_eq!(wire[0].role, "system");
        assert_eq!(wire[0].content, "you are a duck");
        assert_eq!(wire[1].role, "user");
        assert_eq!(wire[2].role, "assistant");
    }

    /// An empty system prompt must not become an empty system message: some
    /// templates render one as a blank turn the model then answers.
    #[test]
    fn an_empty_system_prompt_adds_no_message() {
        let wire = MistralRsProvider::to_wire_messages("", &[ChatMessage::user("hi")]);
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0].role, "user");
    }

    /// A prior turn's tool call and its result both survive the round trip —
    /// this is what lets the model see what it already did.
    #[test]
    fn prior_tool_calls_and_results_round_trip_onto_the_wire() {
        let history = vec![
            ChatMessage::assistant_with_tool_calls(
                "",
                vec![ToolCallRecord {
                    id: "call_7".into(),
                    name: "giap-weather__get_current_weather".into(),
                    arguments: r#"{"city":"Nairobi"}"#.into(),
                }],
            ),
            ChatMessage::tool_result("22C and clear", "call_7"),
        ];
        let wire = MistralRsProvider::to_wire_messages("sys", &history);
        assert_eq!(wire[1].tool_calls.len(), 1);
        assert_eq!(
            wire[1].tool_calls[0].function.name,
            "giap-weather__get_current_weather"
        );
        assert_eq!(
            wire[1].tool_calls[0].function.arguments,
            r#"{"city":"Nairobi"}"#
        );
        assert_eq!(wire[2].role, "tool");
        assert_eq!(wire[2].tool_call_id.as_deref(), Some("call_7"));
    }

    /// The capture hook and the live request must be the same body. This is the
    /// property that makes a dumped payload worth replaying, so it is asserted
    /// on the shared builder rather than on either caller.
    #[test]
    fn the_captured_body_differs_from_the_sent_one_only_in_the_stream_flag() {
        let options = InferenceOptions {
            max_tokens: Some(256),
            temperature: Some(0.7),
            enable_thinking: false,
            ..Default::default()
        };
        let messages = [ChatMessage::user("what time is it?")];
        let sent = serde_json::to_value(MistralRsProvider::build_request(
            "default",
            "you are a duck",
            &messages,
            &[],
            &options,
            true,
        ))
        .unwrap();
        let mut captured = serde_json::to_value(MistralRsProvider::build_request(
            "default",
            "you are a duck",
            &messages,
            &[],
            &options,
            false,
        ))
        .unwrap();

        assert_eq!(captured["stream"], serde_json::json!(false));
        assert_eq!(sent["stream"], serde_json::json!(true));
        assert_eq!(captured["messages"][0]["role"], "system");
        assert_eq!(captured["messages"][0]["content"], "you are a duck");
        assert_eq!(captured["chat_template_kwargs"]["enable_thinking"], false);

        // Everything else identical, checked by making the one known
        // difference go away rather than by listing the fields — a field added
        // to `ChatRequest` is then covered without a second edit here.
        captured["stream"] = serde_json::json!(true);
        assert_eq!(captured, sent);
    }

    #[test]
    fn a_dispatcher_tools_json_string_is_passed_through_unchanged() {
        let options = InferenceOptions {
            tools_json_override: Some(
                r#"[{"type":"function","function":{"name":"a","description":"d","parameters":{}}}]"#
                    .into(),
            ),
            ..Default::default()
        };
        let arr = MistralRsProvider::tools_array(&options, &[]);
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["function"]["name"], "a");
    }

    /// A malformed override must degrade to a toolless turn, never abort it.
    #[test]
    fn a_malformed_tools_override_falls_back_to_the_definitions() {
        let options = InferenceOptions {
            tools_json_override: Some("not json".into()),
            ..Default::default()
        };
        let defs = vec![ToolDefinition {
            name: "giap-device__list_devices".into(),
            description: "list".into(),
            parameters_schema: serde_json::json!({"type":"object"}),
        }];
        let arr = MistralRsProvider::tools_array(&options, &defs);
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["function"]["name"], "giap-device__list_devices");
    }

    /// Fragments keyed by index accumulate into whole calls, and only the first
    /// fragment carries the name.
    #[test]
    fn tool_call_fragments_accumulate_by_index() {
        let mut pending = BTreeMap::new();
        let first: Delta = serde_json::from_str(
            r#"{"tool_calls":[{"index":0,"id":"c1","function":{"name":"t","arguments":"{\"a\":"}}]}"#,
        )
        .unwrap();
        let second: Delta =
            serde_json::from_str(r#"{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}"#)
                .unwrap();
        assert!(drain_delta(first, &mut pending).is_empty());
        assert!(drain_delta(second, &mut pending).is_empty());
        let call = pending.get(&0).unwrap();
        assert_eq!(call.id, "c1");
        assert_eq!(call.name, "t");
        assert_eq!(call.arguments, r#"{"a":1}"#);
    }

    /// Two concurrent calls must not merge into one set of arguments.
    #[test]
    fn two_tool_calls_in_one_turn_stay_separate() {
        let mut pending = BTreeMap::new();
        let d: Delta = serde_json::from_str(
            r#"{"tool_calls":[
                {"index":0,"id":"c1","function":{"name":"a","arguments":"{}"}},
                {"index":1,"id":"c2","function":{"name":"b","arguments":"{}"}}
            ]}"#,
        )
        .unwrap();
        drain_delta(d, &mut pending);
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[&0].name, "a");
        assert_eq!(pending[&1].name, "b");
    }

    #[test]
    fn thinking_and_text_are_separate_events() {
        let mut pending = BTreeMap::new();
        let d: Delta =
            serde_json::from_str(r#"{"reasoning_content":"hmm","content":"hello"}"#).unwrap();
        let events = drain_delta(d, &mut pending);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], MrEvent::Thinking(t) if t == "hmm"));
        assert!(matches!(&events[1], MrEvent::Text(t) if t == "hello"));
    }

    /// mistral.rs serves whatever it serves; tool calling is a property of the
    /// server, not of the model name.
    #[test]
    fn tool_calling_is_reported_regardless_of_the_model_name() {
        let p = MistralRsProvider::new("http://127.0.0.1:9002", "some-unknown-gguf");
        assert!(p.capabilities().tool_calling);
    }

    /// Nothing on this path reads `request.images`, so no model name, however much it looks
    /// like a vision model, may claim vision here.
    #[test]
    fn no_model_claims_vision_on_this_path() {
        for model in ["gemma-4-E2B-it", "llama3.2-vision:11b", "qwen2.5-vl:7b"] {
            let p = MistralRsProvider::new("http://127.0.0.1:9002", model);
            assert!(!p.capabilities().vision, "{model}");
        }
    }

    #[test]
    fn the_declared_context_window_reaches_capabilities() {
        let p = MistralRsProvider::new("http://127.0.0.1:9002/", "gemma-4-E2B-it")
            .with_context_window(8192);
        assert_eq!(p.capabilities().context_window_tokens, 8192);
        // And the trailing slash does not survive into request URLs.
        assert_eq!(p.base_url(), "http://127.0.0.1:9002");
    }
}
