use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::security::domain::event::{Event, EventQuery};

/// A row of drained tracing output from `pond_logs.db`'s misnamed `event_log` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationalLogEntry {
    pub id: i64,
    pub timestamp: String,
    /// Severity: `"INFO"` | `"WARN"` | `"ERROR"`
    pub level: String,
    /// Component or subsystem that emitted the event (e.g. `"agent"`, `"pond-server"`).
    pub source: String,
    pub message: String,
    /// Optional JSON metadata blob.
    pub metadata: Option<String>,
}

/// Driven Port: the operational log viewer's store. Only `pond-server`'s tracing drain writes
/// here; this is not the audit trail (see [`crate::security::ports::audit`]).
#[async_trait]
pub trait OperationalLogRepository: Send + Sync {
    /// Return up to `limit` recent entries, optionally filtered by `level`.
    async fn list(&self, limit: u32, level: Option<&str>) -> Result<Vec<OperationalLogEntry>>;
    async fn insert(
        &self,
        level: &str,
        source: &str,
        message: &str,
        metadata: Option<&str>,
    ) -> Result<()>;
}

/// Driven Port: the unified, append-only event log, the authoritative record of what the
/// assistant did. Prefer it for anything semantic.
#[async_trait]
pub trait EventLog: Send + Sync {
    /// Append a single event. Append-only — events are never mutated.
    async fn append(&self, event: Event) -> Result<()>;

    /// Return matching events, newest first, honoring `query.limit`.
    async fn query(&self, query: EventQuery) -> Result<Vec<Event>>;

    /// Delete every event matching `query`, ignoring `limit`; returns the rows removed.
    async fn purge(&self, query: EventQuery) -> Result<u64>;
}
