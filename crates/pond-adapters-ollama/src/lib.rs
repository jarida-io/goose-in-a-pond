//! `LlmProvider` over a local Ollama `/api/chat`; a missing model is pulled, then retried.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::models::ports::provider::{LlmProvider, StreamToken, TokenStream, UsageStats};
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub const DEFAULT_HOST: &str = "http://localhost:11434";

pub const DEFAULT_MODEL: &str = "llama3.2";

// ── Ollama /api/chat request/response types ───────────────────────────────────

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<OllamaMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
}

#[derive(Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: OllamaMessage,
    #[serde(default)]
    prompt_eval_count: u32,
    #[serde(default)]
    eval_count: u32,
}

// ── Pull request ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct PullRequest<'a> {
    model: &'a str,
    stream: bool,
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct OllamaProvider {
    client: Client,
    base_url: String,
    endpoint: String,
    model: String,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
}

impl OllamaProvider {
    pub fn new(host: Option<&str>, model: Option<&str>) -> Self {
        let base = host.unwrap_or(DEFAULT_HOST).trim_end_matches('/');
        Self {
            client: Client::new(),
            base_url: base.to_string(),
            endpoint: format!("{}/api/chat", base),
            model: model.unwrap_or(DEFAULT_MODEL).to_string(),
            max_tokens: None,
            temperature: None,
        }
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    pub fn with_temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }

