//! Reads the reliability report out of the pond's two databases. Only reads: it can run beside
//! a serving pond, over connections opened read-only.

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use pond_core::mcp::ports::notification_queue::NotificationQueueRepository;
use pond_core::mcp::ports::notification_recipients::PHONE_CLIENT_TYPE;
use pond_core::shared::domain::reliability::{PhoneTotals, ReliabilityReport, TurnTotals};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Pool, Row, Sqlite};

use crate::sqlite_notification_queue::SqliteNotificationQueue;

/// Where the pond looks for its push keys, under the data directory.
pub const ANDROID_PUSH_KEY: &str = "secrets/fcm-service-account.json";
pub const APPLE_PUSH_KEY: &str = "secrets/apns-key.json";

const SQLITE_STAMP: &str = "%Y-%m-%d %H:%M:%S";

/// The report over the last `days` days, with phones counted as unseen past `unseen_days`.
pub async fn report(
    system: &Pool<Sqlite>,
    logs: &Pool<Sqlite>,
    data_dir: &Path,
    days: u32,
    unseen_days: u32,
    now: DateTime<Utc>,
) -> Result<ReliabilityReport> {
    let since = now - Duration::days(i64::from(days));
    let notices = Arc::new(SqliteNotificationQueue::new(system.clone()))
        .delivery_totals(since)
        .await
        .context("reading what became of notices")?;

    // A reply is any assistant message stored after the person's, in the same conversation.
    let turns = sqlx::query(
        "SELECT COUNT(*) AS asked, \
                COALESCE(SUM(EXISTS (SELECT 1 FROM session_messages a \
                                     WHERE a.session_id = m.session_id AND a.role = 'assistant' \
                                       AND a.rowid > m.rowid)), 0) AS answered \
         FROM session_messages m JOIN sessions s ON s.id = m.session_id \
         WHERE m.role = 'user' AND m.created_at >= ? AND s.id NOT LIKE 'sched-%' \
           AND (s.surface_device_id IS NULL \
                OR s.surface_device_id NOT IN (SELECT id FROM devices WHERE is_test = 1))",
    )
    .bind(since.format(SQLITE_STAMP).to_string())
    .fetch_one(system)
    .await
    .context("counting messages and replies")?;

    let unseen_before = (now - Duration::days(i64::from(unseen_days))).to_rfc3339();
    let phones = sqlx::query(
        "SELECT COUNT(*) AS paired, \
                COALESCE(SUM(d.is_test), 0) AS test, \
                COALESCE(SUM(? > 0 AND COALESCE((SELECT MAX(t.last_seen_at) FROM session_tokens t \
                     WHERE t.device_id = d.id AND t.revoked_at IS NULL), '') NOT IN ('') \
                     AND (SELECT MAX(t.last_seen_at) FROM session_tokens t \
                          WHERE t.device_id = d.id AND t.revoked_at IS NULL) < ?), 0) AS unseen, \
                COALESCE(SUM(EXISTS (SELECT 1 FROM push_tokens p WHERE p.device_id = d.id)), 0) \
                    AS with_push_token \
         FROM devices d \
         WHERE d.device_type = ? \
           AND EXISTS (SELECT 1 FROM session_tokens t \
                       WHERE t.device_id = d.id AND t.revoked_at IS NULL)",
    )
    .bind(i64::from(unseen_days))
    .bind(unseen_before)
    .bind(PHONE_CLIENT_TYPE)
    .fetch_one(system)
    .await
    .context("counting phones")?;

    let stops: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM event_log \
         WHERE timestamp >= ? AND json_extract(metadata, '$.kind') = 'shutdown'",
    )
    .bind(since.format(SQLITE_STAMP).to_string())
    .fetch_one(logs)
    .await
    .context("counting stops")?;

    let count = |row: &sqlx::sqlite::SqliteRow, name: &str| row.get::<i64, _>(name).max(0) as u64;
    Ok(ReliabilityReport {
        days,
        notices,
        turns: TurnTotals {
            asked: count(&turns, "asked"),
            answered: count(&turns, "answered"),
        },
        phones: PhoneTotals {
            paired: count(&phones, "paired"),
            test: count(&phones, "test"),
            unseen: count(&phones, "unseen"),
            with_push_token: count(&phones, "with_push_token"),
        },
        stops: stops.max(0) as u64,
        android_push_key: data_dir.join(ANDROID_PUSH_KEY).exists(),
        apple_push_key: data_dir.join(APPLE_PUSH_KEY).exists(),
    })
}

