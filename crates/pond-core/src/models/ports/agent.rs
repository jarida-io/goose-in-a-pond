use crate::models::domain::model_capabilities::ModelCapabilities;
use crate::models::services::context::prefix_cache::PrefixCacheState;
pub use crate::shared::domain::agent::{
    AgentRequest, AgentResponse, AgentStreamEvent, WarmupPhase,
};
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AgentError {
    #[error("General error: {0}")]
    General(String),
}

#[async_trait]
pub trait Agent: Send + Sync {
    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse>;

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>>;

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    /// Compact this session now, on the user's instruction; returns tokens retained if reported.
    /// `Ok(None)`: the backend has no manual compaction; the caller must say so, not claim success.
    async fn compact_session(&self, _session_id: &str) -> Result<Option<u32>> {
        Ok(None)
    }

    /// Call a tool by fully-qualified name (e.g. `giap-weather__get_current_weather`).
    /// Fallback for a model that emits tool calls as text markup, not the structured protocol.
    async fn call_tool(
        &self,
        _session_id: &str,
        _tool_name: &str,
        _args_json: &str,
    ) -> Result<String> {
        Err(anyhow::anyhow!("call_tool not supported by this agent"))
    }

    /// Release engine-side state for a GIAP session id. Call BEFORE dropping the
    /// `engine_session_map` pairing: after that the engine-side id is unrecoverable.
    async fn forget_session(&self, _session_id: &str) {}

    /// State of the static prompt prefix in the engine's KV cache, if tracked. With no visible
    /// cache return `None` (reads as `Warm`): a fake cold state triggers needless re-prefills.
    fn prefix_cache_state(&self) -> Option<PrefixCacheState> {
        None
    }

    /// Prefill the static prompt prefix into the engine's KV cache before the first message.
    /// `voice_mode` must match the surface warmed (its prompt differs); `progress` must not block.
    async fn prewarm(
        &self,
        _voice_mode: bool,
        progress: std::sync::Arc<dyn Fn(WarmupPhase) + Send + Sync>,
    ) {
        progress(WarmupPhase::Skipped {
            reason: "this agent keeps no prefix cache".to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoSessionStoreAgent;

    #[async_trait]
    impl Agent for NoSessionStoreAgent {
        async fn chat(&self, _request: AgentRequest) -> Result<AgentResponse> {
            unimplemented!("not exercised by this test")
        }

        async fn chat_stream(
            &self,
            _request: AgentRequest,
        ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
            unimplemented!("not exercised by this test")
        }
    }

    #[tokio::test]
    async fn forget_session_default_is_a_harmless_no_op_for_a_stateless_agent() {
        let agent: Box<dyn Agent> = Box::new(NoSessionStoreAgent);
        agent.forget_session("session-that-was-never-real").await;
    }

    /// Cold permits recompaction, so it must never be the default for mocks and HTTP agents.
    #[test]
    fn an_agent_with_no_prefix_cache_resolves_to_the_warm_posture() {
        use crate::models::services::context::prefix_cache::{CachePosture, PrefixCacheState};

        let agent: Box<dyn Agent> = Box::new(NoSessionStoreAgent);
        let state = agent.prefix_cache_state();
        assert!(state.is_none());
        assert_eq!(
            PrefixCacheState::posture_of(state.as_ref()),
            CachePosture::Warm
        );
    }
}