    /// Download a model; with `stream: false` Ollama answers once, when the pull is done.
    async fn pull_model(&self, model: &str) -> Result<()> {
        let url = format!("{}/api/pull", self.base_url);
        let body = PullRequest {
            model,
            stream: false,
        };

        tracing::info!(
            "Ollama: pulling model '{}' — this may take a while...",
            model
        );

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("Ollama pull request failed: {}", e))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Ollama pull failed: {}", text));
        }

        tracing::info!("Ollama: model '{}' pulled successfully", model);
        Ok(())
    }

    fn build_messages(&self, system_prompt: &str, messages: &[ChatMessage]) -> Vec<OllamaMessage> {
        let mut out = vec![OllamaMessage {
            role: "system".to_string(),
            content: system_prompt.to_string(),
        }];
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
            });
        }
        out
    }

    fn build_options(&self) -> Option<OllamaOptions> {
        if self.max_tokens.is_some() || self.temperature.is_some() {
            Some(OllamaOptions {
                num_predict: self.max_tokens,
                temperature: self.temperature,
            })
        } else {
            None
        }
    }

    async fn send_chat(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
    ) -> Result<ChatMessage> {
        let body = ChatRequest {
            model: &self.model,
            messages: self.build_messages(system_prompt, messages),
            stream: false,
            options: self.build_options(),
        };

        let resp = self
            .client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("Ollama unreachable — is it running? ({})", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Ollama error {}: {}", status, text));
        }

        let parsed: ChatResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("Ollama response parse error: {}", e))?;

        Ok(ChatMessage::assistant(parsed.message.content))
    }

    async fn send_chat_with_usage(
        &self,
        system_prompt: &str,
        messages: &[ChatMessage],
    ) -> Result<(ChatMessage, UsageStats)> {
        let body = ChatRequest {
            model: &self.model,
            messages: self.build_messages(system_prompt, messages),
            stream: false,
            options: self.build_options(),
        };

        let resp = self
            .client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("Ollama unreachable — is it running? ({})", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Ollama error {}: {}", status, text));
        }

        let parsed: ChatResponse = resp
            .json()
            .await
            .map_err(|e| anyhow!("Ollama response parse error: {}", e))?;

        let usage = UsageStats {
            prompt_tokens: parsed.prompt_eval_count,
            completion_tokens: parsed.eval_count,
            // Ollama's /api/chat reports no reasoning counter.
            reasoning_tokens: None,
        };
        Ok((ChatMessage::assistant(parsed.message.content), usage))
    }
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    fn capabilities(&self) -> pond_core::models::domain::model_capabilities::ModelCapabilities {
        pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
            &self.model,
        )
    }

    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        match self.send_chat(system_prompt, &messages).await {
            Ok(msg) => Ok(msg),
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("not found") || err_str.contains("no such model") {
                    tracing::info!(
                        "Ollama: model '{}' not found — attempting auto-pull...",
                        self.model
                    );
                    self.pull_model(&self.model).await?;

                    self.send_chat(system_prompt, &messages).await
                } else {
                    Err(e)
                }
            }
        }
    }

    fn model_name(&self) -> String {
        self.model.clone()
    }

    /// Overridden so usage stats are emitted.
    fn stream_complete<'a>(
        &'a self,
        system_prompt: &'a str,
        messages: Vec<ChatMessage>,
    ) -> TokenStream<'a> {
        Box::pin(async_stream::stream! {
            let result = match self.send_chat_with_usage(system_prompt, &messages).await {
                Ok(r) => r,
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("not found") || err_str.contains("no such model") {
                        if let Err(pull_err) = self.pull_model(&self.model).await {
                            yield Err(pull_err);
                            return;
                        }
                        match self.send_chat_with_usage(system_prompt, &messages).await {
                            Ok(r) => r,
                            Err(e2) => { yield Err(e2); return; }
                        }
                    } else {
                        yield Err(e);
                        return;
                    }
                }
            };
            let (msg, usage) = result;
            yield Ok(StreamToken::Text(msg.content));
            yield Ok(StreamToken::Usage(usage));
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper: spin up a mock Ollama server, send one turn, return the reply.
    async fn complete_with_mock(response_body: serde_json::Value) -> Result<ChatMessage> {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(Some(&server.uri()), Some("llama3.2"));
        provider
            .complete("You are helpful.", vec![ChatMessage::user("Hello")])
            .await
    }

    #[tokio::test]
    async fn returns_assistant_message_on_success() {
        let body = serde_json::json!({
            "model": "llama3.2",
            "message": { "role": "assistant", "content": "Hi there!" },
            "done": true
        });

        let reply = complete_with_mock(body).await.unwrap();
        assert_eq!(reply.role, Role::Assistant);
        assert_eq!(reply.content, "Hi there!");
    }

    #[tokio::test]
    async fn returns_error_on_non_200() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(Some(&server.uri()), Some("llama3.2"));
        let err = provider
            .complete("sys", vec![ChatMessage::user("hi")])
            .await
            .unwrap_err();

        assert!(err.to_string().contains("500"));
    }

    #[tokio::test]
    async fn returns_error_when_ollama_offline() {
        // Port 1 is reserved and will refuse connections immediately.
        let provider = OllamaProvider::new(Some("http://127.0.0.1:1"), Some("llama3.2"));
        let err = provider
            .complete("sys", vec![ChatMessage::user("hi")])
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("Ollama unreachable"),
            "unexpected error: {}",
            err
        );
    }

    #[tokio::test]
    async fn model_name_reflects_configured_model() {
        let provider = OllamaProvider::new(None, Some("gemma2"));
        assert_eq!(provider.model_name(), "gemma2");
    }

    #[tokio::test]
    async fn default_model_is_llama3_2() {
        let provider = OllamaProvider::new(None, None);
        assert_eq!(provider.model_name(), DEFAULT_MODEL);
    }

    // ── Auto-pull + inference tests ──────────────────────────────────────

    #[tokio::test]
    async fn auto_pulls_missing_model_then_completes_inference() {
        let server = MockServer::start().await;

        // First /api/chat call → 404 "model not found"
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_string(r#"{"error":"model 'test-model' not found"}"#),
            )
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;

        // Pull endpoint — should be called exactly once
        Mock::given(method("POST"))
            .and(path("/api/pull"))
            .and(body_partial_json(
                serde_json::json!({"model": "test-model"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "success"
            })))
            .expect(1)
            .mount(&server)
            .await;

        // After pull, the retry /api/chat should succeed
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "test-model",
                "message": { "role": "assistant", "content": "I'm ready after being pulled!" },
                "done": true
            })))
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(Some(&server.uri()), Some("test-model"));
        let reply = provider
            .complete("You are helpful.", vec![ChatMessage::user("Hello")])
            .await
            .unwrap();

        assert_eq!(reply.role, Role::Assistant);
        assert_eq!(reply.content, "I'm ready after being pulled!");
    }

    #[tokio::test]
    async fn does_not_pull_on_other_errors() {
        let server = MockServer::start().await;

        // Return a 500 error — should NOT trigger a pull
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(500).set_body_string("GPU out of memory"))
            .mount(&server)
            .await;

        // Pull endpoint — should NOT be called
        Mock::given(method("POST"))
            .and(path("/api/pull"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(Some(&server.uri()), Some("test-model"));
        let err = provider
            .complete("sys", vec![ChatMessage::user("hi")])
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("500"),
            "expected 500 error, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn inference_succeeds_with_options() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "model": "llama3.2",
                "message": { "role": "assistant", "content": "Options work!" },
                "done": true
            })))
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(Some(&server.uri()), Some("llama3.2"))
            .with_max_tokens(512)
            .with_temperature(0.3);

        let reply = provider
            .complete("sys", vec![ChatMessage::user("test")])
            .await
            .unwrap();

        assert_eq!(reply.content, "Options work!");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["options"]["num_predict"], 512);
        assert_eq!(body["options"]["temperature"], 0.3);
    }
}
