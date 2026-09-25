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

    /// Where picture support stands for `model` under `provider`, as a pure read: header, size
    /// and sidecar at most, never a hash, a rename or a fetch, so a model list may call it per
    /// row. `None` means this agent does not know, and callers fail open to the adapter's own
    /// backstop rather than refuse on no information.
    fn vision_state(&self, _provider: &str, _model: &str) -> Option<EncoderState> {
        None
    }

    /// A model just arrived (download finished) or became the active chat model: start
    /// whatever it needs to be fully usable, such as its vision encoder, in the background.
    /// Must return at once and never fail the caller; the default has nothing to prepare.
    fn prepare_model(&self, _model: &str) {}

    /// Compact this session's history now, on the user's instruction.
    ///
    /// The engine owns compaction since GIAP stopped trimming, so this is a
    /// request to the engine rather than work GIAP does itself — the port
    /// exists because `pond-api` must not depend on the goose crate, and the
    /// hexagonal invariant is enforced by CI's fast-crate list, not by
    /// convention.
    ///
    /// Returns the tokens retained afterwards when the engine reports them.
    /// `Ok(None)` means the backend has no manual compaction and the caller
    /// should say so rather than claim a no-op succeeded.
    ///
    /// Deliberately NOT "compact if needed": the automatic axis belongs to the
    /// engine's own threshold. This is the explicit press.
    async fn compact_session(&self, _session_id: &str) -> Result<Option<u32>> {
        Ok(None)
    }

    /// Call a tool directly by its fully-qualified name (e.g. "giap-weather__get_current_weather").
    ///
    /// Fallback for a model that emits tool calls as text markup (`<|tool_call>...<tool_call|>`)
    /// instead of through the structured protocol. Errors if the tool is not found.
    async fn call_tool(
        &self,
        _session_id: &str,
        _tool_name: &str,
        _args_json: &str,
    ) -> Result<String> {
        Err(anyhow::anyhow!("call_tool not supported by this agent"))
    }

    /// Release any engine-side session state paired with a GIAP session id. Call this BEFORE the
    /// `engine_session_map` pairing is dropped: once gone, the engine-side id is unrecoverable and
    /// its state is unreachable garbage. It sits on the delete-session request path, so log and
    /// return rather than propagate; the default no-op is correct for an agent with no store.
    async fn forget_session(&self, _session_id: &str) {}

    /// The state of this agent's static prompt prefix in the engine's KV cache, when it tracks
    /// one (PAI-4 P5). Moving the prefix costs a re-prefill: 3.7 s on the Orin, measured.
    /// `None` is correct for an agent with no visible prefix cache: `posture_of(None)` is `Warm`,
    /// the narrowing answer, whereas a fabricated cold state would spend re-prefills mid-flight.
    fn prefix_cache_state(&self) -> Option<PrefixCacheState> {
        None
    }

    /// Precompile the static prompt prefix into the engine's KV cache before the first message:
    /// a cold turn otherwise pays model load plus preamble prefill (~12 s on the Mac at 61 tools,
    /// ~5 s on the Orin). `voice_mode` must match the surface warmed — the voice prompt renders
    /// its own section. `progress` must be cheap and non-blocking; failure is never propagated.
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

    /// Implements only the two required methods — proving `forget_session`
    /// compiles, is reachable through `dyn Agent`, and needs no session store
    /// (or session ID validity) to run: the whole point of the default.
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
        // Neither a real session store nor an existing session id is needed —
        // this must simply return, not panic and not err.
        agent.forget_session("session-that-was-never-real").await;
    }

    /// An agent that reports no prefix cache must resolve to the WARM posture, not the cold one.
    /// Cold is the permission to recompact, and handing it to every mock and HTTP path by default
    /// would widen P5 to "recompact whenever nobody said otherwise".
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

    /// An agent that does not implement picture support reports "unknown", which the API passes
    /// through, and has nothing to prepare. Reporting NotDeclared by default would refuse every
    /// image on every backend that simply has not been taught the port.
    #[test]
    fn an_agent_that_does_not_report_vision_is_unknown_and_prepares_nothing() {
        let agent: Box<dyn Agent> = Box::new(NoSessionStoreAgent);
        assert_eq!(agent.vision_state("local", "gemma-4-E2B-it"), None);
        agent.prepare_model("gemma-4-E2B-it");
    }
}
