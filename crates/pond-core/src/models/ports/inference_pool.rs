use crate::models::domain::message::ChatMessage;
use anyhow::Result;
use async_trait::async_trait;

/// Lower ordinal = higher priority; matters only when the backend serializes requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskPriority {
    /// Main chat — user is waiting for first token.
    Interactive = 0,
    /// Tool classification — gates the main chat prompt.
    Classifier = 1,
    /// Answer review — runs after main chat completes.
    Review = 2,
    /// Background tasks — memory extraction, consolidation.
    Background = 3,
}

pub struct InferenceResult {
    pub response: ChatMessage,
}

/// Runs completion tasks on one shared `LlmProvider`, bounded by the adapter's semaphore.
/// HTTP backends run in parallel; GGUF serializes behind Goose's model mutex.
#[async_trait]
pub trait InferencePool: Send + Sync {
    /// Returns when inference completes; `task_id` is for logging/tracing only.
    async fn submit(
        &self,
        task_id: &str,
        system_prompt: String,
        messages: Vec<ChatMessage>,
        priority: TaskPriority,
    ) -> Result<InferenceResult>;

    /// Maximum concurrent requests this pool supports.
    fn concurrency(&self) -> usize;
}
