use crate::models::ports::agent::{Agent, AgentRequest, AgentResponse, AgentStreamEvent};
use crate::user_data::domain::memory::MemoryFragment;
use crate::user_data::ports::memory_repository::MemoryRepository;
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

/// Test mock agent that reads and writes to a `MemoryRepository`.
///
/// Recognises two message patterns:
/// - `"remember <fact>"` → saves `fact` to the repository and returns an ack.
/// - anything else       → returns all stored memories so the test can assert recall.
///
/// The same `Arc<dyn MemoryRepository>` is shared with `AppState` (desktop path)
/// or held by the test directly (CLI path), so both turns see the same data.
pub struct MemoryAwareAgent {
    memory_repo: Arc<dyn MemoryRepository>,
}

impl MemoryAwareAgent {
    pub fn new(memory_repo: Arc<dyn MemoryRepository>) -> Self {
        Self { memory_repo }
    }

    async fn process(&self, message: &str, session_id: &str) -> String {
        let lower = message.to_lowercase();
        if let Some(_) = lower.strip_prefix("remember ") {
            let fact = message["remember ".len()..].trim_end_matches('.').trim().to_string();
            let fragment = MemoryFragment::from_chat(
                uuid::Uuid::new_v4().to_string(),
                None,
                Some(session_id.to_string()),
                fact.clone(),
            );
            let _ = self.memory_repo.add(fragment).await;
            format!("Got it, I'll remember: {}", fact)
        } else {
            let memories = self
                .memory_repo
                .search_recent(None, 10)
                .await
                .unwrap_or_default();
            if memories.is_empty() {
                "I don't have any memories stored.".to_string()
            } else {
                let facts: Vec<&str> = memories.iter().map(|m| m.content.as_str()).collect();
                format!("From my memory: {}", facts.join("; "))
            }
        }
    }
}

#[async_trait]
impl Agent for MemoryAwareAgent {
    async fn chat(&self, request: AgentRequest) -> Result<AgentResponse> {
        let text = self.process(&request.message, &request.session_id).await;
        Ok(AgentResponse {
            text,
            metadata: HashMap::new(),
        })
    }

    async fn chat_stream(
        &self,
        request: AgentRequest,
    ) -> Result<BoxStream<'static, Result<AgentStreamEvent>>> {
        let text = self.process(&request.message, &request.session_id).await;
        let session_id = request.session_id.clone();
        let model_role = request.model_role.clone();
        let stream = async_stream::stream! {
            yield Ok(AgentStreamEvent::Text { content: text });
            yield Ok(AgentStreamEvent::Done { session_id, model_role, usage: None });
        };
        Ok(stream.boxed())
    }
}
