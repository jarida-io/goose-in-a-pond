//! SQLite-backed event-log adapters (`pond_logs.db`).
//!
//! - [`SqliteEventLogRepository`] — the legacy `event_log` table (migration 0001),
//!   still used by the desktop Logs screen.
//! - [`SqliteEventLog`] — the unified, typed, append-only `events` table
//!   (migration 0004, #109) implementing [`EventLog`].

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Serialize};
use sqlx::{Pool, Row, Sqlite};
use std::sync::Arc;

use pond_core::security::domain::event::{Event, EventQuery};
use pond_core::security::ports::event_log::{EventLog, EventLogRepository, LogEntry};

pub struct SqliteEventLogRepository {
    pool: Pool<Sqlite>,
}

impl SqliteEventLogRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl EventLogRepository for SqliteEventLogRepository {
    async fn list(&self, limit: u32, level: Option<&str>) -> Result<Vec<LogEntry>> {
        let rows = match level {
            Some(lvl) => {
                sqlx::query(
                    "SELECT id, timestamp, level, source, message, metadata \
                     FROM event_log \
                     WHERE level = ? \
                     ORDER BY id DESC LIMIT ?",
                )
                .bind(lvl)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, timestamp, level, source, message, metadata \
                     FROM event_log \
                     ORDER BY id DESC LIMIT ?",
                )
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?
            }
        };

        Ok(rows
            .iter()
            .map(|r| LogEntry {
                id: r.get("id"),
                timestamp: r.get("timestamp"),
                level: r.get("level"),
                source: r.get("source"),
                message: r.get("message"),
                metadata: r.get("metadata"),
            })
            .collect())
    }

    async fn insert(
        &self,
        level: &str,
        source: &str,
        message: &str,
        metadata: Option<&str>,
    ) -> Result<()> {
        sqlx::query("INSERT INTO event_log (level, source, message, metadata) VALUES (?, ?, ?, ?)")
            .bind(level)
            .bind(source)
            .bind(message)
            .bind(metadata)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Default cap on `query` results when the caller doesn't set `EventQuery.limit`,
/// and the hard ceiling regardless of what they ask for — so a single query can
/// never pull an unbounded number of rows into memory.
const DEFAULT_QUERY_LIMIT: i64 = 500;
const MAX_QUERY_LIMIT: i64 = 5_000;

/// Render a `#[serde(rename_all = "snake_case")]` unit enum as its bare string
/// (e.g. `EventCategory::Sensor` -> `"sensor"`) for storage, via serde so the
/// on-disk value always matches the wire form.
fn enum_to_str<T: Serialize>(value: &T) -> Result<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(anyhow!("expected a string-valued enum, got {other}")),
    }
}

/// Inverse of [`enum_to_str`].
fn enum_from_str<T: DeserializeOwned>(s: &str) -> Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(
        s.to_string(),
    ))?)
}

/// Unified, append-only event store (#109) implementing [`EventLog`] over the
/// `events` table in `pond_logs.db`.
pub struct SqliteEventLog {
    pool: Pool<Sqlite>,
}

impl SqliteEventLog {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }

    /// Wrap as `Arc<dyn EventLog>`.
    pub fn into_dyn(self) -> Arc<dyn EventLog> {
        Arc::new(self)
    }

    fn row_to_event(row: &sqlx::sqlite::SqliteRow) -> Result<Event> {
        let ts: String = row.get("timestamp");
        let attributes: String = row.get("attributes");
        Ok(Event {
            category: enum_from_str(&row.get::<String, _>("category"))?,
            action: row.get("action"),
            session_id: row.get::<Option<String>, _>("session_id"),
            trace_id: row.get::<Option<String>, _>("trace_id"),
            attributes: serde_json::from_str(&attributes)?,
            privacy_sensitivity: enum_from_str(&row.get::<String, _>("privacy_sensitivity"))?,
            timestamp: DateTime::parse_from_rfc3339(&ts)
                .map_err(|e| anyhow!("bad timestamp {ts:?}: {e}"))?
                .with_timezone(&Utc),
        })
    }
}

