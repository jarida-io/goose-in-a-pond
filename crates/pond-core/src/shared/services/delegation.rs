//! Sub-agent delegation primitive.
//!
//! `DelegatingAgent` is an `Agent` that classifies each incoming message with a
//! chain of matcher functions and dispatches to the first matching sub-agent,
//! falling back to a default handler when none match.
//!
//! **Shared context** is carried through the unchanged `session_id`: every
//! sub-agent that handles a delegated task participates in the same conversation
//! thread, so any session history consulted or persisted by either agent is
//! visible to both.
//!
//! # Example
//! ```text
//! DelegatingAgent
//!   ├── is_device_actionable(msg) → DeviceAgent
//!   └── fallback                  → AlertAckAgent
//! ```
//!
//! NOTE: this is a neutral routing primitive — it evaluates whatever matcher
//! closures the caller registers, and is intentionally NOT wired into the live
//! chat/tool path. Per the project's no-keyword-classification rule, do not wire
//! it with hardcoded keyword matchers to gate tool use or model behaviour; the
//! keyword matcher in the tests below is illustrative only.

use crate::models::ports::agent::{Agent, AgentRequest, AgentResponse, AgentStreamEvent};
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use std::sync::Arc;

type MatchFn = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// Routes each request to the first matching sub-agent, in insertion order, else `fallback`.
pub struct DelegatingAgent {
    routes: Vec<(MatchFn, Arc<dyn Agent>)>,
    fallback: Arc<dyn Agent>,
}

impl DelegatingAgent {
    pub fn new(fallback: Arc<dyn Agent>) -> Self {
        Self {
            routes: Vec::new(),
            fallback,
        }
    }

    /// Register a sub-agent that handles messages for which `matcher` returns true.
    pub fn route(
        mut self,
        matcher: impl Fn(&str) -> bool + Send + Sync + 'static,
        agent: Arc<dyn Agent>,
    ) -> Self {
        self.routes.push((Box::new(matcher), agent));
        self
    }

    fn select(&self, message: &str) -> &dyn Agent {
        for (matcher, agent) in &self.routes {
            if matcher(message) {
                return agent.as_ref();
            }
        }
        self.fallback.as_ref()
    }
}

