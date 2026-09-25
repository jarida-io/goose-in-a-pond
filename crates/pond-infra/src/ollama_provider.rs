//! Ollama LLM provider adapter.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::models::ports::provider::LlmProvider;
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
}

#[derive(Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    message: OllamaMessage,
}

// ── Adapter ───────────────────────────────────────────────────────────────────

pub struct OllamaProvider {
    client: Client,
    endpoint: String,
    model: String,
}

impl OllamaProvider {
    /// A `None` host or model falls back to [`DEFAULT_HOST`] / `DEFAULT_MODEL`.
    pub fn new(host: Option<&str>, model: Option<&str>) -> Self {
        let base = host.unwrap_or(DEFAULT_HOST).trim_end_matches('/');
        Self {
            client: Client::new(),
            endpoint: format!("{}/api/chat", base),
            model: model.unwrap_or(DEFAULT_MODEL).to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let mut ollama_messages: Vec<OllamaMessage> = vec![OllamaMessage {
            role: "system".to_string(),
            content: system_prompt.to_string(),
        }];

        for m in &messages {
            ollama_messages.push(OllamaMessage {
                role: match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::System => "system",
                }
                .to_string(),
                content: m.content.clone(),
            });
        }

        let body = ChatRequest {
            model: &self.model,
            messages: ollama_messages,
            stream: false,
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

    fn model_name(&self) -> String {
        self.model.clone()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
}
