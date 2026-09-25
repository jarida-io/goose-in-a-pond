//! `LlmProvider` over llamafile's OpenAI-compatible `/v1/chat/completions`, via plain `reqwest`
//! to avoid linking Goose (its v8/llama-cpp-2 conflict on Windows).

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::models::ports::provider::{LlmProvider, StreamToken, TokenStream, UsageStats};
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub const DEFAULT_HOST: &str = "http://127.0.0.1:8080";

/// Stop tokens some models (e.g. Gemma) leave in their output.
const STOP_TOKENS: &[&str] = &["<end_of_turn>", "<|eot_id|>", "<|im_end|>"];

fn strip_stop_tokens(mut s: String) -> String {
    loop {
        let trimmed = s.trim_end();
        let mut changed = false;
        for tok in STOP_TOKENS {
            if let Some(without) = trimmed.strip_suffix(tok) {
                s = without.trim_end().to_string();
                changed = true;
                break;
            }
        }
        if !changed {
            return s.trim_end().to_string();
        }
    }
}

/// Model name that llamafile reports in its responses.
pub const DEFAULT_MODEL: &str = "LLaMA_CPP";

// ── OpenAI-compatible request/response types ──────────────────────────────────

#[derive(Serialize)]
struct CompletionRequest<'a> {
    model: &'a str,
    messages: Vec<OaiMessage<'a>>,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Serialize)]
struct OaiMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct CompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: OaiResponseMessage,
}

#[derive(Deserialize)]
struct OaiResponseMessage {
    content: String,
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct LlamafileProvider {
    client: Client,
    endpoint: String,
    model: String,
    max_tokens: u32,
    temperature: f32,
}

impl LlamafileProvider {
    /// `host` is a base URL without the API path; `None` means `DEFAULT_HOST`.
    pub fn new(host: Option<&str>) -> Self {
        let base = host.unwrap_or(DEFAULT_HOST);
        Self {
            client: Client::new(),
            endpoint: format!("{}/v1/chat/completions", base),
            model: DEFAULT_MODEL.to_string(),
            max_tokens: 1024,
            temperature: 0.7,
        }
    }

    /// Override the maximum number of tokens to generate (default: 1024).
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    /// Override the sampling temperature (default: 0.7).
    pub fn with_temperature(mut self, t: f32) -> Self {
        self.temperature = t;
        self
    }

    fn build_oai_messages(system_prompt: &str, messages: &[ChatMessage]) -> Vec<serde_json::Value> {
        let mut oai = vec![serde_json::json!({ "role": "system", "content": system_prompt })];
        for m in messages {
            oai.push(serde_json::json!({
                "role": match m.role {
                    Role::User      => "user",
                    Role::Assistant => "assistant",
                    Role::System    => "system",
                    Role::Tool      => "tool",
                },
                "content": m.content,
            }));
        }
        oai
    }
}

#[async_trait]
impl LlmProvider for LlamafileProvider {
    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let mut oai: Vec<OaiMessage> = vec![OaiMessage {
            role: "system",
            content: system_prompt,
        }];
        for m in &messages {
            oai.push(OaiMessage {
                role: match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::System => "system",
                    Role::Tool => "tool",
                },
                content: &m.content,
            });
        }

        let body = CompletionRequest {
            model: &self.model,
            messages: oai,
            temperature: self.temperature,
            max_tokens: self.max_tokens,
        };

        let resp = self
            .client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("llamafile request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("llamafile error {}: {}", status, text));
        }

        let parsed: CompletionResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("llamafile response parse error: {}", e))?;

        let content = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| strip_stop_tokens(c.message.content))
            .ok_or_else(|| anyhow!("llamafile returned no choices"))?;

        Ok(ChatMessage::assistant(content))
    }

    fn model_name(&self) -> String {
        self.model.clone()
    }

    /// Overrides the default with native SSE streaming (`"stream": true`).
    fn stream_complete<'a>(
        &'a self,
        system_prompt: &'a str,
        messages: Vec<ChatMessage>,
    ) -> TokenStream<'a> {
        let client = self.client.clone();
        let endpoint = self.endpoint.clone();
        let model = self.model.clone();
        let temperature = self.temperature;
        let max_tokens = self.max_tokens;
        let oai_messages = Self::build_oai_messages(system_prompt, &messages);

        Box::pin(async_stream::stream! {
            let body = serde_json::json!({
                "model":       model,
                "messages":    oai_messages,
                "temperature": temperature,
                "max_tokens":  max_tokens,
                "stream":      true,
            });

            let resp = match client.post(&endpoint).json(&body).send().await {
                Ok(r)  => r,
                Err(e) => { yield Err(anyhow!("llamafile stream request failed: {}", e)); return; }
            };

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                yield Err(anyhow!("llamafile stream error {}: {}", status, text));
                return;
            }

            let mut byte_stream = resp.bytes_stream();
            let mut line_buf = String::new();

            while let Some(chunk) = byte_stream.next().await {
                let chunk = match chunk {
                    Ok(c)  => c,
                    Err(e) => { yield Err(anyhow!("llamafile stream read error: {}", e)); return; }
                };

                line_buf.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(pos) = line_buf.find('\n') {
                    let line = line_buf[..pos].trim_end_matches('\r').to_string();
                    line_buf = line_buf[pos + 1..].to_string();

                    if let Some(data) = line.strip_prefix("data: ") {
                        let data = data.trim();
                        if data == "[DONE]" {
                            return;
                        }
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                            if let Some(token) = v["choices"][0]["delta"]["content"].as_str() {
                                let token = strip_stop_tokens(token.to_string());
                                if !token.is_empty() {
                                    yield Ok(StreamToken::Text(token));
                                }
                            }
                            if let Some(usage) = v.get("usage") {
                                let prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
                                let completion_tokens = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;
                                if prompt_tokens > 0 || completion_tokens > 0 {
                                    yield Ok(StreamToken::Usage(UsageStats { prompt_tokens, completion_tokens, reasoning_tokens: None }));
                                }
                            }
                        }
                    }
                }
            }
        })
    }
}
