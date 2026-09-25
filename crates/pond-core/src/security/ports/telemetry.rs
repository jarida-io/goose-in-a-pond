//! Port trait for recording and querying per-turn telemetry metrics.

use crate::security::domain::turn_metrics::{TelemetrySummary, TurnMetrics};

/// Driven port for turn-level telemetry storage and retrieval.
#[async_trait::async_trait]
pub trait TelemetryPort: Send + Sync {
    /// Record metrics for a completed chat turn.
    async fn record_turn(&self, metrics: TurnMetrics) -> Result<(), String>;

    /// Retrieve all recorded turns for a session, ordered by turn number.
    async fn get_turns(&self, session_id: &str) -> Result<Vec<TurnMetrics>, String>;

    async fn get_summary(&self, session_id: &str) -> Result<TelemetrySummary, String>;
}
