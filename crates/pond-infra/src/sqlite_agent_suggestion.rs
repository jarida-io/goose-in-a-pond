//! SQLite-backed [`AgentSuggestionLedger`] over `agent_suggestion_dismissals` (migration 0064).

use anyhow::{bail, Result};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use pond_core::user_data::ports::agent_suggestion::AgentSuggestionLedger;
use sqlx::{Pool, Sqlite};
use std::collections::BTreeSet;

pub struct SqliteAgentSuggestionLedger {
    pool: Pool<Sqlite>,
}

impl SqliteAgentSuggestionLedger {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AgentSuggestionLedger for SqliteAgentSuggestionLedger {
    async fn dismiss(&self, template: &str, profile_id: &str, at: DateTime<Utc>) -> Result<()> {
        if template.trim().is_empty() || profile_id.trim().is_empty() {
            bail!("a dismissal names a template and a member");
        }
        sqlx::query(
            "INSERT INTO agent_suggestion_dismissals (template, profile_id, dismissed_at) \
             VALUES (?, ?, ?) \
             ON CONFLICT(template, profile_id) DO UPDATE SET dismissed_at = excluded.dismissed_at",
        )
        .bind(template)
        .bind(profile_id)
        .bind(at.to_rfc3339_opts(SecondsFormat::Secs, true))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn dismissed(&self, profile_id: &str) -> Result<BTreeSet<String>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT template FROM agent_suggestion_dismissals WHERE profile_id = ?")
                .bind(profile_id)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|(template,)| template).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn ledger_with_members(
        members: &[&str],
    ) -> (tempfile::TempDir, SqliteAgentSuggestionLedger) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        for id in members {
            sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
                .bind(id)
                .bind(id)
                .execute(&db.system)
                .await
                .unwrap();
        }
        (tmp, SqliteAgentSuggestionLedger::new(db.system.clone()))
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
    }

    #[tokio::test]
    async fn a_dismissal_is_kept_per_member_and_dismissing_again_moves_the_time() {
        let (_tmp, ledger) = ledger_with_members(&["jerry", "liz"]).await;
        ledger
            .dismiss("trip-planner", "jerry", at(0))
            .await
            .unwrap();
        ledger
            .dismiss("trip-planner", "jerry", at(60))
            .await
            .unwrap();
        ledger.dismiss("study-planner", "liz", at(0)).await.unwrap();
        assert_eq!(
            ledger.dismissed("jerry").await.unwrap(),
            ["trip-planner".to_string()].into_iter().collect()
        );
        assert_eq!(
            ledger.dismissed("liz").await.unwrap(),
            ["study-planner".to_string()].into_iter().collect()
        );
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM agent_suggestion_dismissals WHERE profile_id = ?")
                .bind("jerry")
                .fetch_one(&ledger.pool)
                .await
                .unwrap();
        assert_eq!(count, 1, "a second dismissal must not add a second row");
    }

    #[tokio::test]
    async fn a_departed_members_dismissals_go_with_them() {
        let (_tmp, ledger) = ledger_with_members(&["jerry"]).await;
        ledger
            .dismiss("trip-planner", "jerry", at(0))
            .await
            .unwrap();
        sqlx::query("DELETE FROM profiles WHERE id = ?")
            .bind("jerry")
            .execute(&ledger.pool)
            .await
            .unwrap();
        assert!(ledger.dismissed("jerry").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_dismissal_for_nobody_is_refused() {
        let (_tmp, ledger) = ledger_with_members(&[]).await;
        assert!(ledger.dismiss("trip-planner", "", at(0)).await.is_err());
        // The foreign key holds: a member who does not exist cannot decline anything.
        assert!(ledger
            .dismiss("trip-planner", "ghost", at(0))
            .await
            .is_err());
    }
}
