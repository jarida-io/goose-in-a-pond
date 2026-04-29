pub use crate::domain::agent::{AgentRequest, AgentResponse, AgentStreamEvent};
use crate::domain::model_capabilities::ModelCapabilities;
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AgentError {
    #[error("General error: {0}")]
    General(String),
}

/// Driven Port: Agent
///
/// This trait defines the interface for interacting with an AI agent.
#[async_trait]
pub trait Agent: Send + Sync {
    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse>;

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>>;

    /// Runtime capabilities of the model backing this agent.
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}
