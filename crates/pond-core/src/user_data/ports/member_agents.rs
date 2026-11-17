//! Where a member's specialist switches are kept.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::specialist::{AgentSwitch, MemberAgent, SpecialistId};

/// Every read and write is one member's.
#[async_trait]
pub trait MemberAgentRepository: Send + Sync {
    /// Every shipped specialist's switch for the member: what they stored, else its default.
    async fn switches(&self, profile_id: &str) -> Result<Vec<MemberAgent>>;

    /// One specialist's switch for the member: what they stored, else its default.
    async fn switch(&self, profile_id: &str, agent: SpecialistId) -> Result<AgentSwitch>;

    async fn set(
        &self,
        profile_id: &str,
        agent: SpecialistId,
        state: AgentSwitch,
        at: DateTime<Utc>,
    ) -> Result<()>;
}
