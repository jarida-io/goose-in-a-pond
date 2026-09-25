use anyhow::Result;
use async_trait::async_trait;

use crate::models::domain::message::{ChatMessage, Role};
use crate::models::ports::provider::LlmProvider;

/// Mock LLM provider ("mock-v1") that echoes the last user message.
pub struct MockProvider;

impl MockProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    async fn complete(
        &self,
        _system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        // Simulate a tiny thinking delay
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let last_user_msg = messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.content.clone())
            .unwrap_or_else(|| "(no user message)".to_string());

        Ok(ChatMessage::assistant(format!(
            "Mock response to: {}",
            last_user_msg
        )))
    }

    fn model_name(&self) -> String {
        "mock-v1".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_provider_echoes_last_user_message() {
        let provider = MockProvider::new();
        let messages = vec![
            ChatMessage::user("Hello!"),
            ChatMessage::assistant("Hi there!"),
            ChatMessage::user("What's the weather?"),
        ];

        let response = provider
            .complete("You are helpful.", messages)
            .await
            .unwrap();

        assert_eq!(response.role, Role::Assistant);
        assert_eq!(response.content, "Mock response to: What's the weather?");
    }

    #[tokio::test]
    async fn mock_provider_handles_empty_messages() {
        let provider = MockProvider::new();
        let response = provider.complete("System", vec![]).await.unwrap();
        assert_eq!(response.content, "Mock response to: (no user message)");
    }

    #[test]
    fn mock_provider_model_name() {
        let provider = MockProvider::new();
        assert_eq!(provider.model_name(), "mock-v1");
    }
}
