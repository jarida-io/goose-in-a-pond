use anyhow::{anyhow, Result};
use async_trait::async_trait;
use goose::conversation::message::Message as GooseMessage;
use goose::providers::base::Provider as GooseProvider;
use goose_providers::model::ModelConfig;
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::models::ports::provider::LlmProvider;
use std::sync::Arc;

/// Bridges pond's `LlmProvider` port to Goose's `Provider` trait, converting between pond
/// `ChatMessage` and Goose `Message`. Goose providers are model-agnostic; the `ModelConfig`
/// selects the model per call, so this adapter carries it alongside the provider.
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

    /// Convert a pond ChatMessage into a Goose Message.
    fn to_goose_message(msg: &ChatMessage) -> GooseMessage {
        match msg.role {
            Role::User => GooseMessage::user().with_text(&msg.content),
            Role::Assistant => GooseMessage::assistant().with_text(&msg.content),
            Role::System => {
                // Goose doesn't have a System message role —
                // system content is passed as the `system` parameter to `complete()`.
                // We treat System messages as User messages with a note.
                GooseMessage::user().with_text(&msg.content)
            }
            Role::Tool => {
                // Tool results are treated as user messages in the Goose provider adapter.
                GooseMessage::user().with_text(&msg.content)
            }
        }
    }

    /// Convert a Goose Message back into a pond ChatMessage.
    ///
    /// # Why the thinking fallback is here
    ///
    /// `as_concat_text` filters the message's content blocks to `as_text()`,
    /// which drops `Thinking` blocks entirely. A reasoning-capable local model
    /// that puts its whole answer in the reasoning channel and leaves the text
    /// channel empty therefore hands its caller an EMPTY STRING, with
    /// `finish_reason` "stop" and nothing to say what went wrong.
    ///
    /// Measured, not hypothetical: 19 of 72 replies from Nemotron3-Nano-4B came
    /// through this function as empty strings during the extraction probe, and
    /// all 19 parsed perfectly from the reasoning field. The window was marked
    /// `Unparseable`, the cursor did not advance, and the correct answer was
    /// discarded 26% of the time.
    ///
    /// A FALLBACK and never a concatenation: when the model wrote text, that is
    /// the reply, and a model's private reasoning is not something to paste
    /// into a household's chat.
    fn from_goose_message(msg: &GooseMessage) -> ChatMessage {
        let role = match msg.role {
            rmcp::model::Role::User => Role::User,
            rmcp::model::Role::Assistant => Role::Assistant,
        };

        let text = msg.as_concat_text();
        let content = if text.trim().is_empty() {
            msg.content
                .iter()
                .filter_map(|c| c.as_thinking())
                .map(|t| t.thinking.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            text
        };

        ChatMessage {
            role,
            content,
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
        // Convert pond messages to Goose messages, filtering out System role
        // (system content goes into the `system` parameter instead)
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
