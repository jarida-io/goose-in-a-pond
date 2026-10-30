//! TTL-based pruning of high-frequency tables, run as a background task in `pond-server`.
//!
//! Face embeddings are never auto-expired (user-managed biometrics); only orphans are swept.
//! The notification queue's caps are constants ([`NOTIFICATIONS_DELIVERED_DAYS`],
//! [`NOTIFICATIONS_MAX_AGE_DAYS`], [`NOTIFICATIONS_PER_DEVICE_KEEP`]), not settings, and the
//! household's notification history follows the same age and count caps.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sqlx::{Pool, Sqlite};
use tracing::{info, warn};

use pond_core::security::domain::event::{EventCategory, EventQuery, PrivacySensitivity};
use pond_core::security::ports::event_log::EventLog;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::settings::SettingsRepository;

use crate::sqlite_event_log::SqliteEventLog;

/// Retention ceiling (~100 years): more makes `Utc::now() - Duration` panic in chrono.
const MAX_RETENTION_DAYS: i64 = 36_500;

/// Days a delivered `notifications` row is kept (for audit) before it is deleted.
pub const NOTIFICATIONS_DELIVERED_DAYS: u32 = 7;
/// Days any `notifications` row is kept, delivered or not: a phone away this long gets a fresh
/// start rather than a month of stale reminders.
pub const NOTIFICATIONS_MAX_AGE_DAYS: u32 = 30;
/// Newest `notifications` rows kept per device, so a rule firing every minute for a phone that
/// never reconnects cannot grow the table without bound between passes.
pub const NOTIFICATIONS_PER_DEVICE_KEEP: u32 = 500;

fn clamp_retention_days(days: u32) -> i64 {
    (days as i64).min(MAX_RETENTION_DAYS)
}

/// Retention configuration, rebuilt each cycle from the user's [`Settings`].
pub struct PruningConfig {
    pub interval: Duration,
    /// Days to keep the legacy `event_log` table.
    pub event_log_days: u32,
    pub sensor_readings_days: u32,
    /// Applies to *acknowledged* `camera_events` only.
    pub camera_events_days: u32,
    /// Newest messages kept per session.
    pub session_messages_keep: u32,
    /// Baseline `events` retention in days; `0` = forever.
    pub events_days: u32,
    /// Per-category `events` retention override (snake_case category → days).
    pub events_by_category: HashMap<String, u32>,
    /// Cap in days for `Sensitive`/`Secret` `events`; `0` = no cap.
    pub events_sensitive_days: u32,
}

impl Default for PruningConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(6 * 60 * 60),
            event_log_days: 30,
            sensor_readings_days: 7,
            camera_events_days: 14,
            session_messages_keep: 500,
            events_days: 30,
            events_by_category: HashMap::new(),
            events_sensitive_days: 7,
        }
    }
}

impl PruningConfig {
    /// Per-run config from settings; camera retention and the interval keep their defaults.
    pub fn from_settings(s: &Settings) -> Self {
        let defaults = Self::default();
        Self {
            interval: defaults.interval,
            event_log_days: s.retention_event_log_days,
            sensor_readings_days: s.retention_sensor_days,
            camera_events_days: defaults.camera_events_days,
            session_messages_keep: s.retention_session_messages_keep,
            events_days: s.retention_events_days,
            events_by_category: s.retention_events_by_category.clone(),
            events_sensitive_days: s.retention_sensitive_days,
        }
    }
}

/// Spawn the pruning loop. Call once from `pond-server` main:
///
/// ```rust,ignore
/// tokio::spawn(pond_infra::pruning::run_pruning(
///     db.logs.clone(), db.system.clone(), settings_repo.clone(),
/// ));
/// ```
///
/// Each cycle re-reads the user's [`Settings`] so retention changes take effect
/// on the next pass without a restart.
pub async fn run_pruning(
    logs: Pool<Sqlite>,
    system: Pool<Sqlite>,
    settings_repo: Arc<dyn SettingsRepository>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(6 * 60 * 60));
    // Skip the first tick (fires immediately at t=0) so we don't prune on startup.
    interval.tick().await;
    loop {
        interval.tick().await;
        let config = match settings_repo.get().await {
            Ok(s) => PruningConfig::from_settings(&s),
            Err(e) => {
                warn!("pruning: failed to load settings ({e}); using defaults");
                PruningConfig::default()
            }
        };
        prune_once(&logs, &system, &config).await;
    }
}

pub async fn prune_once(logs: &Pool<Sqlite>, system: &Pool<Sqlite>, config: &PruningConfig) {
    prune_event_log(logs, config.event_log_days).await;
    prune_sensor_readings(logs, config.sensor_readings_days).await;
    prune_camera_events(logs, config.camera_events_days).await;
    prune_events(logs, config).await;
    prune_session_messages(system, config.session_messages_keep).await;
    prune_orphan_face_embeddings(system).await;
    prune_notifications(system).await;
    prune_notification_history(system).await;
}

