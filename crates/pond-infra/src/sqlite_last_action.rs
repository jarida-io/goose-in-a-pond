//! SQLite-backed implementation of `LastActionRepository`. `before`/`after` are JSON.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::ports::last_action::{LastActionRepository, LastDeviceAction};

pub struct SqliteLastActionRepository {
    pool: Pool<Sqlite>,
}

impl SqliteLastActionRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl LastActionRepository for SqliteLastActionRepository {
    async fn last_for(&self, profile_id: &str) -> Result<Option<LastDeviceAction>> {
        let row = sqlx::query(
            "SELECT device_id, before, after, recorded_at FROM last_device_actions \
             WHERE profile_id = ?",
        )
        .bind(profile_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let recorded_at: String = row.try_get("recorded_at")?;
        Ok(Some(LastDeviceAction {
            device_id: row.try_get("device_id")?,
            before: serde_json::from_str(&row.try_get::<String, _>("before")?)?,
            after: serde_json::from_str(&row.try_get::<String, _>("after")?)?,
            recorded_at: DateTime::parse_from_rfc3339(&recorded_at)
                .map(|t| t.with_timezone(&Utc))
                .with_context(|| format!("bad last-action timestamp {recorded_at:?}"))?,
        }))
    }

    async fn record(&self, profile_id: &str, action: LastDeviceAction) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO last_device_actions \
             (profile_id, device_id, before, after, recorded_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(profile_id)
        .bind(&action.device_id)
        .bind(serde_json::to_string(&action.before)?)
        .bind(serde_json::to_string(&action.after)?)
        .bind(action.recorded_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn clear(&self, profile_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM last_device_actions WHERE profile_id = ?")
            .bind(profile_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use pond_core::user_data::ports::device_control::DeviceStatePatch;

    fn action(device_id: &str, before_on: bool, after_on: bool) -> LastDeviceAction {
        LastDeviceAction {
            device_id: device_id.to_string(),
            before: DeviceStatePatch {
                on: Some(before_on),
                ..Default::default()
            },
            after: DeviceStatePatch {
                on: Some(after_on),
                ..Default::default()
            },
            recorded_at: DateTime::parse_from_rfc3339("2026-11-03T09:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        }
    }

    async fn repo_with_profile() -> (SqliteLastActionRepository, Pool<Sqlite>, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('amina', 'Amina')")
            .execute(&db.system)
            .await
            .unwrap();
        (
            SqliteLastActionRepository::new(db.system.clone()),
            db.system,
            tmp,
        )
    }

    #[tokio::test]
    async fn an_action_round_trips_and_a_second_record_replaces_it() {
        let (repo, _pool, _tmp) = repo_with_profile().await;
        assert!(repo.last_for("amina").await.unwrap().is_none());

        let first = action("lamp-1", false, true);
        repo.record("amina", first.clone()).await.unwrap();
        assert_eq!(repo.last_for("amina").await.unwrap(), Some(first));

        // A second command replaces the first -- only the most recent is ever remembered.
        let second = action("lamp-2", true, false);
        repo.record("amina", second.clone()).await.unwrap();
        assert_eq!(repo.last_for("amina").await.unwrap(), Some(second));
    }

    #[tokio::test]
    async fn clearing_removes_it_and_undoing_twice_finds_nothing() {
        let (repo, _pool, _tmp) = repo_with_profile().await;
        repo.record("amina", action("lamp-1", false, true))
            .await
            .unwrap();
        repo.clear("amina").await.unwrap();
        assert!(repo.last_for("amina").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn deleting_the_member_deletes_their_remembered_action() {
        let (repo, pool, _tmp) = repo_with_profile().await;
        repo.record("amina", action("lamp-1", false, true))
            .await
            .unwrap();

        sqlx::query("DELETE FROM profiles WHERE id = 'amina'")
            .execute(&pool)
            .await
            .unwrap();

        assert!(repo.last_for("amina").await.unwrap().is_none());
    }
}
