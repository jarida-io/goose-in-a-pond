use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
use thiserror::Error;

pub use crate::models::domain::message::ChatMessage;
use crate::models::domain::model_capabilities::ModelCapabilities;

#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("Provider error: {0}")]
    General(String),

    #[error("Model not available: {0}")]
    ModelNotAvailable(String),
}

/// Token usage reported by the model after a completion.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct UsageStats {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    /// Hidden reasoning tokens, estimated by GIAP via `TokenCounter` (never exact). Not subtracted
    /// from `completion_tokens`; `None` means not counted, not zero.
    pub reasoning_tokens: Option<u32>,
}

/// A single item emitted by [`TokenStream`].
#[derive(Debug, Clone)]
pub enum StreamToken {
    Text(String),
    /// Emitted once, as the LAST item, by providers that track usage; not part of the text.
    Usage(UsageStats),
}

pub type TokenStream<'a> = Pin<Box<dyn Stream<Item = Result<StreamToken>> + Send + 'a>>;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn complete(
        &self,
        system_prompt: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage>;

    /// The name of the underlying model (e.g. "llama-3.2-3b", "gpt-4o").
    fn model_name(&self) -> String;

    /// The default is the most conservative assumption, so an unknown model still works.
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    fn stream_complete<'a>(
        &'a self,
        system_prompt: &'a str,
        messages: Vec<ChatMessage>,
    ) -> TokenStream<'a> {
        Box::pin(async_stream::stream! {
            match self.complete(system_prompt, messages).await {
                Ok(msg)  => yield Ok(StreamToken::Text(msg.content.clone())),
                Err(e)   => yield Err(e),
            }
        })
    }
}

/// Stand-in for a selected-but-unavailable provider: fails loudly, never falls back silently.
pub struct UnavailableProvider {
    message: String,
}

impl UnavailableProvider {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[async_trait]
impl LlmProvider for UnavailableProvider {
    async fn complete(
        &self,
        _system_prompt: &str,
        _messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        Err(anyhow::anyhow!(self.message.clone()))
    }

    fn model_name(&self) -> String {
        "mesh (unavailable)".to_string()
    }
}
