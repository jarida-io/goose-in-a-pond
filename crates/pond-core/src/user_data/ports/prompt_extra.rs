use crate::user_data::domain::prompt_extra::PromptExtra;
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: keyed prompt extras, injected every turn by `sort_order`, then `key`. A row is the
/// household's when its `profile_id` is `None`, and otherwise that one member's own.
#[async_trait]
pub trait PromptExtraRepository: Send + Sync {
    /// The household's active extras, in injection order; never a member's.
    async fn list_active(&self) -> Result<Vec<PromptExtra>>;

    /// The household's extras, active and inactive; never a member's.
    async fn list_all(&self) -> Result<Vec<PromptExtra>>;

    /// One member's own extras, active and inactive, in injection order.
    async fn list_for_profile(&self, profile_id: &str) -> Result<Vec<PromptExtra>>;

    /// Every member's extras, so a household write can be checked against each member's turns.
    async fn list_all_members(&self) -> Result<Vec<PromptExtra>>;

    /// Insert or replace an extra, keyed by its owner and `key`.
    async fn upsert(&self, extra: &PromptExtra) -> Result<()>;

    /// Insert or replace several extras at once: all of them land, or none do.
    async fn upsert_all(&self, extras: &[PromptExtra]) -> Result<()>;

    /// Delete a household extra by key.
    async fn delete(&self, key: &str) -> Result<()>;

    /// Delete one member's extra by key.
    async fn delete_for_profile(&self, profile_id: &str, key: &str) -> Result<()>;

    /// Delete every extra a member owns, when the member is removed; returns how many went.
    async fn delete_all_for_profile(&self, profile_id: &str) -> Result<u64>;
}
