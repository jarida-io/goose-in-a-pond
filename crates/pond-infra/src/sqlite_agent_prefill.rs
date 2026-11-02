//! SQLite-backed [`AgentPrefillRegistry`] over `agent_prefills` (migration 0066).

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use pond_core::shared::domain::agent_prefill::PrefillRecord;
use pond_core::shared::ports::agent_prefill::AgentPrefillRegistry;
use sqlx::{Pool, Sqlite};
use std::collections::HashMap;

pub struct SqliteAgentPrefillRegistry {
    pool: Pool<Sqlite>,
}

impl SqliteAgentPrefillRegistry {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

fn sql_ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_ts(agent: &str, raw: Option<String>) -> Result<Option<DateTime<Utc>>> {
    raw.map(|raw| {
        DateTime::parse_from_rfc3339(&raw)
            .map(|at| at.with_timezone(&Utc))
            .with_context(|| format!("unreadable prefill timestamp for agent {agent}: {raw}"))
    })
    .transpose()
}

fn checked(agent: &str) -> Result<()> {
    if agent.trim().is_empty() {
        bail!("a prefill row names its agent");
    }
    Ok(())
}

#[async_trait]
impl AgentPrefillRegistry for SqliteAgentPrefillRegistry {
    async fn get_all(&self) -> Result<HashMap<String, PrefillRecord>> {
        let rows: Vec<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT agent, definition_hash, compiled_at, last_used_at FROM agent_prefills",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|(agent, definition_hash, compiled_at, last_used_at)| {
                let record = PrefillRecord {
                    definition_hash,
                    compiled_at: parse_ts(&agent, compiled_at)?,
                    last_used_at: parse_ts(&agent, last_used_at)?,
                };
                Ok((agent, record))
            })
            .collect()
    }

    async fn note_compiled(
        &self,
        agent: &str,
        definition_hash: &str,
        at: DateTime<Utc>,
    ) -> Result<()> {
        checked(agent)?;
        sqlx::query(
            "INSERT INTO agent_prefills (agent, definition_hash, compiled_at) VALUES (?, ?, ?) \
             ON CONFLICT(agent) DO UPDATE SET definition_hash = excluded.definition_hash, \
             compiled_at = excluded.compiled_at",
        )
        .bind(agent)
        .bind(definition_hash)
        .bind(sql_ts(at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn note_used(&self, agent: &str, at: DateTime<Utc>) -> Result<()> {
        checked(agent)?;
        sqlx::query(
            "INSERT INTO agent_prefills (agent, definition_hash, last_used_at) VALUES (?, '', ?) \
             ON CONFLICT(agent) DO UPDATE SET last_used_at = excluded.last_used_at",
        )
        .bind(agent)
        .bind(sql_ts(at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn forget(&self, agent: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM agent_prefills WHERE agent = ?")
            .bind(agent)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn registry() -> (tempfile::TempDir, SqliteAgentPrefillRegistry) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        (tmp, SqliteAgentPrefillRegistry::new(db.system.clone()))
    }

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
    }

    #[tokio::test]
    async fn a_use_keeps_the_compile_and_a_compile_keeps_the_use() {
        let (_tmp, registry) = registry().await;
        registry
            .note_compiled("trip-planner", "abc", t(0))
            .await
            .unwrap();
        registry.note_used("trip-planner", t(60)).await.unwrap();
        let row = registry.get_all().await.unwrap()["trip-planner"].clone();
        assert_eq!(row.definition_hash, "abc");
        assert_eq!(row.compiled_at, Some(t(0)), "a use cleared the compile");
        assert_eq!(row.last_used_at, Some(t(60)));

        registry
            .note_compiled("trip-planner", "def", t(120))
            .await
            .unwrap();
        let row = registry.get_all().await.unwrap()["trip-planner"].clone();
        assert_eq!(row.definition_hash, "def");
        assert_eq!(row.compiled_at, Some(t(120)));
        assert_eq!(row.last_used_at, Some(t(60)), "a compile cleared the use");
    }

    #[tokio::test]
    async fn an_agent_used_before_it_was_compiled_has_no_hash() {
        let (_tmp, registry) = registry().await;
        registry.note_used("study-planner", t(0)).await.unwrap();
        let all = registry.get_all().await.unwrap();
        let row = &all["study-planner"];
        assert_eq!(row.definition_hash, "");
        assert_eq!(row.compiled_at, None);
        assert_eq!(row.last_used_at, Some(t(0)));
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn forgetting_removes_one_row_and_says_whether_there_was_one() {
        let (_tmp, registry) = registry().await;
        registry.note_compiled("a", "h", t(0)).await.unwrap();
        registry.note_compiled("b", "h", t(0)).await.unwrap();
        assert!(registry.forget("a").await.unwrap());
        assert!(!registry.forget("a").await.unwrap());
        let all = registry.get_all().await.unwrap();
        assert!(all.contains_key("b") && !all.contains_key("a"));
    }

    #[tokio::test]
    async fn a_blank_agent_and_an_unreadable_time_are_refused() {
        let (_tmp, registry) = registry().await;
        assert!(registry.note_used("  ", t(0)).await.is_err());
        assert!(registry.note_compiled("", "h", t(0)).await.is_err());
        sqlx::query(
            "INSERT INTO agent_prefills (agent, definition_hash, compiled_at) VALUES ('x', 'h', 'soon')",
        )
        .execute(&registry.pool)
        .await
        .unwrap();
        assert!(
            registry.get_all().await.is_err(),
            "an unreadable row must be an error, not a gap"
        );
    }
}
