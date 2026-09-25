//! Tool-calling chat completion for the agent loop; `LlmProvider` serves plain completions.

use crate::models::domain::message::ChatMessage;
use crate::models::domain::model_capabilities::ModelCapabilities;
use crate::models::ports::provider::UsageStats;
use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// Event emitted during a streaming chat completion.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    Text(String),
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// End-of-generation token usage stats.
    Usage(UsageStats),
}

#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
}

#[derive(Debug, Clone, Default)]
pub struct InferenceOptions {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Enable model thinking/reasoning (produces `<|channel>thought...<channel|>` tags).
    pub enable_thinking: bool,
    /// OpenAI-format tools JSON matching Goose's `format_tools()`; used verbatim when set.
    pub tools_json_override: Option<String>,
    /// Pre-formatted compact tools JSON (name + description only, no schemas).
    pub compact_tools_json_override: Option<String>,
}

pub type ChatEventStream = Pin<Box<dyn Stream<Item = Result<ChatEvent>> + Send>>;

/// LLM inference with native tool calling: HTTP (Ollama, llamafile) or in-process (GGUF).
#[async_trait]
pub trait InferenceProvider: Send + Sync {
    /// Emits [`ChatEvent::ToolCall`] only when `tools` is non-empty and the model supports tools.
    fn stream_chat(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &InferenceOptions,
    ) -> ChatEventStream;

    /// The name of the underlying model (e.g. `"gemma4:e4b"`).
    fn model_name(&self) -> String;

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}
