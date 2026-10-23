//! SQLite-backed [`NotificationRecipients`]: phone rows in `devices` with an unrevoked token.

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::ports::notification_recipients::{NotificationRecipients, PHONE_CLIENT_TYPE};
use sqlx::{Pool, Sqlite};

pub struct SqliteNotificationRecipients {
    pool: Pool<Sqlite>,
}

impl SqliteNotificationRecipients {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl NotificationRecipients for SqliteNotificationRecipients {
    async fn paired_phones(&self) -> Result<Vec<String>> {
        // A revoked pairing (unpair, logout) keeps its device row; only a live token counts.
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT d.id FROM devices d \
             WHERE d.device_type = ? \
               AND EXISTS (SELECT 1 FROM session_tokens t \
                           WHERE t.device_id = d.id AND t.revoked_at IS NULL) \
             ORDER BY d.created_at, d.id",
        )
        .bind(PHONE_CLIENT_TYPE)
        .fetch_all(&self.pool)
        .await?;
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    async fn insert_device(pool: &Pool<Sqlite>, id: &str, device_type: &str) {
        sqlx::query("INSERT INTO devices (id, name, device_type) VALUES (?, ?, ?)")
            .bind(id)
            .bind(id)
            .bind(device_type)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn insert_token(pool: &Pool<Sqlite>, device_id: &str, revoked: bool) {
        sqlx::query(
            "INSERT INTO session_tokens (token_hash, refresh_hash, device_id, client_id, \
             client_type, created_at, expires_at, refresh_expires_at, last_seen_at, revoked_at) \
             VALUES (?, ?, ?, ?, 'x', '', '', '', '', ?)",
        )
        .bind(format!("hash-{device_id}"))
        .bind(format!("refresh-{device_id}"))
        .bind(device_id)
        .bind(device_id)
        .bind(revoked.then_some("2026-10-01T00:00:00Z"))
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn only_phones_with_a_live_pairing_are_recipients() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let pool = db.system.clone();

        insert_device(&pool, "phone-paired", PHONE_CLIENT_TYPE).await;
        insert_token(&pool, "phone-paired", false).await;
        insert_device(&pool, "phone-unpaired", PHONE_CLIENT_TYPE).await;
        insert_token(&pool, "phone-unpaired", true).await;
        insert_device(&pool, "phone-never-paired", PHONE_CLIENT_TYPE).await;
        insert_device(&pool, "desktop", "desktop").await;
        insert_token(&pool, "desktop", false).await;
        insert_device(&pool, "matter-3", "light").await;

        let recipients = SqliteNotificationRecipients::new(pool);
        assert_eq!(
            recipients.paired_phones().await.unwrap(),
            vec!["phone-paired".to_string()],
            "a desktop sees notices live, and a revoked or never-paired phone cannot fetch them"
        );
    }
}