#[async_trait]
impl Agent for DelegatingAgent {
    /// Dispatch the request unchanged, so the shared `session_id` is preserved.
    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
        self.select(&request.message).chat(request).await
    }

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
        self.select(&request.message).chat_stream(request).await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ports::agent::AgentStreamEvent;
    use futures::StreamExt; // for `.boxed()` on the test stubs' streams
    use std::collections::HashMap;

    // ── Minimal labelled agent — echos "<label>: <message>" ──────────────────

    struct LabelledAgent(&'static str);

    #[async_trait]
    impl Agent for LabelledAgent {
        async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
            Ok(AgentResponse {
                text: format!("{}: {}", self.0, request.message),
                metadata: {
                    let mut m = HashMap::new();
                    m.insert("handled_by".to_string(), self.0.to_string());
                    m
                },
            })
        }

        async fn chat_stream(
            &self,
            request: AgentRequest,
        ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
            let text = format!("{}: {}", self.0, request.message);
            let session_id = request.session_id.clone();
            let model_role = request.model_role.clone();
            let stream = async_stream::stream! {
                yield Ok(AgentStreamEvent::Text { content: text });
                yield Ok(AgentStreamEvent::Done { session_id, model_role, usage: None, stats: None });
            };
            Ok(stream.boxed())
        }
    }

    fn make_request(message: &str, session_id: &str) -> AgentRequest {
        AgentRequest {
            message: message.to_string(),
            session_id: session_id.to_string(),
            model_role: "chat".to_string(),
            images: vec![],
            voice_mode: false,
            canvas_mode: false,
            profile_scope: crate::user_data::domain::profile::ProfileScope::Household,
            profile_context: None,
            tool_group_allowlist: None,
            warmup: false,
        }
    }

    // ── Routing classifier used by both tests ─────────────────────────────────

    fn is_device_actionable(msg: &str) -> bool {
        let lower = msg.to_lowercase();
        [
            "motion detected",
            "lights",
            "temperature",
            "lock",
            "flood",
            "smoke",
        ]
        .iter()
        .any(|kw| lower.contains(kw))
    }

    // ── Concrete use case: alerts-triage agent ────────────────────────────────

    #[tokio::test]
    async fn alerts_triage_delegates_to_device_agent() {
        let session_id = "home-session-42";

        let device_agent = Arc::new(LabelledAgent("DeviceAgent"));
        let ack_agent = Arc::new(LabelledAgent("AlertAck"));

        let triage = DelegatingAgent::new(ack_agent).route(is_device_actionable, device_agent);

        let device_resp = triage
            .chat(make_request(
                "motion detected in living room — turn on the lights",
                session_id,
            ))
            .await
            .unwrap();

        assert!(
            device_resp.text.starts_with("DeviceAgent:"),
            "device-actionable alert should be handled by DeviceAgent; got: {:?}",
            device_resp.text
        );
        assert_eq!(
            device_resp.metadata.get("handled_by").map(|s| s.as_str()),
            Some("DeviceAgent")
        );

        let ack_resp = triage
            .chat(make_request(
                "storm warning: heavy rain expected this evening",
                session_id,
            ))
            .await
            .unwrap();

        assert!(
            ack_resp.text.starts_with("AlertAck:"),
            "informational alert should fall back to AlertAck; got: {:?}",
            ack_resp.text
        );
        assert_eq!(
            ack_resp.metadata.get("handled_by").map(|s| s.as_str()),
            Some("AlertAck")
        );
    }

    #[tokio::test]
    async fn session_id_is_shared_across_delegation() {
        let session_id = "shared-ctx-99";

        struct SessionEchoAgent;
        #[async_trait]
        impl Agent for SessionEchoAgent {
            async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
                Ok(AgentResponse {
                    text: format!("session={}", request.session_id),
                    metadata: HashMap::new(),
                })
            }
            async fn chat_stream(
                &self,
                request: AgentRequest,
            ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
                let text = format!("session={}", request.session_id);
                let sid = request.session_id.clone();
                let role = request.model_role.clone();
                let stream = async_stream::stream! {
                    yield Ok(AgentStreamEvent::Text { content: text });
                    yield Ok(AgentStreamEvent::Done { session_id: sid, model_role: role, usage: None, stats: None });
                };
                Ok(stream.boxed())
            }
        }

        let triage = DelegatingAgent::new(Arc::new(LabelledAgent("Fallback")))
            .route(is_device_actionable, Arc::new(SessionEchoAgent));

        let resp = triage
            .chat(make_request("motion detected — check locks", session_id))
            .await
            .unwrap();

        assert_eq!(
            resp.text,
            format!("session={}", session_id),
            "sub-agent must receive the originating session_id unchanged"
        );
    }

    #[tokio::test]
    async fn stream_routing_carries_session_id() {
        let session_id = "stream-session-7";

        let device_agent = Arc::new(LabelledAgent("DeviceAgent"));
        let ack_agent = Arc::new(LabelledAgent("AlertAck"));

        let triage = DelegatingAgent::new(ack_agent).route(is_device_actionable, device_agent);

        let mut stream = triage
            .chat_stream(make_request("flood sensor triggered", session_id))
            .await
            .unwrap();

        let mut text_content = String::new();
        let mut done_session_id = String::new();
        while let Some(ev) = stream.next().await {
            match ev.unwrap() {
                AgentStreamEvent::Text { content } => text_content = content,
                AgentStreamEvent::Done {
                    session_id: sid, ..
                } => done_session_id = sid,
                _ => {}
            }
        }

        assert!(
            text_content.starts_with("DeviceAgent:"),
            "flood sensor is device-actionable; got: {:?}",
            text_content
        );
        assert_eq!(
            done_session_id, session_id,
            "Done event must echo the originating session_id"
        );
    }
}
