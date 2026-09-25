//! SQLite-backed queue holding targeted notifications until the device's stream delivers them.

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::ports::notification::Notification;
use pond_core::mcp::ports::notification_queue::NotificationQueueRepository;
use sqlx::{Pool, Row, Sqlite};

pub struct SqliteNotificationQueue {
    pool: Pool<Sqlite>,
}

impl SqliteNotificationQueue {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

fn row_to_notification(row: &sqlx::sqlite::SqliteRow) -> Notification {
    let data: Option<String> = row.get("data");
    Notification {
        id: row.get("id"),
        target: row.get("device_id"),
        category: row.get("category"),
        title: row.get("title"),
        body: row.get("body"),
        timestamp: row.get("timestamp"),
        data: data.and_then(|s| serde_json::from_str(&s).ok()),
    }
}

#[async_trait]
impl NotificationQueueRepository for SqliteNotificationQueue {
    async fn enqueue(&self, notification: Notification) -> Result<()> {
        let data = notification.data.as_ref().map(|v| v.to_string());
        sqlx::query(
            "INSERT OR REPLACE INTO notifications \
             (id, device_id, category, title, body, timestamp, data) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(notification.id)
        .bind(notification.target)
        .bind(notification.category)
        .bind(notification.title)
        .bind(notification.body)
        .bind(notification.timestamp)
        .bind(data)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_undelivered(&self, device_id: &str) -> Result<Vec<Notification>> {
        let rows = sqlx::query(
            "SELECT id, device_id, category, title, body, timestamp, data \
             FROM notifications \
             WHERE device_id = ? AND delivered_at IS NULL \
             ORDER BY created_at ASC, rowid ASC",
        )
        .bind(device_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_notification).collect())
    }

    async fn mark_delivered(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        // Build a parameterized `IN (?, ?, ...)` — never interpolate ids.
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "UPDATE notifications SET delivered_at = datetime('now') \
             WHERE id IN ({placeholders}) AND delivered_at IS NULL"
        );
        let mut q = sqlx::query(&sql);
        for id in ids {
            q = q.bind(id);
        }
        q.execute(&self.pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::tempdir;

    async fn fresh() -> SqliteNotificationQueue {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        sqlx::query("INSERT INTO devices (id, name) VALUES ('dev-1', 'Phone')")
            .execute(&db.system)
            .await
            .unwrap();
        let q = SqliteNotificationQueue::new(db.system.clone());
        std::mem::forget(tmp);
        q
    }

    fn notif(id: &str, target: &str) -> Notification {
        Notification {
            id: id.into(),
            target: target.into(),
            category: "info".into(),
            title: "Hi".into(),
            body: "body".into(),
            timestamp: "2026-06-29T00:00:00Z".into(),
            data: Some(serde_json::json!({ "k": "v" })),
        }
    }

    #[tokio::test]
    async fn enqueue_list_mark_delivered() {
        let q = fresh().await;
        assert!(q.list_undelivered("dev-1").await.unwrap().is_empty());

        q.enqueue(notif("n1", "dev-1")).await.unwrap();
        q.enqueue(notif("n2", "dev-1")).await.unwrap();

        let undelivered = q.list_undelivered("dev-1").await.unwrap();
        assert_eq!(undelivered.len(), 2);
        assert_eq!(undelivered[0].id, "n1"); // oldest first
        assert_eq!(undelivered[0].data, Some(serde_json::json!({ "k": "v" })));

        q.mark_delivered(&["n1".to_string()]).await.unwrap();
        let left = q.list_undelivered("dev-1").await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "n2");

        // Idempotent / empty input is a no-op.
        q.mark_delivered(&[]).await.unwrap();
    }

    /// `notifications.id` is the primary key and `enqueue` is `INSERT OR REPLACE`, so a shared
    /// id across devices would leave one row; only the real table (not the stub) shows that.
    #[tokio::test]
    async fn a_member_with_two_devices_gets_one_durable_row_each() {
        use crate::broadcast_notification_sender::BroadcastNotificationSender;
        use crate::sqlite_device_attribution::SqliteDeviceAttribution;
        use pond_core::user_data::ports::device_attribution::DeviceAttribution;
        use std::sync::Arc;

        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('liz', 'Liz')")
            .execute(&db.system)
            .await
            .unwrap();
        for (id, name) in [
            ("phone-liz", "Liz Phone"),
            ("watch-liz", "Liz Watch"),
            ("tablet", "Kitchen Tablet"),
        ] {
            sqlx::query("INSERT INTO devices (id, name) VALUES (?, ?)")
                .bind(id)
                .bind(name)
                .execute(&db.system)
                .await
                .unwrap();
        }
        let attribution = Arc::new(SqliteDeviceAttribution::new(db.system.clone()));
        for device in ["phone-liz", "watch-liz"] {
            attribution
                .set_device_profile(device, Some("liz"))
                .await
                .unwrap();
        }
        // The kitchen tablet stays unattributed, like a real shared screen.

        let queue = Arc::new(SqliteNotificationQueue::new(db.system.clone()));
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None)
            .with_device_attribution(attribution.clone());

        let report = sender
            .send_to_profile("liz", notif("prop-1", "unused"))
            .await;
        assert_eq!(report.queued, vec!["phone-liz", "watch-liz"]);

        // Both devices were offline, so both rows are still waiting.
        assert_eq!(
            queue.list_undelivered("phone-liz").await.unwrap().len(),
            1,
            "the first device's row must survive the second device's INSERT OR REPLACE"
        );
        assert_eq!(queue.list_undelivered("watch-liz").await.unwrap().len(), 1);

        // Also the vacuity control: an enqueue that wrote nothing would pass only this one.
        assert!(
            queue.list_undelivered("tablet").await.unwrap().is_empty(),
            "an unattributed device is nobody's, so a targeted proposal never lands on it"
        );

        // One device comes back online and drains; the other's row is untouched.
        let waiting = queue.list_undelivered("phone-liz").await.unwrap();
        queue
            .mark_delivered(&[waiting[0].id.clone()])
            .await
            .unwrap();
        assert!(queue
            .list_undelivered("phone-liz")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            queue.list_undelivered("watch-liz").await.unwrap().len(),
            1,
            "delivering to one device must not mark the other's copy delivered"
        );
    }
}
