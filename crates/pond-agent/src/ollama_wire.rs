//! Ollama `/api/chat` wire types, deliberately independent of `pond-adapters-ollama`.

use serde::{Deserialize, Serialize};

// ── Request types ────────────────────────────────────────────────────────────

/// Body for `POST /api/chat`.
#[derive(Serialize, Debug)]
pub struct OllamaChatRequest<'a> {
    pub model: &'a str,
    pub messages: Vec<OllamaMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<OllamaOptions>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<OllamaTool>,
}

/// Inference options passed via the `options` field.
#[derive(Serialize, Clone, Debug)]
pub struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_predict: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

// ── Message types ────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OllamaMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OllamaToolCall {
    pub function: OllamaFunctionCall,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OllamaFunctionCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

// ── Tool definition types ────────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug)]
pub struct OllamaTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: OllamaFunctionDef,
}

#[derive(Serialize, Clone, Debug)]
pub struct OllamaFunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ── Streaming response types ─────────────────────────────────────────────────

/// One NDJSON line of a streaming `/api/chat` response; the last has `done` and token counts.
#[derive(Deserialize, Debug)]
pub struct OllamaStreamChunk {
    /// Partial message — may contain text content or tool calls.
    #[serde(default)]
    pub message: Option<OllamaMessage>,
    /// True on the final chunk (end of generation).
    #[serde(default)]
    pub done: bool,
    /// Prompt token count (only on the final chunk).
    #[serde(default)]
    pub prompt_eval_count: u32,
    /// Completion token count (only on the final chunk).
    #[serde(default)]
    pub eval_count: u32,
}

// ── Pull request ─────────────────────────────────────────────────────────────

/// Body for `POST /api/pull`.
#[derive(Serialize, Debug)]
pub struct OllamaPullRequest<'a> {
    pub model: &'a str,
    pub stream: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_serializes_without_empty_tools() {
        let req = OllamaChatRequest {
            model: "gemma4:latest",
            messages: vec![OllamaMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
                tool_calls: None,
            }],
            stream: true,
            options: None,
            tools: vec![],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("tools"), "empty tools should be skipped");
        assert!(!json.contains("options"), "None options should be skipped");
    }

    #[test]
    fn stream_chunk_deserializes_with_text() {
        let json = r#"{"message":{"role":"assistant","content":"Hi"},"done":false}"#;
        let chunk: OllamaStreamChunk = serde_json::from_str(json).unwrap();
        assert!(!chunk.done);
        let msg = chunk.message.unwrap();
        assert_eq!(msg.content, "Hi");
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn stream_chunk_deserializes_with_tool_calls() {
        let json = r#"{
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "function": {
                        "name": "get_weather",
                        "arguments": {"location": "Nairobi"}
                    }
                }]
            },
            "done": false
        }"#;
        let chunk: OllamaStreamChunk = serde_json::from_str(json).unwrap();
        let msg = chunk.message.unwrap();
        let calls = msg.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "get_weather");
    }

    #[test]
    fn stream_chunk_deserializes_done_with_usage() {
        let json = r#"{"done":true,"prompt_eval_count":42,"eval_count":100}"#;
        let chunk: OllamaStreamChunk = serde_json::from_str(json).unwrap();
        assert!(chunk.done);
        assert_eq!(chunk.prompt_eval_count, 42);
        assert_eq!(chunk.eval_count, 100);
        assert!(chunk.message.is_none());
    }

    #[test]
    fn tool_definition_serializes_correctly() {
        let tool = OllamaTool {
            tool_type: "function".to_string(),
            function: OllamaFunctionDef {
                name: "get_weather".to_string(),
                description: "Get current weather".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "location": {"type": "string"}
                    },
                    "required": ["location"]
                }),
            },
        };
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["type"], "function");
        assert_eq!(json["function"]["name"], "get_weather");
    }
}
