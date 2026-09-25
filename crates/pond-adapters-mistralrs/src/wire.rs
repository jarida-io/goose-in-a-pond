//! OpenAI chat-completions types as mistral.rs speaks them. Quirks: Gemma 4's thinking is in
//! `delta.reasoning_content`; absent lists arrive as explicit `null` (see [`nullable`]); a
//! mid-stream failure is an `error` object inside a `200 OK` SSE body.

use serde::{Deserialize, Deserializer, Serialize};

/// Read a `null` field as its default; `#[serde(default)]` only covers a missing one.
fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

// ── Request ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: Vec<WireMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Raw tool specs, already OpenAI-shaped by `ToolDispatcher::tools_json`; not re-typed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<&'static str>,
    pub stream_options: StreamOptions,
    /// `{"enable_thinking": bool}` — the knob Gemma 4's template reads.
    pub chat_template_kwargs: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

/// One message; `content` is never `null` (some chat templates reject it on tool-call turns).
#[derive(Debug, Serialize)]
pub struct WireMessage {
    pub role: &'static str,
    pub content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<WireToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WireToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: WireFunctionCall,
}

#[derive(Debug, Serialize)]
pub struct WireFunctionCall {
    pub name: String,
    /// JSON-stringified, per the OpenAI schema — not a nested object.
    pub arguments: String,
}

// ── Streaming response ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct StreamChunk {
    #[serde(default, deserialize_with = "nullable")]
    pub choices: Vec<StreamChoice>,
    #[serde(default)]
    pub usage: Option<WireUsage>,
    /// See the module header: this arrives with HTTP 200.
    #[serde(default)]
    pub error: Option<WireError>,
}

#[derive(Debug, Deserialize, Default)]
pub struct StreamChoice {
    #[serde(default)]
    pub delta: Delta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Delta {
    #[serde(default)]
    pub content: Option<String>,
    /// Where Gemma 4's thinking actually lands.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// Sent as `null` on every frame that has no call — see the module header.
    #[serde(default, deserialize_with = "nullable")]
    pub tool_calls: Vec<DeltaToolCall>,
}

#[derive(Debug, Deserialize)]
pub struct DeltaToolCall {
    /// Which call this fragment belongs to. Absent on single-call streams.
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<DeltaFunction>,
}

#[derive(Debug, Deserialize)]
pub struct DeltaFunction {
    #[serde(default)]
    pub name: Option<String>,
    /// Arrives in fragments that must be concatenated before parsing.
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug, Deserialize, Default, Clone, Copy)]
pub struct WireUsage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
}

#[derive(Debug, Deserialize)]
pub struct WireError {
    #[serde(default)]
    pub message: String,
}

/// `GET /v1/models`; needed because mistral.rs 400s on any model name it doesn't serve.
#[derive(Debug, Deserialize)]
pub struct ModelsResponse {
    #[serde(default)]
    pub data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ModelEntry {
    #[serde(default)]
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_frame_inside_a_200_body_still_parses_as_an_error() {
        let raw = r#"{"error":{"message":"Internal server error.","type":"server_error"}}"#;
        let chunk: StreamChunk = serde_json::from_str(raw).expect("error frame must parse");
        assert_eq!(
            chunk.error.map(|e| e.message).as_deref(),
            Some("Internal server error.")
        );
    }

    /// Thinking rides `reasoning_content`; a chunk carrying only that is not empty.
    #[test]
    fn reasoning_content_is_read_from_its_own_field() {
        let raw = r#"{"choices":[{"delta":{"reasoning_content":"weighing it up"}}]}"#;
        let chunk: StreamChunk = serde_json::from_str(raw).unwrap();
        let delta = &chunk.choices[0].delta;
        assert!(delta.content.is_none());
        assert_eq!(delta.reasoning_content.as_deref(), Some("weighing it up"));
    }

    /// Only the first fragment of a call carries its name.
    #[test]
    fn tool_call_fragments_carry_an_index_and_a_partial_argument_string() {
        let raw = r#"{"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"call_1","function":{"name":"giap-weather__get","arguments":"{\"ci"}}
        ]}}]}"#;
        let chunk: StreamChunk = serde_json::from_str(raw).unwrap();
        let tc = &chunk.choices[0].delta.tool_calls[0];
        assert_eq!(tc.index, 0);
        assert_eq!(tc.id.as_deref(), Some("call_1"));
        let f = tc.function.as_ref().unwrap();
        assert_eq!(f.name.as_deref(), Some("giap-weather__get"));
        assert_eq!(f.arguments.as_deref(), Some("{\"ci"));
    }

    /// A frame copied verbatim off the wire; note its `"tool_calls":null`.
    #[test]
    fn a_frame_this_server_actually_sent_deserializes() {
        let raw = r#"{"id":"2","choices":[{"finish_reason":null,"index":0,"delta":{"content":"I","role":"assistant","tool_calls":null},"logprobs":null}],"created":1788992752,"model":"/models","system_fingerprint":"local","object":"chat.completion.chunk","usage":null}"#;
        let chunk: StreamChunk = serde_json::from_str(raw).expect("a real frame must parse");
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("I"));
        assert!(chunk.choices[0].delta.tool_calls.is_empty());
        assert!(chunk.usage.is_none());
    }

    #[test]
    fn explicit_nulls_read_as_empty_rather_than_failing() {
        let chunk: StreamChunk = serde_json::from_str(r#"{"choices":null,"usage":null}"#).unwrap();
        assert!(chunk.choices.is_empty());
    }

    /// A usage-only frame (`stream_options.include_usage`) has no choices at all.
    #[test]
    fn a_usage_only_frame_has_no_choices() {
        let raw = r#"{"choices":[],"usage":{"prompt_tokens":7212,"completion_tokens":64}}"#;
        let chunk: StreamChunk = serde_json::from_str(raw).unwrap();
        assert!(chunk.choices.is_empty());
        assert_eq!(chunk.usage.unwrap().prompt_tokens, 7212);
    }

    #[test]
    fn an_assistant_message_with_no_text_still_sends_a_content_field() {
        let msg = WireMessage {
            role: "assistant",
            content: String::new(),
            tool_calls: vec![WireToolCall {
                id: "call_1".into(),
                kind: "function",
                function: WireFunctionCall {
                    name: "giap-device__list".into(),
                    arguments: "{}".into(),
                },
            }],
            tool_call_id: None,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json.get("content").and_then(|c| c.as_str()), Some(""));
        assert!(json.get("tool_call_id").is_none());
    }
}
