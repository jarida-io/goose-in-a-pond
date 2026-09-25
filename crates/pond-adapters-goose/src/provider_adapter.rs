use anyhow::{anyhow, Result};
use async_trait::async_trait;
use goose::conversation::message::Message as GooseMessage;
use goose::providers::base::Provider as GooseProvider;
use goose_providers::model::ModelConfig;
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::models::ports::provider::LlmProvider;
use std::sync::Arc;

/// Exposes a Goose `Provider` as a pond `LlmProvider`, carrying the per-call `ModelConfig`.
pub struct GooseProviderAdapter {
    provider: Arc<dyn GooseProvider>,
    model_config: ModelConfig,
}

impl GooseProviderAdapter {
    pub fn new(provider: Arc<dyn GooseProvider>, model_config: ModelConfig) -> Self {
        Self {
            provider,
            model_config,
        }
    }

    fn to_goose_message(msg: &ChatMessage) -> GooseMessage {
        match msg.role {
            Role::User => GooseMessage::user().with_text(&msg.content),
            Role::Assistant => GooseMessage::assistant().with_text(&msg.content),
            Role::System => {
                // Goose has no System role (that text goes in `system`); fall back to User.
                GooseMessage::user().with_text(&msg.content)
            }
            Role::Tool => GooseMessage::user().with_text(&msg.content),
        }
    }

    fn from_goose_message(msg: &GooseMessage) -> ChatMessage {
        let role = match msg.role {
            rmcp::model::Role::User => Role::User,
            rmcp::model::Role::Assistant => Role::Assistant,
        };

        ChatMessage {
            role,
            content: msg.as_concat_text(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

#[async_trait]
impl LlmProvider for GooseProviderAdapter {
    fn capabilities(&self) -> pond_core::models::domain::model_capabilities::ModelCapabilities {
        pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
            &self.model_config.model_name,
        )
    }

    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        // System messages go in the `system` parameter instead.
        let goose_messages: Vec<GooseMessage> = messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(Self::to_goose_message)
            .collect();

        let (response, _usage) = self
            .provider
            .complete(
                &self.model_config,
                system_prompt,
                &goose_messages,
                &[], // no tools for direct completion
            )
            .await
            .map_err(|e| anyhow!("Goose provider error: {}", e))?;

        Ok(Self::from_goose_message(&response))
    }

    fn model_name(&self) -> String {
        self.model_config.model_name.clone()
    }
}