/// Sweeps `>= Sensitive` past the sensitivity cap, then each category past its retention.
async fn prune_events(logs: &Pool<Sqlite>, config: &PruningConfig) {
    let log = SqliteEventLog::new(logs.clone());
    let now = Utc::now();

    if config.events_sensitive_days > 0 {
        let until =
            now - chrono::Duration::days(clamp_retention_days(config.events_sensitive_days));
        match log
            .purge(EventQuery {
                min_sensitivity: Some(PrivacySensitivity::Sensitive),
                until: Some(until),
                ..Default::default()
            })
            .await
        {
            Ok(n) if n > 0 => info!(
                "pruning: deleted {n} sensitive events older than {} days",
                config.events_sensitive_days
            ),
            Ok(_) => {}
            Err(e) => warn!("pruning: events sensitivity sweep failed: {e}"),
        }
    }

    for category in EventCategory::ALL {
        let key = category_key(category);
        let days = config
            .events_by_category
            .get(&key)
            .copied()
            .unwrap_or(config.events_days);
        if days == 0 {
            continue; // keep forever
        }
        let until = now - chrono::Duration::days(clamp_retention_days(days));
        match log
            .purge(EventQuery {
                category: Some(category),
                until: Some(until),
                ..Default::default()
            })
            .await
        {
            Ok(n) if n > 0 => {
                info!("pruning: deleted {n} '{key}' events older than {days} days")
            }
            Ok(_) => {}
            Err(e) => warn!("pruning: events sweep for '{key}' failed: {e}"),
        }
    }
}

/// Serde's snake_case name for a category, as used in `events_by_category`.
fn category_key(category: EventCategory) -> String {
    serde_json::to_value(category)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

async fn prune_event_log(pool: &Pool<Sqlite>, days: u32) {
    let cutoff = format!("-{} days", days);
    match sqlx::query("DELETE FROM event_log WHERE timestamp < datetime('now', ?)")
        .bind(&cutoff)
        .execute(pool)
        .await
    {
        Ok(r) => info!(
            "pruning: deleted {} event_log rows older than {} days",
            r.rows_affected(),
            days
        ),
        Err(e) => warn!("pruning: event_log failed: {}", e),
    }
}

async fn prune_sensor_readings(pool: &Pool<Sqlite>, days: u32) {
    let cutoff = format!("-{} days", days);
    match sqlx::query("DELETE FROM sensor_readings WHERE created_at < datetime('now', ?)")
        .bind(&cutoff)
        .execute(pool)
        .await
    {
        Ok(r) => info!(
            "pruning: deleted {} sensor_readings rows older than {} days",
            r.rows_affected(),
            days
        ),
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
        Ok(r) => info!(
            "pruning: deleted {} camera_events rows older than {} days",
            r.rows_affected(),
            days
        ),
        Err(e) => warn!("pruning: camera_events failed: {}", e),
    }
}

/// Biometric hygiene for connections that ran without `PRAGMA foreign_keys=ON`.
async fn prune_orphan_face_embeddings(pool: &Pool<Sqlite>) {
    // Older DBs and partial test schemas may lack the table.
    let exists: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM sqlite_master WHERE type='table' AND name='face_embeddings'")
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
            info!(
                "pruning: deleted {} orphan face_embeddings rows",
                r.rows_affected()
            )
        }
        Ok(_) => {}
        Err(e) => warn!("pruning: face_embeddings failed: {}", e),
    }
}

/// The durable notification queue: delivered rows after [`NOTIFICATIONS_DELIVERED_DAYS`], every
/// row after [`NOTIFICATIONS_MAX_AGE_DAYS`], and anything beyond a device's newest
/// [`NOTIFICATIONS_PER_DEVICE_KEEP`].
async fn prune_notifications(pool: &Pool<Sqlite>) {
    match sqlx::query(
        "DELETE FROM notifications \
         WHERE (delivered_at IS NOT NULL AND delivered_at < datetime('now', ?)) \
            OR created_at < datetime('now', ?) \
            OR rowid NOT IN ( \
                SELECT rowid FROM notifications n2 \
                WHERE n2.device_id = notifications.device_id \
                ORDER BY created_at DESC, rowid DESC \
                LIMIT ? \
            )",
    )
    .bind(format!("-{NOTIFICATIONS_DELIVERED_DAYS} days"))
    .bind(format!("-{NOTIFICATIONS_MAX_AGE_DAYS} days"))
    .bind(NOTIFICATIONS_PER_DEVICE_KEEP)
    .execute(pool)
    .await
    {
        Ok(r) => info!(
            kind = "notifications_pruned",
            deleted = r.rows_affected(),
            "pruning: notifications past retention"
        ),
        Err(e) => {
            warn!(kind = "notifications_prune_failed", error = %e, "pruning: notifications failed")
        }
    }
}

