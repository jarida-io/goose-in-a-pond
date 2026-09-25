//! LLM provider fallback: any primary error retries on the fallback. Nest for multi-hop.

use crate::models::domain::message::ChatMessage;
use crate::models::ports::provider::LlmProvider;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

pub struct FallbackProvider {
    primary: Arc<dyn LlmProvider>,
    fallback: Arc<dyn LlmProvider>,
}

impl FallbackProvider {
    pub fn new(primary: Arc<dyn LlmProvider>, fallback: Arc<dyn LlmProvider>) -> Self {
        Self { primary, fallback }
    }
}

#[async_trait]
impl LlmProvider for FallbackProvider {
    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        match self.primary.complete(system_prompt, messages.clone()).await {
            Ok(response) => Ok(response),
            Err(e) => {
                tracing::warn!(
                    "Primary provider '{}' failed ({}), falling back to '{}'",
                    self.primary.model_name(),
                    e,
                    self.fallback.model_name(),
                );
                self.fallback.complete(system_prompt, messages).await
            }
        }
    }

    fn model_name(&self) -> String {
        format!(
            "{} → {}",
            self.primary.model_name(),
            self.fallback.model_name()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::mocks::mock_provider::MockProvider;

    struct FailingProvider;

    #[async_trait]
    impl LlmProvider for FailingProvider {
        async fn complete(&self, _: &str, _: Vec<ChatMessage>) -> Result<ChatMessage> {
            anyhow::bail!("simulated provider failure")
        }
        fn model_name(&self) -> String {
            "failing-provider".to_string()
        }
    }

    #[tokio::test]
    async fn uses_primary_when_it_succeeds() {
        let primary = Arc::new(MockProvider::new());
        let fallback = Arc::new(FailingProvider);
        let provider = FallbackProvider::new(primary, fallback);

        let result = provider
            .complete("sys", vec![ChatMessage::user("hello")])
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().content.contains("hello"));
    }

    #[tokio::test]
    async fn falls_back_when_primary_fails() {
        let primary = Arc::new(FailingProvider);
        let fallback = Arc::new(MockProvider::new());
        let provider = FallbackProvider::new(primary, fallback);

        let result = provider
            .complete("sys", vec![ChatMessage::user("fallback test")])
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().content.contains("fallback test"));
    }

    #[tokio::test]
    async fn fails_when_both_fail() {
        let primary = Arc::new(FailingProvider);
        let fallback = Arc::new(FailingProvider);
        let provider = FallbackProvider::new(primary, fallback);

        let result = provider
            .complete("sys", vec![ChatMessage::user("will fail")])
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn model_name_shows_chain() {
        let primary = Arc::new(MockProvider::new());
        let fallback = Arc::new(MockProvider::new());
        let provider = FallbackProvider::new(primary, fallback);
        let name = provider.model_name();
        assert!(name.contains(" → "), "expected arrow in '{}'", name);
    }
}
