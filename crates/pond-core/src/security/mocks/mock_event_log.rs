//! In-memory [`EventLog`] test double (#108).
//!
//! Append-only `Vec` with filtered, newest-first querying — lets Core tests and
//! emitters exercise the unified event model before the durable SQLite adapter
//! (Q2-32) exists.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;

use crate::security::domain::event::{Event, EventQuery};
use crate::security::ports::event_log::EventLog;

/// Test double for [`EventLog`]. Thread-safe; cheap to share via `Arc`.
pub struct MockEventLog {
    events: Mutex<Vec<Event>>,
}

impl MockEventLog {
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    /// All appended events, insertion order (for assertions).
    pub fn all(&self) -> Vec<Event> {
        self.events.lock().unwrap().clone()
    }
}

impl Default for MockEventLog {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventLog for MockEventLog {
    async fn append(&self, event: Event) -> Result<()> {
        self.events.lock().unwrap().push(event);
        Ok(())
    }

    async fn query(&self, query: EventQuery) -> Result<Vec<Event>> {
        let events = self.events.lock().unwrap();
        let mut out: Vec<Event> = events
            .iter()
            .filter(|e| query.matches(e))
            .cloned()
            .collect();
        // Newest first.
        out.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        if let Some(limit) = query.limit {
            out.truncate(limit);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::domain::event::{EventCategory, PrivacySensitivity};
    use std::sync::Arc;

    #[tokio::test]
    async fn append_then_query_roundtrips_and_preserves_privacy() {
        let log = MockEventLog::new();
        log.append(
            Event::new(EventCategory::Auth, "auth.pair")
                .attr("device", "phone-1")
                .sensitivity(PrivacySensitivity::Sensitive),
        )
        .await
        .unwrap();

        let all = log.query(EventQuery::default()).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].action, "auth.pair");
        assert_eq!(all[0].privacy_sensitivity, PrivacySensitivity::Sensitive);
    }

    #[tokio::test]
    async fn query_filters_by_category_and_session() {
        let log = MockEventLog::new();
        log.append(Event::new(EventCategory::Sensor, "sensor.reading").session("s1"))
            .await
            .unwrap();
        log.append(Event::new(EventCategory::Tool, "tool.call").session("s1"))
            .await
            .unwrap();
        log.append(Event::new(EventCategory::Sensor, "sensor.reading").session("s2"))
            .await
            .unwrap();

        let by_cat = log
            .query(EventQuery {
                category: Some(EventCategory::Sensor),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(by_cat.len(), 2);

        let by_session = log
            .query(EventQuery {
                session_id: Some("s1".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(by_session.len(), 2);
    }

    #[tokio::test]
    async fn query_honors_limit_newest_first() {
        let log = MockEventLog::new();
        for i in 0..5 {
            log.append(Event::new(
                EventCategory::System,
                format!("system.tick.{i}"),
            ))
            .await
            .unwrap();
        }
        let limited = log
            .query(EventQuery {
                limit: Some(2),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(limited.len(), 2);
    }

    #[tokio::test]
    async fn usable_as_trait_object() {
        let log: Arc<dyn EventLog> = Arc::new(MockEventLog::new());
        log.append(Event::new(EventCategory::System, "system.boot"))
            .await
            .unwrap();
        assert_eq!(log.query(EventQuery::default()).await.unwrap().len(), 1);
    }
}