/// The household's notification history: every row after [`NOTIFICATIONS_MAX_AGE_DAYS`], and
/// anything beyond the newest [`NOTIFICATIONS_PER_DEVICE_KEEP`], so the dashboard never lists a
/// notice older than a phone could still be holding. Recipients and read marks go by cascade.
async fn prune_notification_history(pool: &Pool<Sqlite>) {
    match sqlx::query(
        "DELETE FROM notification_history \
         WHERE created_at < datetime('now', ?) \
            OR rowid NOT IN ( \
                SELECT rowid FROM notification_history \
                ORDER BY created_at DESC, rowid DESC \
                LIMIT ? \
            )",
    )
    .bind(format!("-{NOTIFICATIONS_MAX_AGE_DAYS} days"))
    .bind(NOTIFICATIONS_PER_DEVICE_KEEP)
    .execute(pool)
    .await
    {
        Ok(r) => info!(
            kind = "notification_history_pruned",
            deleted = r.rows_affected(),
            "pruning: notification history past retention"
        ),
        Err(e) => warn!(
            kind = "notification_history_prune_failed",
            error = %e,
            "pruning: notification history failed"
        ),
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
        Ok(r) => info!(
            "pruning: deleted {} session_messages beyond per-session cap of {}",
            r.rows_affected(),
            keep
        ),
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

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_log")
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

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sensor_readings")
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

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM camera_events")
            .fetch_one(&logs)
            .await
            .unwrap();
        assert_eq!(count, 1, "only unacknowledged alert should remain");
    }

    #[tokio::test]
    async fn prune_session_messages_keeps_most_recent() {
        let (logs, system, _tmp) = make_pools().await;

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

        let config = PruningConfig {
            session_messages_keep: 3,
            ..Default::default()
        };
        prune_once(&logs, &system, &config).await;

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_messages WHERE session_id = 's1'")
                .fetch_one(&system)
                .await
                .unwrap();
        assert_eq!(count, 3, "only 3 most recent messages should remain");
    }

    #[tokio::test]
    async fn prune_notifications_keeps_what_a_phone_may_still_need() {
        let (logs, system, _tmp) = make_pools().await;
        sqlx::query("INSERT INTO devices (id, name) VALUES ('phone', 'Phone')")
            .execute(&system)
            .await
            .unwrap();
        for (id, created, delivered) in [
            ("fresh-waiting", "datetime('now')", "NULL"),
            ("old-waiting", "datetime('now', '-10 days')", "NULL"),
            (
                "recently-delivered",
                "datetime('now', '-3 days')",
                "datetime('now', '-3 days')",
            ),
            (
                "long-delivered",
                "datetime('now', '-9 days')",
                "datetime('now', '-8 days')",
            ),
            ("stale-waiting", "datetime('now', '-31 days')", "NULL"),
        ] {
            sqlx::query(&format!(
                "INSERT INTO notifications \
                 (id, device_id, category, title, body, timestamp, created_at, delivered_at) \
                 VALUES (?, 'phone', 'info', 't', 'b', '', {created}, {delivered})"
            ))
            .bind(id)
            .execute(&system)
            .await
            .unwrap();
        }

        prune_once(&logs, &system, &PruningConfig::default()).await;

        let mut left: Vec<String> = sqlx::query_scalar("SELECT id FROM notifications")
            .fetch_all(&system)
            .await
            .unwrap();
        left.sort();
        assert_eq!(
            left,
            vec!["fresh-waiting", "old-waiting", "recently-delivered"],
            "undelivered rows survive until the age cap; delivered ones only the audit window"
        );
    }

    #[tokio::test]
    async fn prune_notifications_caps_rows_per_device() {
        let (logs, system, _tmp) = make_pools().await;
        sqlx::query("INSERT INTO devices (id, name) VALUES ('phone', 'Phone')")
            .execute(&system)
            .await
            .unwrap();
        for i in 0..(NOTIFICATIONS_PER_DEVICE_KEEP + 5) {
            sqlx::query(
                "INSERT INTO notifications (id, device_id, category, title, body, timestamp) \
                 VALUES (?, 'phone', 'info', 't', 'b', '')",
            )
            .bind(format!("n{i}"))
            .execute(&system)
            .await
            .unwrap();
        }

        prune_once(&logs, &system, &PruningConfig::default()).await;

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications")
            .fetch_one(&system)
            .await
            .unwrap();
        assert_eq!(count, i64::from(NOTIFICATIONS_PER_DEVICE_KEEP));
        let oldest_kept: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM notifications WHERE id = 'n0')")
                .fetch_one(&system)
                .await
                .unwrap();
        assert!(!oldest_kept, "the oldest rows go first");
    }

    #[tokio::test]
    async fn prune_notification_history_applies_the_queue_s_age_and_count_caps() {
        let (logs, system, _tmp) = make_pools().await;
        for (id, created) in [
            ("fresh", "datetime('now')"),
            ("old", "datetime('now', '-20 days')"),
            ("stale", "datetime('now', '-31 days')"),
        ] {
            sqlx::query(&format!(
                "INSERT INTO notification_history \
                 (id, audience, category, title, body, timestamp, created_at) \
                 VALUES (?, 'household', 'info', 't', 'b', '', {created})"
            ))
            .bind(id)
            .execute(&system)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO notification_history_reads (notification_id, device_id) \
             VALUES ('stale', 'dashboard'), ('fresh', 'dashboard')",
        )
        .execute(&system)
        .await
        .unwrap();

        prune_once(&logs, &system, &PruningConfig::default()).await;

        let mut left: Vec<String> = sqlx::query_scalar("SELECT id FROM notification_history")
            .fetch_all(&system)
            .await
            .unwrap();
        left.sort();
        assert_eq!(left, vec!["fresh", "old"]);
        let reads: Vec<String> =
            sqlx::query_scalar("SELECT notification_id FROM notification_history_reads")
                .fetch_all(&system)
                .await
                .unwrap();
        assert_eq!(
            reads,
            vec!["fresh"],
            "a pruned notice's read marks go with it"
        );

        for i in 0..(NOTIFICATIONS_PER_DEVICE_KEEP + 3) {
            sqlx::query(
                "INSERT INTO notification_history \
                 (id, audience, category, title, body, timestamp) \
                 VALUES (?, 'household', 'info', 't', 'b', '')",
            )
            .bind(format!("n{i}"))
            .execute(&system)
            .await
            .unwrap();
        }
        prune_once(&logs, &system, &PruningConfig::default()).await;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_history")
            .fetch_one(&system)
            .await
            .unwrap();
        assert_eq!(count, i64::from(NOTIFICATIONS_PER_DEVICE_KEEP));
        let oldest_kept: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM notification_history WHERE id = 'old')",
        )
        .fetch_one(&system)
        .await
        .unwrap();
        assert!(!oldest_kept, "the oldest rows go first");
    }

    #[tokio::test]
    async fn prune_is_idempotent() {
        let (logs, system, _tmp) = make_pools().await;
        let config = PruningConfig::default();
        // Running on empty tables should not error
        prune_once(&logs, &system, &config).await;
        prune_once(&logs, &system, &config).await;
    }

    #[tokio::test]
    async fn prune_events_honors_category_override_and_sensitivity_cap() {
        use pond_core::security::domain::event::Event;

        let (logs, system, _tmp) = make_pools().await;
        let log = SqliteEventLog::new(logs.clone());
        let now = Utc::now();

        let append_aged = |cat, action: &str, sens, age_days: i64| {
            let log = &log;
            let action = action.to_string();
            async move {
                let mut e = Event::new(cat, action).sensitivity(sens);
                e.timestamp = now - chrono::Duration::days(age_days);
                log.append(e).await.unwrap();
            }
        };

        // Network: 20d old. Baseline 30d would keep it, but a 14d override prunes it.
        append_aged(
            EventCategory::Network,
            "egress.http",
            PrivacySensitivity::Internal,
            20,
        )
        .await;
        // Sensor: 10d old, no override → baseline 30d keeps it.
        append_aged(
            EventCategory::Sensor,
            "sensor.reading",
            PrivacySensitivity::Internal,
            10,
        )
        .await;
        // Auth secret: 5d old → baseline 30d would keep, but 3d sensitivity cap prunes it.
        append_aged(
            EventCategory::Auth,
            "auth.token",
            PrivacySensitivity::Secret,
            5,
        )
        .await;
        // System: 2d old → kept by everything.
        append_aged(
            EventCategory::System,
            "system.tick",
            PrivacySensitivity::Internal,
            2,
        )
        .await;

        let mut by_category = HashMap::new();
        by_category.insert("network".to_string(), 14u32);
        let config = PruningConfig {
            events_days: 30,
            events_by_category: by_category,
            events_sensitive_days: 3,
            ..Default::default()
        };

        prune_once(&logs, &system, &config).await;

        let remaining = log.query(EventQuery::default()).await.unwrap();
        let actions: Vec<&str> = remaining.iter().map(|e| e.action.as_str()).collect();
        assert_eq!(
            remaining.len(),
            2,
            "network (override) + secret (cap) pruned"
        );
        assert!(actions.contains(&"sensor.reading"));
        assert!(actions.contains(&"system.tick"));
        assert!(!actions.contains(&"egress.http"));
        assert!(!actions.contains(&"auth.token"));
    }
}
