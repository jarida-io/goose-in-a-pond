use anyhow::Result;
use async_trait::async_trait;
use crate::ports::agent::{Agent, AgentRequest, AgentResponse, AgentStreamEvent};
use futures::stream::{BoxStream, StreamExt};
use std::collections::HashMap;

/// Mock adapter for the Agent port.
///
/// Echoes back the user's message for testing the workflow loop
/// without a real LLM provider.
pub struct MockAgent;

impl MockAgent {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Agent for MockAgent {
    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
        // Simulate a tiny "thinking" delay
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

        Ok(AgentResponse {
            text: format!("Echo: {}", request.message),
            metadata: HashMap::new(),
        })
    }

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
        let response_text = format!("Echo: {}", request.message);
        let session_id = request.session_id.clone();
        let model_role = request.model_role.clone();

        let stream = async_stream::stream! {
            yield Ok(AgentStreamEvent::Status { content: "Mock agent thinking...".to_string() });
            yield Ok(AgentStreamEvent::Text { content: response_text });
            yield Ok(AgentStreamEvent::Done { session_id, model_role });
        };

        Ok(stream.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_agent_echoes_input() {
        let agent = MockAgent::new();
        let request = AgentRequest {
            message: "Hello, Pond!".to_string(),
            session_id: "test-session".to_string(),
            model_role: "chat".to_string(),
            images: Vec::new(),
        };
        let response = agent.chat(request).await.unwrap();
        assert_eq!(response.text, "Echo: Hello, Pond!");
        assert!(response.metadata.is_empty());
    }

    #[tokio::test]
    async fn mock_agent_streams_echo() {
        let agent = MockAgent::new();
        let request = AgentRequest {
            message: "Hello, Stream!".to_string(),
            session_id: "test-session".to_string(),
            model_role: "chat".to_string(),
            images: Vec::new(),
        };
        let mut stream = agent.chat_stream(request).await.unwrap();

        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.unwrap());
        }

        assert_eq!(events.len(), 3);
        match &events[0] {
            AgentStreamEvent::Status { content } => assert_eq!(content, "Mock agent thinking..."),
            _ => panic!("Expected Status event"),
        }
        match &events[1] {
            AgentStreamEvent::Text { content } => assert_eq!(content, "Echo: Hello, Stream!"),
            _ => panic!("Expected Text event"),
        }
        match &events[2] {
            AgentStreamEvent::Done { .. } => (),
            _ => panic!("Expected Done event"),
        }
    }
}
