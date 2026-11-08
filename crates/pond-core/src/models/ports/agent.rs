use crate::models::domain::conversation_model::NoConversationModel;
use crate::models::domain::model_capabilities::ModelCapabilities;
use crate::models::domain::vision_encoder::EncoderState;
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

    /// `Err` when no conversation model is chosen, so a turn is refused before anything is saved.
    /// An agent that needs no model choice (mocks, HTTP agents) always answers `Ok`.
    async fn ensure_conversation_model(&self) -> Result<(), NoConversationModel> {
        Ok(())
    }

    /// Picture support for `model`: a pure read (no hash, rename or fetch), safe per list row.
    /// `None` means unknown; callers then fail open to the adapter's own backstop.
    fn vision_state(&self, _provider: &str, _model: &str) -> Option<EncoderState> {
        None
    }

    /// Whether `model` hears voice notes, as the state of the encoder that would hear them.
    /// `None` means unknown, and a voice note is refused: no other path would carry it.
    fn audio_input_state(&self, _provider: &str, _model: &str) -> Option<EncoderState> {
        None
    }

    /// Whether a recording reaches `model`'s encoder this turn without loading it into memory the
    /// rest of the turn needs. Blocking: it reads the encoder's header.
    fn hearing_room(
        &self,
        _provider: &str,
        _model: &str,
    ) -> crate::models::domain::audio_input::HearingRoom {
        crate::models::domain::audio_input::HearingRoom::Unknown
    }

    /// Register a model whose files just arrived and settle its picture add-on: verify and
    /// attach what is on disk. Never downloads; must return at once and never fail the caller.
    fn prepare_model(&self, _model: &str) {}

    /// A model's file was deleted: forget every engine registration that names it.
    fn forget_model_file(&self, _path: &std::path::Path) {}

    /// Compact this session now, on the user's instruction; returns tokens retained if reported.
    /// `Ok(None)`: the backend has no manual compaction; the caller must say so, not claim success.
    async fn compact_session(&self, _session_id: &str) -> Result<Option<u32>> {
        Ok(None)
    }

    /// Whether this session's next turn would start by compacting it, by the backend's own rule.
    /// `false` when the backend never compacts on its own, or cannot tell.
    async fn compacts_on_next_turn(&self, _session_id: &str) -> bool {
        false
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

    /// Defaulting to NotDeclared would refuse every image on backends that lack the port.
    #[test]
    fn an_agent_that_does_not_report_vision_is_unknown_and_prepares_nothing() {
        let agent: Box<dyn Agent> = Box::new(NoSessionStoreAgent);
        assert_eq!(agent.vision_state("local", "gemma-4-E2B-it"), None);
        agent.prepare_model("gemma-4-E2B-it");
    }
}
