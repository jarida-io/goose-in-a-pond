//! `InferenceProvider` over Ollama's NDJSON streaming chat, with native tool calls.
//! A 404 for the model triggers one `/api/pull` and a retry.

use crate::ollama_wire::{
    OllamaChatRequest, OllamaFunctionDef, OllamaMessage, OllamaOptions, OllamaPullRequest,
    OllamaStreamChunk, OllamaTool,
};
use anyhow::{anyhow, Result};
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::models::domain::model_capabilities::ModelCapabilities;
use pond_core::models::ports::inference::{
    ChatEvent, ChatEventStream, InferenceOptions, InferenceProvider, ToolDefinition,
};
use pond_core::models::ports::provider::UsageStats;
use tracing;

pub struct OllamaInferenceProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl OllamaInferenceProvider {
    /// `base_url` e.g. `"http://localhost:11434"`; `model` e.g. `"gemma4:latest"`.
    pub fn new(base_url: &str, model: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
        }
    }

    fn to_ollama_tools(tools: &[ToolDefinition]) -> Vec<OllamaTool> {
        tools
            .iter()
            .map(|t| OllamaTool {
                tool_type: "function".to_string(),
                function: OllamaFunctionDef {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters_schema.clone(),
                },
            })
            .collect()
    }

    /// Convert `ChatMessage` list to Ollama message format, prepending system prompt.
    fn to_ollama_messages(system_prompt: &str, messages: &[ChatMessage]) -> Vec<OllamaMessage> {
        let mut out = Vec::with_capacity(messages.len() + 1);
        out.push(OllamaMessage {
            role: "system".to_string(),
            content: system_prompt.to_string(),
            tool_calls: None,
        });
        for m in messages {
            out.push(OllamaMessage {
                role: match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::System => "system",
                    Role::Tool => "tool",
                }
                .to_string(),
                content: m.content.clone(),
                tool_calls: None,
            });
        }
        out
    }

    /// Pull (download) a model from Ollama. Blocks until complete.
    async fn pull_model(&self, model: &str) -> Result<()> {
        let url = format!("{}/api/pull", self.base_url);
        tracing::info!(model, "Ollama: pulling model -- this may take a while");

        let resp = self
            .client
            .post(&url)
            .json(&OllamaPullRequest {
                model,
                stream: false,
            })
            .send()
            .await
            .map_err(|e| anyhow!("Ollama pull request failed: {}", e))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Ollama pull failed: {}", text));
        }

        tracing::info!(model, "Ollama: model pulled successfully");
        Ok(())
    }

    /// Send a streaming chat request and return the raw response for chunk parsing.
    async fn send_streaming_request(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &InferenceOptions,
    ) -> Result<reqwest::Response> {
        let url = format!("{}/api/chat", self.base_url);

        let ollama_options = if options.max_tokens.is_some() || options.temperature.is_some() {
            Some(OllamaOptions {
                num_predict: options.max_tokens,
                temperature: options.temperature,
            })
        } else {
            None
        };

        let body = OllamaChatRequest {
            model: &self.model,
            messages: Self::to_ollama_messages(system_prompt, messages),
            stream: true,
            options: ollama_options,
            tools: Self::to_ollama_tools(tools),
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("Ollama unreachable -- is it running? ({})", e))?;

        if resp.status().is_success() {
            return Ok(resp);
        }

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();

        // Auto-pull on 404 / "not found"
        if status.as_u16() == 404 || text.contains("not found") || text.contains("no such model") {
            tracing::info!(
                model = %self.model,
                "Ollama: model not found -- attempting auto-pull"
            );
            self.pull_model(&self.model).await?;

            let retry_body = OllamaChatRequest {
                model: &self.model,
                messages: Self::to_ollama_messages(system_prompt, messages),
                stream: true,
                options: if options.max_tokens.is_some() || options.temperature.is_some() {
                    Some(OllamaOptions {
                        num_predict: options.max_tokens,
                        temperature: options.temperature,
                    })
                } else {
                    None
                },
                tools: Self::to_ollama_tools(tools),
            };

            let retry_resp = self
                .client
                .post(format!("{}/api/chat", self.base_url))
                .json(&retry_body)
                .send()
                .await
                .map_err(|e| anyhow!("Ollama unreachable after pull: {}", e))?;

            if !retry_resp.status().is_success() {
                let retry_text = retry_resp.text().await.unwrap_or_default();
                return Err(anyhow!("Ollama error after pull: {}", retry_text));
            }

            return Ok(retry_resp);
        }

        Err(anyhow!("Ollama error {}: {}", status, text))
    }
}

