//! In-memory [`TelemetryPort`] for tests and lightweight deployments.

use crate::security::domain::turn_metrics::{TelemetrySummary, TurnMetrics};
use crate::security::ports::telemetry::TelemetryPort;
use tokio::sync::RwLock;

/// In-memory telemetry store; `RwLock` because API reads outnumber writes (one per turn).
pub struct InMemoryTelemetry {
    turns: RwLock<Vec<TurnMetrics>>,
}

impl InMemoryTelemetry {
    pub fn new() -> Self {
        Self {
            turns: RwLock::new(Vec::new()),
        }
    }
}

impl Default for InMemoryTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl TelemetryPort for InMemoryTelemetry {
    async fn record_turn(&self, metrics: TurnMetrics) -> Result<(), String> {
        self.turns.write().await.push(metrics);
        Ok(())
    }

    async fn get_turns(&self, session_id: &str) -> Result<Vec<TurnMetrics>, String> {
        let guard = self.turns.read().await;
        let mut turns: Vec<TurnMetrics> = guard
            .iter()
            .filter(|t| t.session_id == session_id)
            .cloned()
            .collect();
        turns.sort_by_key(|t| t.turn_number);
        Ok(turns)
    }

    async fn get_summary(&self, session_id: &str) -> Result<TelemetrySummary, String> {
        let guard = self.turns.read().await;
        let session_turns: Vec<&TurnMetrics> = guard
            .iter()
            .filter(|t| t.session_id == session_id)
            .collect();

        if session_turns.is_empty() {
            return Ok(TelemetrySummary {
                avg_ttft_ms: 0.0,
                avg_completion_tokens: 0.0,
                total_turns: 0,
                avg_context_utilization: 0.0,
            });
        }

        let n = session_turns.len() as f64;
        let sum_ttft: u64 = session_turns.iter().map(|t| t.ttft_ms).sum();
        let sum_completion: u32 = session_turns.iter().map(|t| t.completion_tokens).sum();
        let sum_ctx: f64 = session_turns
            .iter()
            .map(|t| t.context_utilization_pct as f64)
            .sum();

        Ok(TelemetrySummary {
            avg_ttft_ms: sum_ttft as f64 / n,
            avg_completion_tokens: sum_completion as f64 / n,
            total_turns: session_turns.len() as u32,
            avg_context_utilization: sum_ctx / n,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_turn(session_id: &str, turn_number: u32) -> TurnMetrics {
        TurnMetrics {
            session_id: session_id.to_string(),
            turn_number,
            prompt_tokens: 100 * turn_number,
            completion_tokens: 50 * turn_number,
            ttft_ms: 200 + (turn_number as u64 * 10),
            total_latency_ms: 1000 + (turn_number as u64 * 100),
            tool_name: None,
            tool_latency_ms: None,
            tool_cache_hit: None,
            context_utilization_pct: 30.0 + turn_number as f32,
            model_name: "test-model".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            prefill_ms: None,
            model_load_ms: None,
            decode_tok_per_sec: None,
            prefill_tok_per_sec: None,
            prefilled_tokens: None,
            reused_prefix_tokens: None,
            context_limit_tokens: None,
            inference_count: None,
            reasoning_tokens: None,
            reengagements: None,
        }
    }

    #[tokio::test]
    async fn record_and_retrieve_turn() {
        let telemetry = InMemoryTelemetry::new();
        let turn = make_turn("sess-1", 1);

        telemetry.record_turn(turn.clone()).await.unwrap();

        let turns = telemetry.get_turns("sess-1").await.unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].session_id, "sess-1");
        assert_eq!(turns[0].turn_number, 1);
        assert_eq!(turns[0].prompt_tokens, 100);
        assert_eq!(turns[0].completion_tokens, 50);
        assert_eq!(turns[0].ttft_ms, 210);
    }

    #[tokio::test]
    async fn summary_computes_correct_averages() {
        let telemetry = InMemoryTelemetry::new();

        // make_turn(n): ttft 200+10n, completion 50n, ctx 30+n.
        telemetry.record_turn(make_turn("sess-1", 1)).await.unwrap();
        telemetry.record_turn(make_turn("sess-1", 2)).await.unwrap();
        telemetry.record_turn(make_turn("sess-1", 3)).await.unwrap();

        let summary = telemetry.get_summary("sess-1").await.unwrap();
        assert_eq!(summary.total_turns, 3);
        assert!((summary.avg_ttft_ms - 220.0).abs() < 0.01);
        assert!((summary.avg_completion_tokens - 100.0).abs() < 0.01);
        assert!((summary.avg_context_utilization - 32.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn sessions_are_isolated() {
        let telemetry = InMemoryTelemetry::new();

        telemetry.record_turn(make_turn("sess-a", 1)).await.unwrap();
        telemetry.record_turn(make_turn("sess-a", 2)).await.unwrap();
        telemetry.record_turn(make_turn("sess-b", 1)).await.unwrap();

        let turns_a = telemetry.get_turns("sess-a").await.unwrap();
        let turns_b = telemetry.get_turns("sess-b").await.unwrap();

        assert_eq!(turns_a.len(), 2);
        assert_eq!(turns_b.len(), 1);

        let summary_a = telemetry.get_summary("sess-a").await.unwrap();
        let summary_b = telemetry.get_summary("sess-b").await.unwrap();

        assert_eq!(summary_a.total_turns, 2);
        assert_eq!(summary_b.total_turns, 1);
    }

    #[tokio::test]
    async fn empty_session_returns_zero_summary() {
        let telemetry = InMemoryTelemetry::new();

        let summary = telemetry.get_summary("nonexistent").await.unwrap();
        assert_eq!(summary.total_turns, 0);
        assert_eq!(summary.avg_ttft_ms, 0.0);
        assert_eq!(summary.avg_completion_tokens, 0.0);
        assert_eq!(summary.avg_context_utilization, 0.0);
    }

    #[tokio::test]
    async fn turns_returned_in_order() {
        let telemetry = InMemoryTelemetry::new();

        telemetry.record_turn(make_turn("sess-1", 3)).await.unwrap();
        telemetry.record_turn(make_turn("sess-1", 1)).await.unwrap();
        telemetry.record_turn(make_turn("sess-1", 2)).await.unwrap();

        let turns = telemetry.get_turns("sess-1").await.unwrap();
        assert_eq!(turns[0].turn_number, 1);
        assert_eq!(turns[1].turn_number, 2);
        assert_eq!(turns[2].turn_number, 3);
    }

    #[tokio::test]
    async fn tool_metadata_round_trips() {
        let telemetry = InMemoryTelemetry::new();

        let mut turn = make_turn("sess-1", 1);
        turn.tool_name = Some("weather".to_string());
        turn.tool_latency_ms = Some(350);
        turn.tool_cache_hit = Some(false);

        telemetry.record_turn(turn).await.unwrap();

        let turns = telemetry.get_turns("sess-1").await.unwrap();
        assert_eq!(turns[0].tool_name.as_deref(), Some("weather"));
        assert_eq!(turns[0].tool_latency_ms, Some(350));
        assert_eq!(turns[0].tool_cache_hit, Some(false));
    }
}
