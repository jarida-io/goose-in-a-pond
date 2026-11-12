//! Driven port: the producers' side of `hook_events`, the one queue a to-do hook reads. A price
//! change, a webhook call, a new mail or a calendar event is written here without knowing which
//! hooks, if any, will match it.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

/// One event, in the vocabulary of `docs/architecture/my-goose-hub.md`.
#[derive(Debug, Clone, PartialEq)]
pub struct NewHookEvent {
    /// Names the event so a second write of it is dropped; `None` for a fresh id.
    pub once_key: Option<String>,
    /// The member it is about; `None` for a household event any member's hook may match.
    pub profile_id: Option<String>,
    pub source_kind: String,
    pub source_id: String,
    pub event: String,
    pub payload: Value,
    pub at: DateTime<Utc>,
}

impl NewHookEvent {
    pub fn new(
        profile_id: Option<&str>,
        source_kind: &str,
        source_id: &str,
        event: &str,
        payload: Value,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            once_key: None,
            profile_id: profile_id.map(str::to_string),
            source_kind: source_kind.to_string(),
            source_id: source_id.to_string(),
            event: event.to_string(),
            payload,
            at,
        }
    }

    pub fn once(mut self, key: impl Into<String>) -> Self {
        self.once_key = Some(key.into());
        self
    }
}

#[async_trait]
pub trait HookEventSink: Send + Sync {
    /// `false` when an event with the same `once_key` was already written.
    async fn record(&self, event: NewHookEvent) -> Result<bool>;
}
