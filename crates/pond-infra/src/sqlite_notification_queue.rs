//! SQLite-backed queue holding targeted notifications until the device's stream delivers them.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pond_core::mcp::domain::notice_delivery::{DeliveryTotals, UnfetchedNotice};
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

    /// Stamps `column` once on a row, but only when the row is `device_id`'s.
    async fn stamp_own(&self, column: &'static str, id: &str, device_id: &str) -> Result<bool> {
        let sql = format!(
            "UPDATE notifications SET {column} = COALESCE({column}, datetime('now')) \
             WHERE id = ? AND device_id = ?"
        );
        let result = sqlx::query(&sql)
            .bind(id)
            .bind(device_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

/// SQLite's `datetime('now')` form, which the stamps in this table are written in.
const SQLITE_STAMP: &str = "%Y-%m-%d %H:%M:%S";

fn parse_stamp(stored: Option<String>) -> Option<DateTime<Utc>> {
    chrono::NaiveDateTime::parse_from_str(&stored?, SQLITE_STAMP)
        .ok()
        .map(|naive| naive.and_utc())
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

    async fn enqueue_held(&self, notification: Notification) -> Result<()> {
        let id = notification.id.clone();
        self.enqueue(notification).await?;
        sqlx::query("UPDATE notifications SET held = 1 WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn take_held(&self) -> Result<Vec<Notification>> {
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT id, device_id, category, title, body, timestamp, data \
             FROM notifications WHERE held = 1 ORDER BY created_at ASC, rowid ASC",
        )
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM notifications WHERE held = 1")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(rows.iter().map(row_to_notification).collect())
    }

    async fn list_undelivered(&self, device_id: &str) -> Result<Vec<Notification>> {
        let rows = sqlx::query(
            "SELECT id, device_id, category, title, body, timestamp, data \
             FROM notifications \
             WHERE device_id = ? AND delivered_at IS NULL AND held = 0 \
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
            "UPDATE notifications SET delivered_at = datetime('now'), \
                    fetched_at = COALESCE(fetched_at, datetime('now')) \
             WHERE id IN ({placeholders}) AND delivered_at IS NULL"
        );
        let mut q = sqlx::query(&sql);
        for id in ids {
            q = q.bind(id);
        }
        q.execute(&self.pool).await?;
        Ok(())
    }

    async fn is_queued_for(&self, id: &str, device_id: &str) -> Result<bool> {
        let found: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM notifications WHERE id = ? AND device_id = ?")
                .bind(id)
                .bind(device_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(found.is_some())
    }

    async fn acknowledge(&self, id: &str, device_id: &str) -> Result<bool> {
        // `device_id` in the WHERE is the ownership check; COALESCE keeps the first stamp.
        let result = sqlx::query(
            "UPDATE notifications SET delivered_at = COALESCE(delivered_at, datetime('now')), \
                    fetched_at = COALESCE(fetched_at, datetime('now')) \
             WHERE id = ? AND device_id = ?",
        )
        .bind(id)
        .bind(device_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn note_wake(&self, id: &str) -> Result<()> {
        sqlx::query(
            "UPDATE notifications SET wakes = wakes + 1, woken_at = datetime('now') WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn note_fetched(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "UPDATE notifications SET fetched_at = datetime('now') \
             WHERE id IN ({placeholders}) AND fetched_at IS NULL"
        );
        let mut q = sqlx::query(&sql);
        for id in ids {
            q = q.bind(id);
        }
        q.execute(&self.pool).await?;
        Ok(())
    }

    async fn note_shown(&self, id: &str, device_id: &str) -> Result<bool> {
        self.stamp_own("shown_at", id, device_id).await
    }

    async fn note_opened(&self, id: &str, device_id: &str) -> Result<bool> {
        self.stamp_own("opened_at", id, device_id).await
    }

    async fn note_spoken(&self, id: &str) -> Result<()> {
        sqlx::query(
            "UPDATE notifications SET spoken_at = COALESCE(spoken_at, datetime('now')) \
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_unfetched(&self, since: DateTime<Utc>) -> Result<Vec<UnfetchedNotice>> {
        let rows = sqlx::query(
            "SELECT id, device_id, category, title, body, timestamp, data, \
                    created_at, wakes, woken_at, spoken_at \
             FROM notifications \
             WHERE fetched_at IS NULL AND delivered_at IS NULL AND held = 0 \
                   AND created_at >= ? \
             ORDER BY created_at ASC, rowid ASC",
        )
        .bind(since.format(SQLITE_STAMP).to_string())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let wakes: i64 = row.get("wakes");
                let spoken_at: Option<String> = row.get("spoken_at");
                Some(UnfetchedNotice {
                    notification: row_to_notification(row),
                    queued_at: parse_stamp(row.get::<Option<String>, _>("created_at"))?,
                    wakes: wakes.max(0) as u32,
                    last_wake_at: parse_stamp(row.get("woken_at")),
                    spoken: spoken_at.is_some(),
                })
            })
            .collect())
    }

    async fn delivery_totals(&self, since: DateTime<Utc>) -> Result<DeliveryTotals> {
        // Seconds from queued to fetched; julianday keeps the one-second stamps comparable.
        const LAG: &str = "(julianday(n.fetched_at) - julianday(n.created_at)) * 86400.0";
        let sql = format!(
            "SELECT COUNT(*) AS queued, \
                    COALESCE(SUM(n.wakes > 0), 0) AS woken, \
                    COALESCE(SUM(n.fetched_at IS NOT NULL), 0) AS fetched, \
                    COALESCE(SUM(n.fetched_at IS NOT NULL AND {LAG} <= 60.0), 0) AS in_a_minute, \
                    COALESCE(SUM(n.fetched_at IS NOT NULL AND {LAG} > 3600.0), 0) AS after_an_hour, \
                    COALESCE(SUM(n.shown_at IS NOT NULL), 0) AS shown, \
                    COALESCE(SUM(n.opened_at IS NOT NULL), 0) AS opened, \
                    COALESCE(SUM(n.spoken_at IS NOT NULL), 0) AS spoken \
             FROM notifications n JOIN devices d ON d.id = n.device_id \
             WHERE n.created_at >= ? AND d.is_test = 0 AND n.held = 0"
        );
        let row = sqlx::query(&sql)
            .bind(since.format(SQLITE_STAMP).to_string())
            .fetch_one(&self.pool)
            .await?;
        let count = |name: &str| row.get::<i64, _>(name).max(0) as u64;
        Ok(DeliveryTotals {
            queued: count("queued"),
            woken: count("woken"),
            fetched: count("fetched"),
            fetched_within_a_minute: count("in_a_minute"),
            fetched_after_an_hour: count("after_an_hour"),
            shown: count("shown"),
            opened: count("opened"),
            spoken: count("spoken"),
        })
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

    #[tokio::test]
    async fn a_held_notice_is_unseen_until_it_is_taken() {
        let q = fresh().await;
        let long_ago = Utc::now() - chrono::Duration::days(1);
        q.enqueue_held(notif("h1", "dev-1")).await.unwrap();
        q.enqueue(notif("n1", "dev-1")).await.unwrap();

        let seen = q.list_undelivered("dev-1").await.unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].id, "n1");
        assert_eq!(
            q.list_unfetched(long_ago).await.unwrap().len(),
            1,
            "no wake for a held one"
        );
        assert_eq!(q.delivery_totals(long_ago).await.unwrap().queued, 1);
        assert!(q.is_queued_for("h1", "dev-1").await.unwrap());

        let taken = q.take_held().await.unwrap();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].id, "h1");
        assert!(q.take_held().await.unwrap().is_empty(), "taken once");
        assert!(!q.is_queued_for("h1", "dev-1").await.unwrap());
    }

    #[tokio::test]
    async fn the_record_says_what_became_of_each_copy() {
        let q = fresh().await;
        let long_ago = Utc::now() - chrono::Duration::days(1);
        q.enqueue(notif("n1", "dev-1")).await.unwrap();
        q.enqueue(notif("n2", "dev-1")).await.unwrap();
        q.enqueue(notif("n3", "dev-1")).await.unwrap();

        q.note_wake("n1").await.unwrap();
        q.note_wake("n1").await.unwrap();
        q.note_fetched(&["n2".to_string()]).await.unwrap();
        assert!(q.note_shown("n2", "dev-1").await.unwrap());
        assert!(q.note_opened("n2", "dev-1").await.unwrap());
        assert!(
            !q.note_opened("n2", "dev-2").await.unwrap(),
            "another device cannot mark this one's copy opened"
        );
        q.note_spoken("n3").await.unwrap();

        let unfetched = q.list_unfetched(long_ago).await.unwrap();
        let ids: Vec<&str> = unfetched
            .iter()
            .map(|u| u.notification.id.as_str())
            .collect();
        assert_eq!(ids, ["n1", "n3"], "a fetched copy is no longer waiting");
        assert_eq!(unfetched[0].wakes, 2);
        assert!(unfetched[0].last_wake_at.is_some());
        assert!(unfetched[1].spoken);

        let totals = q.delivery_totals(long_ago).await.unwrap();
        assert_eq!(
            (
                totals.queued,
                totals.woken,
                totals.fetched,
                totals.shown,
                totals.opened,
                totals.spoken
            ),
            (3, 1, 1, 1, 1, 1)
        );
        assert_eq!(totals.fetched_within_a_minute, 1);
        assert_eq!(totals.never_fetched(), 2);
    }

    #[tokio::test]
    async fn an_acknowledged_copy_counts_as_fetched_and_a_test_device_is_left_out() {
        let q = fresh().await;
        let long_ago = Utc::now() - chrono::Duration::days(1);
        sqlx::query("INSERT INTO devices (id, name, is_test) VALUES ('harness', 'Harness', 1)")
            .execute(&q.pool)
            .await
            .unwrap();
        q.enqueue(notif("n1", "dev-1")).await.unwrap();
        q.enqueue(notif("t1", "harness")).await.unwrap();
        assert!(q.acknowledge("n1", "dev-1").await.unwrap());

        let totals = q.delivery_totals(long_ago).await.unwrap();
        assert_eq!((totals.queued, totals.fetched), (1, 1));
    }

    #[tokio::test]
    async fn only_the_owning_device_can_acknowledge_a_row() {
        let q = fresh().await;
        q.enqueue(notif("n1", "dev-1")).await.unwrap();

        assert!(q.is_queued_for("n1", "dev-1").await.unwrap());
        assert!(
            !q.is_queued_for("n1", "dev-2").await.unwrap(),
            "another device's row is not this device's copy"
        );

        assert!(
            !q.acknowledge("n1", "dev-2").await.unwrap(),
            "a device that does not own the row is refused"
        );
        assert_eq!(
            q.list_undelivered("dev-1").await.unwrap().len(),
            1,
            "and the refusal leaves the owner's row waiting"
        );

        assert!(q.acknowledge("n1", "dev-1").await.unwrap());
        assert!(q.list_undelivered("dev-1").await.unwrap().is_empty());
        assert!(
            q.acknowledge("n1", "dev-1").await.unwrap(),
            "acknowledging twice is still the owner's row"
        );
        assert!(
            q.is_queued_for("n1", "dev-1").await.unwrap(),
            "a delivered row still counts as queued for the device"
        );
        assert!(!q.acknowledge("missing", "dev-1").await.unwrap());
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
        // Newest device first, and the two may be registered in the same second.
        let mut queued = report.queued.clone();
        queued.sort();
        assert_eq!(queued, vec!["phone-liz", "watch-liz"]);

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
