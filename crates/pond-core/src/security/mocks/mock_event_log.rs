//! In-memory [`EventLog`] test double standing in for the durable SQLite adapter.

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

    async fn purge(&self, query: EventQuery) -> Result<u64> {
        let mut events = self.events.lock().unwrap();
        let before = events.len();
        events.retain(|e| !query.matches(e));
        Ok((before - events.len()) as u64)
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
    async fn purge_by_category_and_min_sensitivity() {
        let log = MockEventLog::new();
        log.append(Event::new(EventCategory::Network, "egress.http").session("s1"))
            .await
            .unwrap();
        log.append(
            Event::new(EventCategory::Auth, "auth.token").sensitivity(PrivacySensitivity::Secret),
        )
        .await
        .unwrap();
        log.append(Event::new(EventCategory::Sensor, "sensor.reading"))
            .await
            .unwrap();

        // Purge just the Network category.
        let n = log
            .purge(EventQuery {
                category: Some(EventCategory::Network),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(log.all().len(), 2);

        // Purge everything at/above Sensitive — removes the Secret auth event.
        let n = log
            .purge(EventQuery {
                min_sensitivity: Some(PrivacySensitivity::Sensitive),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(n, 1);
        let remaining = log.all();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].category, EventCategory::Sensor);

        // Filterless purge clears the rest ("clear my activity").
        let n = log.purge(EventQuery::default()).await.unwrap();
        assert_eq!(n, 1);
        assert!(log.all().is_empty());
    }

    #[tokio::test]
    async fn query_max_sensitivity_excludes_secret_and_limit_counts_visible() {
        let log = MockEventLog::new();
        log.append(
            Event::new(EventCategory::Auth, "auth.token").sensitivity(PrivacySensitivity::Secret),
        )
        .await
        .unwrap();
        log.append(Event::new(EventCategory::Sensor, "sensor.reading"))
            .await
            .unwrap();
        log.append(
            Event::new(EventCategory::Network, "egress.http")
                .sensitivity(PrivacySensitivity::Sensitive),
        )
        .await
        .unwrap();

        // The filtered Secret row never consumes limit budget.
        let visible = log
            .query(EventQuery {
                max_sensitivity: Some(PrivacySensitivity::Sensitive),
                limit: Some(2),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(visible.len(), 2);
        assert!(visible.iter().all(|e| e.action != "auth.token"));

        // min + max combine: exactly the Sensitive band.
        let band = log
            .query(EventQuery {
                min_sensitivity: Some(PrivacySensitivity::Sensitive),
                max_sensitivity: Some(PrivacySensitivity::Sensitive),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(band.len(), 1);
        assert_eq!(band[0].action, "egress.http");
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
