//! SQLite-backed [`NotificationHistory`]: the household's record of durable notices
//! (`notification_history`), scoped per device by [`HistoryAudience`], with per-device read state.
//! The pruning pass applies the queue's retention to it (`crate::pruning`).

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::domain::notification_history::{HistoryAudience, HistoryEntry};
use pond_core::mcp::ports::notification::Notification;
use pond_core::mcp::ports::notification_history::NotificationHistory;
use sqlx::{Pool, Row, Sqlite};

/// The household's notification history in `pond_system.db` (migration `0062`).
pub struct SqliteNotificationHistory {
    pool: Pool<Sqlite>,
}

impl SqliteNotificationHistory {
    /// A history over the system database's pool.
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

/// [`HistoryAudience::admits`] as SQL over `notification_history h`; binds the device id twice.
const VISIBLE_TO_DEVICE: &str = "( \
        (h.audience = 'household' AND (h.except_device IS NULL OR h.except_device != ?)) \
     OR (h.audience = 'devices' AND EXISTS ( \
            SELECT 1 FROM notification_history_recipients x \
            WHERE x.notification_id = h.id AND x.device_id = ?)) \
    )";

#[async_trait]
impl NotificationHistory for SqliteNotificationHistory {
    async fn record(&self, notification: &Notification, audience: &HistoryAudience) -> Result<()> {
        let (kind, except_device) = match audience {
            HistoryAudience::Household { except_device } => ("household", except_device.clone()),
            HistoryAudience::Devices(_) => ("devices", None),
        };
        let data = notification.data.as_ref().map(|v| v.to_string());
        let mut tx = self.pool.begin().await?;
        // An upsert, not INSERT OR REPLACE: a replace deletes the row, and with it (by cascade)
        // every device's read state and the audience's recipients.
        sqlx::query(
            "INSERT INTO notification_history \
             (id, audience, except_device, category, title, body, timestamp, data) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
                category = excluded.category, title = excluded.title, body = excluded.body, \
                timestamp = excluded.timestamp, data = excluded.data",
        )
        .bind(&notification.id)
        .bind(kind)
        .bind(except_device)
        .bind(&notification.category)
        .bind(&notification.title)
        .bind(&notification.body)
        .bind(&notification.timestamp)
        .bind(data)
        .execute(&mut *tx)
        .await?;
        if let HistoryAudience::Devices(devices) = audience {
            for device_id in devices {
                // A device id with no `devices` row (removed meanwhile) is skipped: a foreign-key
                // failure would lose the whole notice for the member's other devices.
                sqlx::query(
                    "INSERT OR IGNORE INTO notification_history_recipients \
                     (notification_id, device_id) \
                     SELECT ?, id FROM devices WHERE id = ?",
                )
                .bind(&notification.id)
                .bind(device_id)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    async fn list_for_device(&self, device_id: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
        let sql = format!(
            "SELECT h.id, h.category, h.title, h.body, h.timestamp, h.data, \
                    EXISTS (SELECT 1 FROM notification_history_reads r \
                            WHERE r.notification_id = h.id AND r.device_id = ?) AS read \
             FROM notification_history h \
             WHERE {VISIBLE_TO_DEVICE} \
             ORDER BY h.created_at DESC, h.rowid DESC \
             LIMIT ?"
        );
        let rows = sqlx::query(&sql)
            .bind(device_id)
            .bind(device_id)
            .bind(device_id)
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|row| {
                let data: Option<String> = row.get("data");
                HistoryEntry {
                    id: row.get("id"),
                    category: row.get("category"),
                    title: row.get("title"),
                    body: row.get("body"),
                    timestamp: row.get("timestamp"),
                    data: data.and_then(|s| serde_json::from_str(&s).ok()),
                    read: row.get::<bool, _>("read"),
                }
            })
            .collect())
    }

    async fn mark_read(&self, device_id: &str, ids: &[String]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        // Parameterized `IN (?, ...)`, never interpolated ids; the visibility clause is the
        // ownership check.
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "INSERT OR IGNORE INTO notification_history_reads (notification_id, device_id) \
             SELECT h.id, ? FROM notification_history h \
             WHERE h.id IN ({placeholders}) AND {VISIBLE_TO_DEVICE}"
        );
        let mut query = sqlx::query(&sql).bind(device_id);
        for id in ids {
            query = query.bind(id);
        }
        let result = query
            .bind(device_id)
            .bind(device_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::{tempdir, TempDir};

    async fn fresh() -> (SqliteNotificationHistory, Pool<Sqlite>, TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        for id in ["dashboard", "phone-liz", "phone-jerry"] {
            sqlx::query("INSERT INTO devices (id, name) VALUES (?, ?)")
                .bind(id)
                .bind(id)
                .execute(&db.system)
                .await
                .unwrap();
        }
        (
            SqliteNotificationHistory::new(db.system.clone()),
            db.system,
            tmp,
        )
    }

    fn notice(id: &str) -> Notification {
        Notification {
            id: id.into(),
            target: "broadcast".into(),
            category: "alert".into(),
            title: format!("title {id}"),
            body: "body".into(),
            timestamp: "2026-10-28T07:00:00Z".into(),
            data: Some(serde_json::json!({"open": "models", "kind": "disk_low"})),
        }
    }

    fn household() -> HistoryAudience {
        HistoryAudience::Household {
            except_device: None,
        }
    }

    async fn ids(history: &SqliteNotificationHistory, device: &str) -> Vec<String> {
        history
            .list_for_device(device, 50)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect()
    }

    #[tokio::test]
    async fn a_household_notice_is_listed_newest_first_with_its_payload() {
        let (history, _pool, _tmp) = fresh().await;
        history.record(&notice("n1"), &household()).await.unwrap();
        history.record(&notice("n2"), &household()).await.unwrap();

        let listed = history.list_for_device("dashboard", 50).await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "n2", "newest first");
        assert_eq!(listed[1].category, "alert");
        assert_eq!(listed[1].title, "title n1");
        assert_eq!(
            listed[1].data,
            Some(serde_json::json!({"open": "models", "kind": "disk_low"}))
        );
        assert!(!listed[1].read);

        assert_eq!(
            history.list_for_device("dashboard", 1).await.unwrap().len(),
            1,
            "the limit is applied"
        );
    }

    #[tokio::test]
    async fn a_member_notice_is_never_listed_to_another_members_device() {
        let (history, _pool, _tmp) = fresh().await;
        history
            .record(
                &notice("proposal-1"),
                &HistoryAudience::Devices(vec!["phone-liz".into()]),
            )
            .await
            .unwrap();

        assert_eq!(ids(&history, "phone-liz").await, vec!["proposal-1"]);
        assert!(ids(&history, "phone-jerry").await.is_empty());
        assert!(
            ids(&history, "dashboard").await.is_empty(),
            "a shared screen is nobody's, so it never lists a member's notice"
        );
        assert_eq!(
            history
                .mark_read("phone-jerry", &["proposal-1".into()])
                .await
                .unwrap(),
            0,
            "a device cannot mark a notice it may not read"
        );
    }

    #[tokio::test]
    async fn the_device_that_caused_a_notice_does_not_list_it() {
        let (history, _pool, _tmp) = fresh().await;
        history
            .record(
                &notice("removed"),
                &HistoryAudience::Household {
                    except_device: Some("phone-liz".into()),
                },
            )
            .await
            .unwrap();
        assert!(ids(&history, "phone-liz").await.is_empty());
        assert_eq!(ids(&history, "phone-jerry").await, vec!["removed"]);
        assert_eq!(ids(&history, "dashboard").await, vec!["removed"]);
    }

    #[tokio::test]
    async fn read_state_is_per_device_and_survives_a_re_record() {
        let (history, _pool, _tmp) = fresh().await;
        history.record(&notice("n1"), &household()).await.unwrap();

        assert_eq!(
            history
                .mark_read("dashboard", &["n1".into(), "missing".into()])
                .await
                .unwrap(),
            1,
            "an unknown id is skipped, not an error"
        );
        assert_eq!(
            history
                .mark_read("dashboard", &["n1".into()])
                .await
                .unwrap(),
            0,
            "marking twice marks nothing new"
        );
        assert!(history.list_for_device("dashboard", 50).await.unwrap()[0].read);
        assert!(
            !history.list_for_device("phone-liz", 50).await.unwrap()[0].read,
            "another device's read state is its own"
        );

        let mut updated = notice("n1");
        updated.body = "updated".into();
        history.record(&updated, &household()).await.unwrap();
        let listed = history.list_for_device("dashboard", 50).await.unwrap();
        assert_eq!(listed.len(), 1, "one notice, one row");
        assert_eq!(listed[0].body, "updated");
        assert!(listed[0].read, "recording again keeps the read state");
    }

    #[tokio::test]
    async fn a_recipient_with_no_device_row_is_skipped_not_fatal() {
        let (history, _pool, _tmp) = fresh().await;
        history
            .record(
                &notice("proposal-2"),
                &HistoryAudience::Devices(vec!["gone".into(), "phone-liz".into()]),
            )
            .await
            .unwrap();
        assert_eq!(ids(&history, "phone-liz").await, vec!["proposal-2"]);
        assert!(ids(&history, "gone").await.is_empty());
    }

    #[tokio::test]
    async fn removing_a_device_removes_its_addressed_rows() {
        let (history, pool, _tmp) = fresh().await;
        history
            .record(
                &notice("proposal-3"),
                &HistoryAudience::Devices(vec!["phone-liz".into()]),
            )
            .await
            .unwrap();
        sqlx::query("DELETE FROM devices WHERE id = 'phone-liz'")
            .execute(&pool)
            .await
            .unwrap();
        let recipients: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM notification_history_recipients")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(recipients, 0);
    }

    /// The live test restarts on a populated directory too; this pins the same thing in-process:
    /// a pond that already has devices and queued notices takes the migration and works.
    #[tokio::test]
    async fn the_migration_applies_to_a_database_that_already_has_rows() {
        let tmp = tempdir().unwrap();
        let pool = Database::connect(&tmp.path().join("pond_system.db"))
            .await
            .unwrap();
        let mut before = sqlx::migrate!("migrations/system");
        before.migrations = before
            .migrations
            .iter()
            .filter(|m| m.version < 62)
            .cloned()
            .collect::<Vec<_>>()
            .into();
        before.run(&pool).await.unwrap();
        sqlx::query("INSERT INTO devices (id, name) VALUES ('phone-liz', 'Liz Phone')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO notifications (id, device_id, category, title, body, timestamp) \
             VALUES ('n0:phone-liz', 'phone-liz', 'info', 't', 'b', '')",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::migrate!("migrations/system")
            .run(&pool)
            .await
            .unwrap();

        let history = SqliteNotificationHistory::new(pool.clone());
        history
            .record(
                &notice("proposal-1"),
                &HistoryAudience::Devices(vec!["phone-liz".into()]),
            )
            .await
            .unwrap();
        assert_eq!(ids(&history, "phone-liz").await, vec!["proposal-1"]);
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(queued, 1, "the queue's rows are untouched");
    }
}
