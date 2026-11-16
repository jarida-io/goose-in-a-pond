//! In-memory [`MemberAgentRepository`]: a member with nothing stored is on the shipped
//! defaults.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::specialist::{switches, AgentSwitch, MemberAgent, SpecialistId};
use crate::user_data::ports::member_agents::MemberAgentRepository;

#[derive(Default)]
pub struct InMemoryMemberAgentRepository {
    rows: Mutex<HashMap<(String, SpecialistId), MemberAgent>>,
}

impl InMemoryMemberAgentRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MemberAgentRepository for InMemoryMemberAgentRepository {
    async fn switches(&self, profile_id: &str) -> Result<Vec<MemberAgent>> {
        let stored: Vec<MemberAgent> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|((member, _), _)| member == profile_id)
            .map(|(_, row)| *row)
            .collect();
        Ok(switches(&stored))
    }

    async fn switch(&self, profile_id: &str, agent: SpecialistId) -> Result<AgentSwitch> {
        let all = self.switches(profile_id).await?;
        Ok(all
            .into_iter()
            .find(|row| row.agent == agent)
            .map(|row| row.state)
            .unwrap_or(AgentSwitch::Off))
    }

    async fn set(
        &self,
        profile_id: &str,
        agent: SpecialistId,
        state: AgentSwitch,
        at: DateTime<Utc>,
    ) -> Result<()> {
        self.rows.lock().unwrap().insert(
            (profile_id.to_string(), agent),
            MemberAgent {
                agent,
                state,
                changed_at: Some(at),
            },
        );
        Ok(())
    }
}