#[async_trait]
impl EventLog for SqliteEventLog {
    async fn append(&self, event: Event) -> Result<()> {
        sqlx::query(
            "INSERT INTO events
                (timestamp, category, action, session_id, trace_id, attributes, privacy_sensitivity)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(event.timestamp.to_rfc3339())
        .bind(enum_to_str(&event.category)?)
        .bind(&event.action)
        .bind(&event.session_id)
        .bind(&event.trace_id)
        .bind(serde_json::to_string(&event.attributes)?)
        .bind(enum_to_str(&event.privacy_sensitivity)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn query(&self, query: EventQuery) -> Result<Vec<Event>> {
        // Build a fully parameterized statement — every filter is a bound `?`,
        // never string-interpolated, so untrusted filter values can't inject SQL.
        let mut sql = String::from(
            "SELECT id, timestamp, category, action, session_id, trace_id, attributes, \
             privacy_sensitivity FROM events WHERE 1 = 1",
        );
        if query.category.is_some() {
            sql.push_str(" AND category = ?");
        }
        if query.session_id.is_some() {
            sql.push_str(" AND session_id = ?");
        }
        if query.trace_id.is_some() {
            sql.push_str(" AND trace_id = ?");
        }
        if query.since.is_some() {
            sql.push_str(" AND timestamp >= ?");
        }
        if query.until.is_some() {
            sql.push_str(" AND timestamp < ?");
        }
        sql.push_str(" ORDER BY timestamp DESC, id DESC LIMIT ?");

        let mut q = sqlx::query(&sql);
        if let Some(category) = &query.category {
            q = q.bind(enum_to_str(category)?);
        }
        if let Some(session_id) = &query.session_id {
            q = q.bind(session_id.clone());
        }
        if let Some(trace_id) = &query.trace_id {
            q = q.bind(trace_id.clone());
        }
        if let Some(since) = query.since {
            q = q.bind(since.to_rfc3339());
        }
        if let Some(until) = query.until {
            q = q.bind(until.to_rfc3339());
        }
        let limit = query
            .limit
            .map(|l| l as i64)
            .unwrap_or(DEFAULT_QUERY_LIMIT)
            .clamp(1, MAX_QUERY_LIMIT);
        q = q.bind(limit);

        let rows = q.fetch_all(&self.pool).await?;
        rows.iter().map(Self::row_to_event).collect()
    }
}

#[cfg(test)]
mod event_log_tests {
    use super::*;
    use crate::db::Database;
    use pond_core::security::domain::event::{
        AttributeValue, Event, EventCategory, EventQuery, PrivacySensitivity,
    };
    use tempfile::tempdir;

    async fn fresh() -> SqliteEventLog {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let log = SqliteEventLog::new(db.logs.clone());
        std::mem::forget(tmp); // keep the sqlite file alive for the test
        log
    }

    #[tokio::test]
    async fn append_then_query_roundtrips_typed_fields() {
        let log = fresh().await;
        let event = Event::new(EventCategory::Sensor, "sensor.reading")
            .attr("device_id", "sensor-1")
            .attr("value", 21.5_f64)
            .session("sess-1")
            .sensitivity(PrivacySensitivity::Sensitive);
        log.append(event).await.unwrap();

        let got = log.query(EventQuery::default()).await.unwrap();
        assert_eq!(got.len(), 1);
        let e = &got[0];
        assert_eq!(e.category, EventCategory::Sensor);
        assert_eq!(e.action, "sensor.reading");
        assert_eq!(e.session_id.as_deref(), Some("sess-1"));
        assert_eq!(e.privacy_sensitivity, PrivacySensitivity::Sensitive);
        assert_eq!(
            e.attributes.get("value"),
            Some(&AttributeValue::Float(21.5))
        );
        assert_eq!(
            e.attributes.get("device_id"),
            Some(&AttributeValue::Text("sensor-1".into()))
        );
    }

    #[tokio::test]
    async fn query_filters_by_category_and_session() {
        let log = fresh().await;
        log.append(Event::new(EventCategory::Sensor, "sensor.reading").session("a"))
            .await
            .unwrap();
        log.append(Event::new(EventCategory::Camera, "camera.event").session("b"))
            .await
            .unwrap();

        let sensors = log
            .query(EventQuery {
                category: Some(EventCategory::Sensor),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(sensors.len(), 1);
        assert_eq!(sensors[0].category, EventCategory::Sensor);

        let sess_b = log
            .query(EventQuery {
                session_id: Some("b".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(sess_b.len(), 1);
        assert_eq!(sess_b[0].session_id.as_deref(), Some("b"));
    }

    #[tokio::test]
    async fn query_honors_limit_newest_first() {
        let log = fresh().await;
        for i in 0..5 {
            // Distinct, increasing timestamps so ordering is deterministic.
            let mut e = Event::new(EventCategory::System, format!("evt.{i}"));
            e.timestamp = chrono::Utc::now() + chrono::Duration::seconds(i);
            log.append(e).await.unwrap();
        }
        let got = log
            .query(EventQuery {
                limit: Some(2),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(got.len(), 2);
        // Newest first.
        assert_eq!(got[0].action, "evt.4");
        assert_eq!(got[1].action, "evt.3");
    }

    #[tokio::test]
    async fn query_filters_by_time_window() {
        let log = fresh().await;
        let base = chrono::Utc::now();
        for i in 0..3 {
            let mut e = Event::new(EventCategory::System, format!("evt.{i}"));
            e.timestamp = base + chrono::Duration::minutes(i);
            log.append(e).await.unwrap();
        }
        // [base+1min, base+2min) → only evt.1.
        let got = log
            .query(EventQuery {
                since: Some(base + chrono::Duration::minutes(1)),
                until: Some(base + chrono::Duration::minutes(2)),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].action, "evt.1");
    }

    #[tokio::test]
    async fn usable_as_trait_object() {
        let log: Arc<dyn EventLog> = Arc::new(fresh().await);
        log.append(Event::new(EventCategory::System, "system.start"))
            .await
            .unwrap();
        assert_eq!(log.query(EventQuery::default()).await.unwrap().len(), 1);
    }
}
