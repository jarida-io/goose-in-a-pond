//! SQLite-backed [`MemberAgentRepository`] over the `member_agents` table. A missing row is
//! the specialist's shipped default.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Sqlite};

use pond_core::user_data::domain::specialist::{
    specialist, switches, AgentSwitch, MemberAgent, SpecialistId,
};
use pond_core::user_data::ports::member_agents::MemberAgentRepository;

use crate::sqlite_rows::{maybe_time_of, stamp, word_in};

pub struct SqliteMemberAgentRepository {
    pool: Pool<Sqlite>,
}

impl SqliteMemberAgentRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

fn row_of(row: &SqliteRow) -> Result<MemberAgent> {
    Ok(MemberAgent {
        agent: word_in(row, "agent", SpecialistId::parse)?,
        state: word_in(row, "state", AgentSwitch::parse)?,
        changed_at: maybe_time_of(row, "changed_at")?,
    })
}

#[async_trait]
impl MemberAgentRepository for SqliteMemberAgentRepository {
    async fn switches(&self, profile_id: &str) -> Result<Vec<MemberAgent>> {
        let rows =
            sqlx::query("SELECT agent, state, changed_at FROM member_agents WHERE profile_id = ?")
                .bind(profile_id)
                .fetch_all(&self.pool)
                .await
                .context("read a member's specialist switches")?;
        let stored = rows.iter().map(row_of).collect::<Result<Vec<_>>>()?;
        Ok(switches(&stored))
    }

    async fn switch(&self, profile_id: &str, agent: SpecialistId) -> Result<AgentSwitch> {
        let row = sqlx::query(
            "SELECT agent, state, changed_at FROM member_agents \
             WHERE profile_id = ? AND agent = ?",
        )
        .bind(profile_id)
        .bind(agent.as_str())
        .fetch_optional(&self.pool)
        .await
        .context("read a member's switch for one specialist")?;
        match row {
            Some(row) => Ok(row_of(&row)?.state),
            None => Ok(specialist(agent).default_switch()),
        }
    }

    async fn set(
        &self,
        profile_id: &str,
        agent: SpecialistId,
        state: AgentSwitch,
        at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO member_agents (profile_id, agent, state, changed_at) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT(profile_id, agent) DO UPDATE SET state = excluded.state, \
             changed_at = excluded.changed_at",
        )
        .bind(profile_id)
        .bind(agent.as_str())
        .bind(state.as_str())
        .bind(stamp(at))
        .execute(&self.pool)
        .await
        .context("set a member's specialist switch")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_rows::testing::{noon, pool};
    use pond_core::user_data::domain::specialist::SPECIALISTS;
    use pond_core::user_data::mocks::mock_member_agents::InMemoryMemberAgentRepository;

    async fn a_switch_is_the_default_until_set_and_is_one_members(
        repo: &dyn MemberAgentRepository,
    ) {
        let fresh = repo.switches("jerry").await.unwrap();
        assert_eq!(fresh.len(), SPECIALISTS.len());
        assert!(fresh.iter().all(|row| row.changed_at.is_none()));
        assert_eq!(
            repo.switch("jerry", SpecialistId::Budget).await.unwrap(),
            AgentSwitch::Off
        );
        assert_eq!(
            repo.switch("jerry", SpecialistId::Trip).await.unwrap(),
            AgentSwitch::On
        );

        repo.set("jerry", SpecialistId::Budget, AgentSwitch::Offered, noon())
            .await
            .unwrap();
        repo.set("jerry", SpecialistId::Budget, AgentSwitch::On, noon())
            .await
            .unwrap();
        repo.set("jerry", SpecialistId::Hunt, AgentSwitch::Off, noon())
            .await
            .unwrap();
        let set = repo.switches("jerry").await.unwrap();
        let budget = set
            .iter()
            .find(|r| r.agent == SpecialistId::Budget)
            .unwrap();
        assert_eq!(
            (budget.state, budget.changed_at),
            (AgentSwitch::On, Some(noon()))
        );
        assert_eq!(
            repo.switch("jerry", SpecialistId::Hunt).await.unwrap(),
            AgentSwitch::Off
        );
        assert_eq!(
            repo.switch("liz", SpecialistId::Budget).await.unwrap(),
            AgentSwitch::Off,
            "one member's switch is not another's"
        );
        assert_eq!(
            repo.switch("liz", SpecialistId::Hunt).await.unwrap(),
            AgentSwitch::On
        );
    }

    #[tokio::test]
    async fn a_specialist_is_on_its_default_until_a_member_switches_it() {
        let sqlite = SqliteMemberAgentRepository::new(pool().await);
        a_switch_is_the_default_until_set_and_is_one_members(&sqlite).await;
        a_switch_is_the_default_until_set_and_is_one_members(&InMemoryMemberAgentRepository::new())
            .await;
    }
}