impl InferenceProvider for OllamaInferenceProvider {
    fn stream_chat(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &InferenceOptions,
    ) -> ChatEventStream {
        let system_prompt = system_prompt.to_string();
        let messages = messages.to_vec();
        let tools = tools.to_vec();
        let options = options.clone();

        let client = self.client.clone();
        let base_url = self.base_url.clone();
        let model = self.model.clone();

        Box::pin(async_stream::stream! {
            // Build a temporary provider to reuse send_streaming_request logic.
            let provider = OllamaInferenceProvider {
                client,
                base_url,
                model,
            };

            let resp = match provider
                .send_streaming_request(&system_prompt, &messages, &tools, &options)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            // Parse NDJSON: accumulate chunks into lines, parse each complete line.
            let mut line_buf = String::new();
            let mut response = resp;

            loop {
                match response.chunk().await {
                    Ok(Some(bytes)) => {
                        let text = match std::str::from_utf8(&bytes) {
                            Ok(t) => t,
                            Err(e) => {
                                yield Err(anyhow!("Invalid UTF-8 from Ollama: {}", e));
                                return;
                            }
                        };
                        line_buf.push_str(text);

                        while let Some(newline_pos) = line_buf.find('\n') {
                            let line = line_buf[..newline_pos].trim().to_string();
                            line_buf = line_buf[newline_pos + 1..].to_string();

                            if line.is_empty() {
                                continue;
                            }

                            let chunk: OllamaStreamChunk = match serde_json::from_str(&line) {
                                Ok(c) => c,
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        line = %line,
                                        "Failed to parse Ollama stream chunk"
                                    );
                                    continue;
                                }
                            };

                            if let Some(msg) = &chunk.message {
                                if !msg.content.is_empty() {
                                    yield Ok(ChatEvent::Text(msg.content.clone()));
                                }

                                if let Some(ref calls) = msg.tool_calls {
                                    for call in calls {
                                        yield Ok(ChatEvent::ToolCall {
                                            id: uuid::Uuid::new_v4().to_string(),
                                            name: call.function.name.clone(),
                                            arguments: call.function.arguments.clone(),
                                        });
                                    }
                                }
                            }

                            // Final chunk with usage stats.
                            if chunk.done {
                                if chunk.prompt_eval_count > 0 || chunk.eval_count > 0 {
                                    yield Ok(ChatEvent::Usage(UsageStats {
                                        prompt_tokens: chunk.prompt_eval_count,
                                        completion_tokens: chunk.eval_count,
                                        reasoning_tokens: None,
                                    }));
                                }
                                return;
                            }
                        }
                    }
                    Ok(None) => {
                        // Stream ended. Process any remaining buffered content.
                        let remaining = line_buf.trim().to_string();
                        if !remaining.is_empty() {
                            if let Ok(chunk) = serde_json::from_str::<OllamaStreamChunk>(&remaining) {
                                if let Some(msg) = &chunk.message {
                                    if !msg.content.is_empty() {
                                        yield Ok(ChatEvent::Text(msg.content.clone()));
                                    }
                                    if let Some(ref calls) = msg.tool_calls {
                                        for call in calls {
                                            yield Ok(ChatEvent::ToolCall {
                                                id: uuid::Uuid::new_v4().to_string(),
                                                name: call.function.name.clone(),
                                                arguments: call.function.arguments.clone(),
                                            });
                                        }
                                    }
                                }
                                if chunk.prompt_eval_count > 0 || chunk.eval_count > 0 {
                                    yield Ok(ChatEvent::Usage(UsageStats {
                                        prompt_tokens: chunk.prompt_eval_count,
                                        completion_tokens: chunk.eval_count,
                                        reasoning_tokens: None,
                                    }));
                                }
                            }
                        }
                        return;
                    }
                    Err(e) => {
                        yield Err(anyhow!("Ollama stream error: {}", e));
                        return;
                    }
                }
            }
        })
    }

    fn model_name(&self) -> String {
        self.model.clone()
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::from_model_name(&self.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_ollama_messages_prepends_system() {
        let msgs = vec![
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi there"),
        ];
        let result = OllamaInferenceProvider::to_ollama_messages("Be helpful.", &msgs);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].role, "system");
        assert_eq!(result[0].content, "Be helpful.");
        assert_eq!(result[1].role, "user");
        assert_eq!(result[2].role, "assistant");
    }

    #[test]
    fn to_ollama_tools_converts_definitions() {
        let defs = vec![ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get weather for a location".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"}
                }
            }),
        }];
        let tools = OllamaInferenceProvider::to_ollama_tools(&defs);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_type, "function");
        assert_eq!(tools[0].function.name, "get_weather");
    }

    #[test]
    fn model_name_returns_configured_model() {
        let p = OllamaInferenceProvider::new("http://localhost:11434", "gemma4:latest");
        assert_eq!(p.model_name(), "gemma4:latest");
    }

    #[test]
    fn capabilities_detected_from_model_name() {
        let p = OllamaInferenceProvider::new("http://localhost:11434", "gemma4:latest");
        let caps = p.capabilities();
        assert!(caps.thinking);
        assert!(caps.tool_calling);
    }
}
