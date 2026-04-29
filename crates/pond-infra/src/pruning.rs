//! TTL-based data pruning for high-frequency tables.
//!
//! Runs as a background `tokio::spawn` task inside `pond-server`. Fires every
//! 6 hours and deletes rows that have exceeded their retention window.
//!
//! # Retention defaults
//!
//! | Table              | Rule                                       |
//! |--------------------|--------------------------------------------|
//! | `event_log`        | Delete rows older than 30 days             |
//! | `sensor_readings`  | Delete rows older than 7 days              |
//! | `camera_events`    | Delete *acknowledged* rows older than 14 d |
//! | `session_messages` | Keep the 500 most recent per session       |
//! | `face_embeddings`  | Delete orphaned rows (no matching profile) |
//!
//! Face embeddings themselves are never auto-expired — they are explicit
//! biometric data managed by the user.  The orphan sweep defends against
//! cases where a profile row is deleted without FK cascade (e.g. older
//! SQLite connections that did not enable `PRAGMA foreign_keys`).
//!
//! All constants are configurable via [`PruningConfig`].

use sqlx::{Pool, Sqlite};
use std::time::Duration;
use tracing::{info, warn};

/// Retention configuration — all fields have sane defaults via [`Default`].
pub struct PruningConfig {
    /// Interval between pruning runs (default: 6 hours).
    pub interval: Duration,
    /// Retain `event_log` rows for this many days (default: 30).
    pub event_log_days: u32,
    /// Retain `sensor_readings` rows for this many days (default: 7).
    pub sensor_readings_days: u32,
    /// Retain *acknowledged* `camera_events` rows for this many days (default: 14).
    pub camera_events_days: u32,
    /// Maximum messages to keep per session in `session_messages` (default: 500).
    pub session_messages_keep: u32,
}

impl Default for PruningConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(6 * 60 * 60),
            event_log_days: 30,
            sensor_readings_days: 7,
            camera_events_days: 14,
            session_messages_keep: 500,
        }
    }
}

/// Spawn the pruning loop.  Call once from `pond-server` main:
///
/// ```rust,ignore
/// tokio::spawn(pond_infra::pruning::run_pruning(
///     db.logs.clone(), db.system.clone(), Default::default(),
/// ));
/// ```
pub async fn run_pruning(logs: Pool<Sqlite>, system: Pool<Sqlite>, config: PruningConfig) {
    let mut interval = tokio::time::interval(config.interval);
    // Skip the first tick (fires immediately at t=0) so we don't prune on startup.
    interval.tick().await;
    loop {
        interval.tick().await;
        prune_once(&logs, &system, &config).await;
    }
}

/// Execute one pruning pass across all tables.
pub async fn prune_once(logs: &Pool<Sqlite>, system: &Pool<Sqlite>, config: &PruningConfig) {
    prune_event_log(logs, config.event_log_days).await;
    prune_sensor_readings(logs, config.sensor_readings_days).await;
    prune_camera_events(logs, config.camera_events_days).await;
    prune_session_messages(system, config.session_messages_keep).await;
    prune_orphan_face_embeddings(system).await;
}

async fn prune_event_log(pool: &Pool<Sqlite>, days: u32) {
    let cutoff = format!("-{} days", days);
    match sqlx::query(
        "DELETE FROM event_log WHERE timestamp < datetime('now', ?)",
    )
    .bind(&cutoff)
    .execute(pool)
    .await
    {
        Ok(r) => info!("pruning: deleted {} event_log rows older than {} days", r.rows_affected(), days),
        Err(e) => warn!("pruning: event_log failed: {}", e),
    }
}

async fn prune_sensor_readings(pool: &Pool<Sqlite>, days: u32) {
    let cutoff = format!("-{} days", days);
    match sqlx::query(
        "DELETE FROM sensor_readings WHERE created_at < datetime('now', ?)",
    )
    .bind(&cutoff)
    .execute(pool)
    .await
    {
        Ok(r) => info!("pruning: deleted {} sensor_readings rows older than {} days", r.rows_affected(), days),
        Err(e) => warn!("pruning: sensor_readings failed: {}", e),
    }
}

async fn prune_camera_events(pool: &Pool<Sqlite>, days: u32) {
    let cutoff = format!("-{} days", days);
    // Only prune acknowledged events — unacknowledged alerts are kept indefinitely.
    match sqlx::query(
        "DELETE FROM camera_events \
         WHERE created_at < datetime('now', ?) AND acknowledged = 1",
    )
    .bind(&cutoff)
    .execute(pool)
    .await
    {
        Ok(r) => info!("pruning: deleted {} camera_events rows older than {} days", r.rows_affected(), days),
        Err(e) => warn!("pruning: camera_events failed: {}", e),
    }
}

/// Remove face_embeddings rows whose `profile_id` no longer exists in
/// `profiles`.  Defensive cleanup — relied upon for biometric-data
/// hygiene if the DB connection ever runs without `PRAGMA foreign_keys=ON`.
async fn prune_orphan_face_embeddings(pool: &Pool<Sqlite>) {
    // Skip silently on schemas that don't yet have the face_embeddings table
    // (older DBs, tests that mount partial schemas).
    let exists: Option<(i64,)> = sqlx::query_as(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='face_embeddings'",
    )
    .fetch_optional(pool)
    .await
    .unwrap_or(None);
    if exists.is_none() {
        return;
    }

    match sqlx::query(
        "DELETE FROM face_embeddings \
         WHERE profile_id NOT IN (SELECT id FROM profiles)",
    )
    .execute(pool)
    .await
    {
        Ok(r) if r.rows_affected() > 0 => {
            info!("pruning: deleted {} orphan face_embeddings rows", r.rows_affected())
        }
        Ok(_) => {}
        Err(e) => warn!("pruning: face_embeddings failed: {}", e),
    }
}

