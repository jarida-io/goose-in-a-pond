//! Where a member's don't-list is kept.

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::dont_list::{DontRule, Strength};

/// Every read and write is one member's.
#[async_trait]
pub trait DontListRepository: Send + Sync {
    /// The member's rules of one strength, or all of them: `never` first, then newest.
    async fn list(&self, profile_id: &str, strength: Option<Strength>) -> Result<Vec<DontRule>>;

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<DontRule>>;

    /// Keeps a rule, merged into the one the member already has under its key, and returns it
    /// as stored.
    async fn add(&self, rule: &DontRule) -> Result<DontRule>;

    /// `false` when the member has no rule by that id.
    async fn delete(&self, profile_id: &str, id: &str) -> Result<bool>;
}
