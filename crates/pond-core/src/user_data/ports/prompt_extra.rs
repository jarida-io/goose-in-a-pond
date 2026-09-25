use crate::user_data::domain::prompt_extra::PromptExtra;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: keyed system-prompt extras, injected every turn by `sort_order`, then `key`.
#[async_trait]
pub trait PromptExtraRepository: Send + Sync {
    /// Return all active extras, in injection order.
    async fn list_active(&self) -> Result<Vec<PromptExtra>>;

    /// Return all extras (active and inactive).
    async fn list_all(&self) -> Result<Vec<PromptExtra>>;

    /// Insert or replace an extra (keyed by `key`).
    async fn upsert(&self, extra: &PromptExtra) -> Result<()>;

    /// Delete an extra by key.
    async fn delete(&self, key: &str) -> Result<()>;
}
