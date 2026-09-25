//! Redacts event attributes before they reach the log.
//! A decorator: every `EventLog::append` caller shares the one `Arc` built in `run_server`.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::security::domain::event::{AttributeValue, Event, EventQuery, PrivacySensitivity};
use crate::security::domain::redaction::RedactionLevel;
use crate::security::ports::event_log::EventLog;
use crate::security::ports::redactor::Redactor;

/// Attribute recording how many findings were removed. A count, never text.
pub const REDACTED_ATTRIBUTE: &str = "redacted";

pub struct RedactingEventLog {
    inner: Arc<dyn EventLog>,
    redactor: Arc<dyn Redactor>,
}

impl RedactingEventLog {
    /// `Full`, not `Secrets`: attributes are telemetry, surfaced by the activity API.
    pub const LEVEL: RedactionLevel = RedactionLevel::Full;

    pub fn new(inner: Arc<dyn EventLog>, redactor: Arc<dyn Redactor>) -> Self {
        Self { inner, redactor }
    }

    fn scrub(&self, mut event: Event) -> Event {
        let mut highest: Option<PrivacySensitivity> = None;
        let mut removed = 0i64;
        for value in event.attributes.values_mut() {
            let text = match value {
                AttributeValue::Text(t) => t,
                _ => continue,
            };
            let result = self.redactor.redact(text, Self::LEVEL);
            if result.findings.is_empty() {
                continue;
            }
            removed += result.findings.len() as i64;
            if let Some(s) = result.highest_sensitivity() {
                highest = Some(highest.map_or(s, |h| h.max(s)));
            }
            *text = result.text;
        }
        if let Some(s) = highest {
            // Raise, never lower: a higher sensitivity only narrows surfacing and retention.
            if event.privacy_sensitivity < s {
                event.privacy_sensitivity = s;
            }
            event = event.attr(REDACTED_ATTRIBUTE, removed);
        }
        event
    }
}

#[async_trait]
impl EventLog for RedactingEventLog {
    async fn append(&self, event: Event) -> Result<()> {
        self.inner.append(self.scrub(event)).await
    }

    async fn query(&self, query: EventQuery) -> Result<Vec<Event>> {
        self.inner.query(query).await
    }

    async fn purge(&self, query: EventQuery) -> Result<u64> {
        self.inner.purge(query).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use crate::security::domain::event::EventCategory;
    use crate::security::domain::redaction::RedactionKind;
    use crate::security::mocks::mock_redactor::MockRedactor;

    const KEY: &str = "sk-abcdefghijklmnopqrstuvwxyz123456";

    struct CapturingLog(Mutex<Vec<Event>>);

    #[async_trait]
    impl EventLog for CapturingLog {
        async fn append(&self, event: Event) -> Result<()> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
        async fn query(&self, _q: EventQuery) -> Result<Vec<Event>> {
            Ok(self.0.lock().unwrap().clone())
        }
        async fn purge(&self, _q: EventQuery) -> Result<u64> {
            Ok(0)
        }
    }

    fn wire(kind: RedactionKind) -> (Arc<CapturingLog>, RedactingEventLog) {
        let inner = Arc::new(CapturingLog(Mutex::new(Vec::new())));
        let log =
            RedactingEventLog::new(inner.clone(), Arc::new(MockRedactor::replacing(KEY, kind)));
        (inner, log)
    }

    #[tokio::test]
    async fn a_text_attribute_is_redacted_in_place() {
        let (inner, log) = wire(RedactionKind::ApiKey);
        log.append(
            Event::new(EventCategory::Tool, "tool.call")
                .attr("note", format!("used {KEY} for the call"))
                .attr("latency_ms", 42_i64),
        )
        .await
        .unwrap();

        let seen = inner.0.lock().unwrap().clone();
        let note = match seen[0].attributes.get("note").unwrap() {
            AttributeValue::Text(t) => t.clone(),
            other => panic!("note became {other:?}"),
        };
        assert!(!note.contains(KEY), "{note}");
        assert!(note.contains("for the call"), "prose was mangled: {note}");
        // Non-text attributes cannot carry prose and must not be disturbed.
        assert_eq!(
            seen[0].attributes.get("latency_ms"),
            Some(&AttributeValue::Int(42))
        );
        // A credential-class finding narrows both surfacing and retention.
        assert_eq!(seen[0].privacy_sensitivity, PrivacySensitivity::Secret);
        assert_eq!(
            seen[0].attributes.get(REDACTED_ATTRIBUTE),
            Some(&AttributeValue::Int(1))
        );
    }

    #[tokio::test]
    async fn an_event_with_no_finding_passes_through_identical() {
        let (inner, log) = wire(RedactionKind::ApiKey);
        let original = Event::new(EventCategory::Network, "egress.http")
            .attr("host", "api.open-meteo.com")
            .attr("status", 200_i64);
        log.append(original.clone()).await.unwrap();

        let seen = inner.0.lock().unwrap().clone();
        assert_eq!(seen[0].attributes, original.attributes);
        assert_eq!(seen[0].privacy_sensitivity, original.privacy_sensitivity);
        assert!(!seen[0].attributes.contains_key(REDACTED_ATTRIBUTE));
    }

    #[tokio::test]
    async fn sensitivity_is_raised_never_lowered() {
        // An email finding is Sensitive; the event is already Secret.
        let (inner, log) = wire(RedactionKind::EmailAddress);
        log.append(
            Event::new(EventCategory::Auth, "security.audit")
                .attr("principal", format!("token:{KEY}"))
                .sensitivity(PrivacySensitivity::Secret),
        )
        .await
        .unwrap();
        let seen = inner.0.lock().unwrap().clone();
        assert_eq!(seen[0].privacy_sensitivity, PrivacySensitivity::Secret);
    }

    #[tokio::test]
    async fn reads_and_purges_go_straight_through() {
        let (inner, log) = wire(RedactionKind::ApiKey);
        inner
            .append(Event::new(EventCategory::System, "system.start"))
            .await
            .unwrap();
        assert_eq!(log.query(EventQuery::default()).await.unwrap().len(), 1);
        assert_eq!(log.purge(EventQuery::default()).await.unwrap(), 0);
    }
}
