//! Driven port: the shipped agents each member has declined, so their cards stay hidden for them.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;

#[async_trait]
pub trait AgentSuggestionLedger: Send + Sync {
    /// Record that `profile_id` declined `template`; declining again moves the time.
    async fn dismiss(&self, template: &str, profile_id: &str, at: DateTime<Utc>) -> Result<()>;

    /// The templates `profile_id` has declined.
    async fn dismissed(&self, profile_id: &str) -> Result<BTreeSet<String>>;
}