async fn prune_session_messages(pool: &Pool<Sqlite>, keep: u32) {
    // Delete messages that are NOT in the most-recent `keep` rows for each session.
    match sqlx::query(
        "DELETE FROM session_messages \
         WHERE rowid NOT IN ( \
             SELECT rowid FROM session_messages sm2 \
             WHERE sm2.session_id = session_messages.session_id \
             ORDER BY created_at DESC, rowid DESC \
             LIMIT ? \
         )",
    )
    .bind(keep)
    .execute(pool)
    .await
    {
        Ok(r) => info!("pruning: deleted {} session_messages beyond per-session cap of {}", r.rows_affected(), keep),
        Err(e) => warn!("pruning: session_messages failed: {}", e),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::tempdir;

    async fn make_pools() -> (Pool<Sqlite>, Pool<Sqlite>, tempfile::TempDir) {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        (db.logs, db.system, tmp)
    }

    #[tokio::test]
    async fn prune_event_log_removes_old_rows() {
        let (logs, system, _tmp) = make_pools().await;

        // Insert 3 old rows and 2 fresh rows
        for _ in 0..3 {
            sqlx::query(
                "INSERT INTO event_log (timestamp, level, source, message) \
                 VALUES (datetime('now', '-31 days'), 'INFO', 'test', 'old')",
            )
            .execute(&logs)
            .await
            .unwrap();
        }
        for _ in 0..2 {
            sqlx::query(
                "INSERT INTO event_log (level, source, message) \
                 VALUES ('INFO', 'test', 'fresh')",
            )
            .execute(&logs)
            .await
            .unwrap();
        }

        let config = PruningConfig::default();
        prune_once(&logs, &system, &config).await;

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM event_log")
                .fetch_one(&logs)
                .await
                .unwrap();
        assert_eq!(count, 2, "only fresh rows should remain");
    }

    #[tokio::test]
    async fn prune_sensor_readings_removes_old_rows() {
        let (logs, system, _tmp) = make_pools().await;

        sqlx::query(
            "INSERT INTO sensor_readings (device_id, sensor_type, value, unit, created_at) \
             VALUES ('dev1', 'temperature', 22.5, '°C', datetime('now', '-8 days'))",
        )
        .execute(&logs)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sensor_readings (device_id, sensor_type, value, unit) \
             VALUES ('dev1', 'temperature', 23.0, '°C')",
        )
        .execute(&logs)
        .await
        .unwrap();

        prune_once(&logs, &system, &PruningConfig::default()).await;

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sensor_readings")
                .fetch_one(&logs)
                .await
                .unwrap();
        assert_eq!(count, 1, "old sensor reading should be pruned");
    }

    #[tokio::test]
    async fn prune_camera_events_only_removes_acknowledged_old_rows() {
        let (logs, system, _tmp) = make_pools().await;

        // Old + acknowledged → should be pruned
        sqlx::query(
            "INSERT INTO camera_events (camera_id, event_type, acknowledged, created_at) \
             VALUES ('cam1', 'motion', 1, datetime('now', '-15 days'))",
        )
        .execute(&logs)
        .await
        .unwrap();
        // Old + NOT acknowledged → must be kept
        sqlx::query(
            "INSERT INTO camera_events (camera_id, event_type, acknowledged, created_at) \
             VALUES ('cam1', 'motion', 0, datetime('now', '-15 days'))",
        )
        .execute(&logs)
        .await
        .unwrap();

        prune_once(&logs, &system, &PruningConfig::default()).await;

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM camera_events")
                .fetch_one(&logs)
                .await
                .unwrap();
        assert_eq!(count, 1, "only unacknowledged alert should remain");
    }

    #[tokio::test]
    async fn prune_session_messages_keeps_most_recent() {
        let (logs, system, _tmp) = make_pools().await;

        // Create a session and insert 10 messages
        sqlx::query("INSERT INTO sessions (id, created_at, updated_at) VALUES ('s1', datetime('now'), datetime('now'))")
            .execute(&system)
            .await
            .unwrap();
        for i in 0..10 {
            sqlx::query(
                "INSERT INTO session_messages (id, session_id, role, content, created_at) \
                 VALUES (?, 's1', 'user', ?, datetime('now'))",
            )
            .bind(format!("m{}", i))
            .bind(format!("msg {}", i))
            .execute(&system)
            .await
            .unwrap();
        }

        let config = PruningConfig { session_messages_keep: 3, ..Default::default() };
        prune_once(&logs, &system, &config).await;

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_messages WHERE session_id = 's1'")
                .fetch_one(&system)
                .await
                .unwrap();
        assert_eq!(count, 3, "only 3 most recent messages should remain");
    }

    #[tokio::test]
    async fn prune_is_idempotent() {
        let (logs, system, _tmp) = make_pools().await;
        let config = PruningConfig::default();
        // Running on empty tables should not error
        prune_once(&logs, &system, &config).await;
        prune_once(&logs, &system, &config).await;
    }
}