/// One database of a pond that may be serving, opened so nothing can be written to it.
pub async fn open_read_only(path: &Path) -> Result<Pool<Sqlite>> {
    let options =
        SqliteConnectOptions::from_str(&format!("sqlite:{}", path.display()))?.read_only(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .with_context(|| format!("opening {} read-only", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[tokio::test]
    async fn the_report_counts_what_arrived_what_was_answered_and_who_is_around() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let pool = db.system.clone();
        let now = Utc::now();
        for (id, is_test, seen_days_ago) in [("daily", 0, 1), ("drawer", 0, 20), ("harness", 1, 0)]
        {
            sqlx::query(
                "INSERT INTO devices (id, name, device_type, is_test) VALUES (?, ?, 'gotg', ?)",
            )
            .bind(id)
            .bind(id)
            .bind(is_test)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO session_tokens (token_hash, refresh_hash, device_id, client_id, \
                 client_type, created_at, expires_at, refresh_expires_at, last_seen_at) \
                 VALUES (?, ?, ?, ?, 'gotg', '', '', '', ?)",
            )
            .bind(format!("h-{id}"))
            .bind(format!("r-{id}"))
            .bind(id)
            .bind(id)
            .bind((now - Duration::days(seen_days_ago)).to_rfc3339())
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO push_tokens (device_id, token, platform, updated_at) VALUES ('daily', 't', 'fcm', '')")
            .execute(&pool)
            .await
            .unwrap();
        for (id, device) in [("n1", "daily"), ("n2", "daily"), ("n3", "harness")] {
            sqlx::query(
                "INSERT INTO notifications (id, device_id, category, title, body, timestamp) \
                 VALUES (?, ?, 'info', 't', 'b', '')",
            )
            .bind(id)
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "UPDATE notifications SET fetched_at = datetime('now'), wakes = 1 WHERE id = 'n1'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO sessions (id, created_at, updated_at) VALUES ('s1', datetime('now'), datetime('now'))")
            .execute(&pool)
            .await
            .unwrap();
        for (id, role) in [("m1", "user"), ("m2", "assistant"), ("m3", "user")] {
            sqlx::query(
                "INSERT INTO session_messages (id, session_id, role, content, created_at) \
                 VALUES (?, 's1', ?, 'x', datetime('now'))",
            )
            .bind(id)
            .bind(role)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO event_log (source, message, metadata) VALUES ('giap::trace', 'stop', '{\"kind\":\"shutdown\"}')")
            .execute(&db.logs)
            .await
            .unwrap();
        std::fs::create_dir_all(tmp.path().join("secrets")).unwrap();
        std::fs::write(tmp.path().join(ANDROID_PUSH_KEY), "{}").unwrap();

        let report = report(&pool, &db.logs, tmp.path(), 7, 14, now)
            .await
            .unwrap();
        assert_eq!(
            (
                report.notices.queued,
                report.notices.woken,
                report.notices.fetched
            ),
            (2, 1, 1),
            "the harness's notice is left out"
        );
        assert_eq!((report.turns.asked, report.turns.answered), (2, 1));
        assert_eq!(
            (
                report.phones.paired,
                report.phones.test,
                report.phones.unseen,
                report.phones.with_push_token
            ),
            (3, 1, 1, 1)
        );
        assert_eq!(report.stops, 1);
        assert!(report.android_push_key && !report.apple_push_key);
    }
}
