use async_trait::async_trait;

/// Snapshot of the LLM memory budget on the current device.
#[derive(Debug, Clone, Default)]
pub struct MemoryStatus {
    /// Total device RAM in MB (all uses combined).
    pub total_mb: u64,
    /// MB currently available for LLM loading.
    pub available_for_llm_mb: u64,
    /// Name of the model currently loaded in the LLM slot, if any.
    pub loaded_model: Option<String>,
}

/// Memory-aware model loading and eviction; a no-op for backends that manage their own memory.
#[async_trait]
pub trait ModelScheduler: Send + Sync {
    /// Wake word heard: may start preloading the chat model while the user is still speaking.
    async fn notify_wake_word(&self);

    fn memory_status(&self) -> MemoryStatus;
}
