//! Driven port: the registry of agents' compiled prefills (`agent_prefills`), keyed by agent name.

use crate::shared::domain::agent_prefill::PrefillRecord;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

#[async_trait]
pub trait AgentPrefillRegistry: Send + Sync {
    /// Every registered agent; an unreadable row is an error, not a gap.
    async fn get_all(&self) -> Result<HashMap<String, PrefillRecord>>;

    /// Record a compile of `agent` at `definition_hash`; its last use is kept.
    async fn note_compiled(
        &self,
        agent: &str,
        definition_hash: &str,
        at: DateTime<Utc>,
    ) -> Result<()>;

    /// Record a run of `agent`; its hash and last compile are kept, and an unknown agent gains a
    /// row with no hash.
    async fn note_used(&self, agent: &str, at: DateTime<Utc>) -> Result<()>;

    /// Drop `agent`'s row; `Ok(false)` if there was none.
    async fn forget(&self, agent: &str) -> Result<bool>;
}
