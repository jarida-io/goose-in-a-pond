//! SQLite-backed [`NotificationRecipients`]: phone rows in `devices` with an unrevoked token.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use pond_core::mcp::ports::notification_recipients::{NotificationRecipients, PHONE_CLIENT_TYPE};
use pond_core::user_data::ports::idle_phones::{IdlePhone, IdlePhones};
use pond_core::user_data::ports::settings::SettingsRepository;
use sqlx::{Pool, Row, Sqlite};

pub struct SqliteNotificationRecipients {
    pool: Pool<Sqlite>,
    /// Where the household's "stop notifying a phone not seen for N days" is read from.
    settings: Option<Arc<dyn SettingsRepository>>,
}

impl SqliteNotificationRecipients {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self {
            pool,
            settings: None,
        }
    }

    /// Leave out a phone that has not connected for the household's chosen number of days.
    pub fn with_idle_limit(mut self, settings: Arc<dyn SettingsRepository>) -> Self {
        self.settings = Some(settings);
        self
    }

    /// The oldest last-seen stamp that still counts, in the form tokens are stamped in. `None`
    /// keeps every phone: no limit wired, a limit of 0, or settings that could not be read.
    async fn seen_since(&self) -> Option<String> {
        let days = match self.settings.as_ref()?.get().await {
            Ok(settings) => settings.unseen_phone_skip_days,
            Err(error) => {
                tracing::warn!(%error, "settings unreadable; every paired phone is notified");
                return None;
            }
        };
        (days > 0).then(|| (Utc::now() - Duration::days(i64::from(days))).to_rfc3339())
    }
}

#[async_trait]
impl NotificationRecipients for SqliteNotificationRecipients {
    async fn paired_phones(&self) -> Result<Vec<String>> {
        // A revoked pairing (unpair, logout) keeps its device row; only a live token counts. A
        // phone seen since the cutoff has a token touched since then; one never touched counts.
        let seen_since = self.seen_since().await;
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT d.id FROM devices d \
             WHERE d.device_type = ? AND d.is_test = 0 \
               AND EXISTS (SELECT 1 FROM session_tokens t \
                           WHERE t.device_id = d.id AND t.revoked_at IS NULL \
                             AND (? IS NULL OR t.last_seen_at IS NULL OR t.last_seen_at = '' \
                                  OR t.last_seen_at >= ?)) \
             ORDER BY d.created_at, d.id",
        )
        .bind(PHONE_CLIENT_TYPE)
        .bind(&seen_since)
        .bind(&seen_since)
        .fetch_all(&self.pool)
        .await?;
        Ok(ids)
    }
}

#[async_trait]
impl IdlePhones for SqliteNotificationRecipients {
    async fn unseen_since(&self, cutoff: DateTime<Utc>) -> Result<Vec<IdlePhone>> {
        // The newest touch across a phone's live sessions is when it was last here.
        let rows = sqlx::query(
            "SELECT d.id, d.name, d.profile_id, MAX(t.last_seen_at) AS last_seen \
             FROM devices d JOIN session_tokens t ON t.device_id = d.id \
             WHERE d.device_type = ? AND t.revoked_at IS NULL \
             GROUP BY d.id \
             HAVING MAX(COALESCE(t.last_seen_at, '')) != '' AND MAX(t.last_seen_at) < ? \
             ORDER BY last_seen, d.id",
        )
        .bind(PHONE_CLIENT_TYPE)
        .bind(cutoff.to_rfc3339())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let last_seen: String = row.get("last_seen");
                Some(IdlePhone {
                    device_id: row.get("id"),
                    name: row.get("name"),
                    profile_id: row.get("profile_id"),
                    last_seen: DateTime::parse_from_rfc3339(&last_seen)
                        .ok()?
                        .with_timezone(&Utc),
                })
            })
            .collect())
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

    struct SkipAfter(u32);
    #[async_trait]
    impl SettingsRepository for SkipAfter {
        async fn get(&self) -> Result<pond_core::user_data::domain::settings::Settings> {
            let mut settings = pond_core::user_data::domain::settings::Settings::default();
            settings.unseen_phone_skip_days = self.0;
            Ok(settings)
        }
        async fn update(&self, _: &pond_core::user_data::domain::settings::Settings) -> Result<()> {
            Ok(())
        }
        async fn get_key(&self, _: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn set_key(&self, _: &str, _: String) -> Result<()> {
            Ok(())
        }
    }

    async fn seen(pool: &Pool<Sqlite>, device_id: &str, days_ago: i64) {
        sqlx::query("UPDATE session_tokens SET last_seen_at = ? WHERE device_id = ?")
            .bind((Utc::now() - Duration::days(days_ago)).to_rfc3339())
            .bind(device_id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn only_a_phone_whose_every_session_is_old_counts_as_idle() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let pool = db.system.clone();
        for phone in ["daily", "drawer", "never-stamped", "gone"] {
            insert_device(&pool, phone, PHONE_CLIENT_TYPE).await;
            insert_token(&pool, phone, phone == "gone").await;
        }
        seen(&pool, "daily", 1).await;
        seen(&pool, "drawer", 45).await;
        seen(&pool, "gone", 45).await;

        let idle = SqliteNotificationRecipients::new(pool)
            .unseen_since(Utc::now() - Duration::days(30))
            .await
            .unwrap();
        assert_eq!(idle.len(), 1, "{idle:?}");
        assert_eq!(idle[0].device_id, "drawer");
        assert!((Utc::now() - idle[0].last_seen).num_days() >= 44);
    }

    #[tokio::test]
    async fn a_test_phone_and_one_long_unseen_are_left_out() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let pool = db.system.clone();
        for phone in ["daily", "drawer", "harness", "never-stamped"] {
            insert_device(&pool, phone, PHONE_CLIENT_TYPE).await;
            insert_token(&pool, phone, false).await;
        }
        seen(&pool, "daily", 1).await;
        seen(&pool, "drawer", 20).await;
        seen(&pool, "harness", 0).await;
        sqlx::query("UPDATE devices SET is_test = 1 WHERE id = 'harness'")
            .execute(&pool)
            .await
            .unwrap();

        let limited = SqliteNotificationRecipients::new(pool.clone())
            .with_idle_limit(Arc::new(SkipAfter(14)));
        assert_eq!(
            limited.paired_phones().await.unwrap(),
            ["daily", "never-stamped"],
            "the phone in a drawer waits until it connects again"
        );
        let unlimited =
            SqliteNotificationRecipients::new(pool).with_idle_limit(Arc::new(SkipAfter(0)));
        assert_eq!(
            unlimited.paired_phones().await.unwrap(),
            ["daily", "drawer", "never-stamped"],
            "0 never stops, and a test phone is left out either way"
        );
    }
}
