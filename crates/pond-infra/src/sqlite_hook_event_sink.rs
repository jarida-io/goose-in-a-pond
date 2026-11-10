//! SQLite-backed `HookEventSink`: appends to `hook_events` (migration 0088).

use anyhow::Result;
use async_trait::async_trait;
use sqlx::{Pool, Sqlite};

use pond_core::shared::ports::hook_event_sink::{HookEventSink, NewHookEvent};

pub struct SqliteHookEventSink {
    pool: Pool<Sqlite>,
}

impl SqliteHookEventSink {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl HookEventSink for SqliteHookEventSink {
    async fn record(&self, event: NewHookEvent) -> Result<bool> {
        // A once-key is the row's id, so the primary key drops a second write of the same event.
        let id = event
            .once_key
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let done = sqlx::query(
            "INSERT OR IGNORE INTO hook_events \
             (id, profile_id, source_kind, source_id, event, payload, at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(&event.profile_id)
        .bind(&event.source_kind)
        .bind(&event.source_id)
        .bind(&event.event)
        .bind(event.payload.to_string())
        .bind(event.at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use sqlx::Row;

    #[tokio::test]
    async fn an_event_is_written_and_a_once_key_writes_it_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::init(dir.path()).await.unwrap();
        let sink = SqliteHookEventSink::new(db.system.clone());
        let now = chrono::Utc::now();
        let event = NewHookEvent::new(
            Some("jerry"),
            "connector",
            "calendar:calendar:icloud:jerry",
            "event_starting",
            serde_json::json!({"title": "MCP Dev Summit"}),
            now,
        )
        .once("calendar:x:starting");
        assert!(sink.record(event.clone()).await.unwrap());
        assert!(!sink.record(event).await.unwrap());
        assert!(sink
            .record(NewHookEvent::new(
                None,
                "presence",
                "home",
                "arrive",
                serde_json::json!({}),
                now
            ))
            .await
            .unwrap());
        let rows = sqlx::query("SELECT source_kind, payload FROM hook_events ORDER BY source_kind")
            .fetch_all(&db.system)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<String, _>("source_kind"), "connector");
        assert_eq!(
            rows[0].get::<String, _>("payload"),
            r#"{"title":"MCP Dev Summit"}"#
        );
    }
}
