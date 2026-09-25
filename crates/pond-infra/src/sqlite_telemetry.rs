//! SQLite-backed `TelemetryPort` over `turn_metrics` in `pond_logs.db`.
//!
//! Writes go to SQLite, then the cache; reads use only the cache, hydrated once at construction.

use anyhow::Result;
use sqlx::{Pool, Row, Sqlite};
use tokio::sync::RwLock;

use pond_core::security::domain::turn_metrics::{TelemetrySummary, TurnMetrics};
use pond_core::security::ports::telemetry::TelemetryPort;

pub struct SqliteTelemetry {
    pool: Pool<Sqlite>,
    cache: RwLock<Vec<TurnMetrics>>,
}

impl SqliteTelemetry {
    /// Loads every persisted turn into the in-memory cache.
    pub async fn new(pool: Pool<Sqlite>) -> Result<Self> {
        let turns = Self::load_all(&pool).await?;
        Ok(Self {
            pool,
            cache: RwLock::new(turns),
        })
    }

    async fn load_all(pool: &Pool<Sqlite>) -> Result<Vec<TurnMetrics>> {
        let rows = sqlx::query(
            "SELECT session_id, turn_number, prompt_tokens, completion_tokens, ttft_ms, \
             total_latency_ms, tool_name, tool_latency_ms, tool_cache_hit, \
             context_utilization_pct, model_name, timestamp, \
             prefill_ms, model_load_ms, decode_tok_per_sec, prefill_tok_per_sec, \
             context_limit_tokens, inference_count, reasoning_tokens, reengagements, \
             prefilled_tokens, reused_prefix_tokens \
             FROM turn_metrics ORDER BY id ASC",
        )
        .fetch_all(pool)
        .await?;

        Ok(rows.iter().map(row_to_turn_metrics).collect())
    }
}

fn row_to_turn_metrics(row: &sqlx::sqlite::SqliteRow) -> TurnMetrics {
    TurnMetrics {
        session_id: row.get("session_id"),
        turn_number: row.get::<i64, _>("turn_number") as u32,
        prompt_tokens: row.get::<i64, _>("prompt_tokens") as u32,
        completion_tokens: row.get::<i64, _>("completion_tokens") as u32,
        ttft_ms: row.get::<i64, _>("ttft_ms") as u64,
        total_latency_ms: row.get::<i64, _>("total_latency_ms") as u64,
        tool_name: row.get("tool_name"),
        tool_latency_ms: row
            .get::<Option<i64>, _>("tool_latency_ms")
            .map(|v| v as u64),
        tool_cache_hit: row.get::<Option<i64>, _>("tool_cache_hit").map(|v| v != 0),
        context_utilization_pct: row.get::<f64, _>("context_utilization_pct") as f32,
        model_name: row.get("model_name"),
        timestamp: row.get("timestamp"),
        prefill_ms: row.get::<Option<i64>, _>("prefill_ms").map(|v| v as u64),
        model_load_ms: row.get::<Option<i64>, _>("model_load_ms").map(|v| v as u64),
        decode_tok_per_sec: row
            .get::<Option<f64>, _>("decode_tok_per_sec")
            .map(|v| v as f32),
        prefill_tok_per_sec: row
            .get::<Option<f64>, _>("prefill_tok_per_sec")
            .map(|v| v as f32),
        context_limit_tokens: row
            .get::<Option<i64>, _>("context_limit_tokens")
            .map(|v| v as u32),
        inference_count: row
            .get::<Option<i64>, _>("inference_count")
            .map(|v| v as u32),
        // NULL stays None: a cached-prompt zero is a measurement, a pre-0010 NULL is not.
        prefilled_tokens: row
            .get::<Option<i64>, _>("prefilled_tokens")
            .map(|v| v as u32),
        reused_prefix_tokens: row
            .get::<Option<i64>, _>("reused_prefix_tokens")
            .map(|v| v as u32),
        // NULL stays None, not Some(0): an unmeasured zero would skew every average.
        reasoning_tokens: row
            .get::<Option<i64>, _>("reasoning_tokens")
            .map(|v| v as u32),
        reengagements: row.get::<Option<i64>, _>("reengagements").map(|v| v as u32),
    }
}

#[async_trait::async_trait]
impl TelemetryPort for SqliteTelemetry {
    async fn record_turn(&self, metrics: TurnMetrics) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO turn_metrics \
             (session_id, turn_number, prompt_tokens, completion_tokens, ttft_ms, \
              total_latency_ms, tool_name, tool_latency_ms, tool_cache_hit, \
              context_utilization_pct, model_name, timestamp, \
              prefill_ms, model_load_ms, decode_tok_per_sec, prefill_tok_per_sec, \
              context_limit_tokens, inference_count, reasoning_tokens, reengagements, \
              prefilled_tokens, reused_prefix_tokens) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&metrics.session_id)
        .bind(metrics.turn_number as i64)
        .bind(metrics.prompt_tokens as i64)
        .bind(metrics.completion_tokens as i64)
        .bind(metrics.ttft_ms as i64)
        .bind(metrics.total_latency_ms as i64)
        .bind(&metrics.tool_name)
        .bind(metrics.tool_latency_ms.map(|v| v as i64))
        .bind(metrics.tool_cache_hit.map(|v| v as i64))
        .bind(metrics.context_utilization_pct as f64)
        .bind(&metrics.model_name)
        .bind(&metrics.timestamp)
        .bind(metrics.prefill_ms.map(|v| v as i64))
        .bind(metrics.model_load_ms.map(|v| v as i64))
        .bind(metrics.decode_tok_per_sec.map(|v| v as f64))
        .bind(metrics.prefill_tok_per_sec.map(|v| v as f64))
        .bind(metrics.context_limit_tokens.map(|v| v as i64))
        .bind(metrics.inference_count.map(|v| v as i64))
        .bind(metrics.reasoning_tokens.map(|v| v as i64))
        .bind(metrics.reengagements.map(|v| v as i64))
        .bind(metrics.prefilled_tokens.map(|v| v as i64))
        .bind(metrics.reused_prefix_tokens.map(|v| v as i64))
        .execute(&self.pool)
        .await
        .map_err(|e| e.to_string())?;

        self.cache.write().await.push(metrics);
        Ok(())
    }

    async fn get_turns(&self, session_id: &str) -> Result<Vec<TurnMetrics>, String> {
        let guard = self.cache.read().await;
        let mut turns: Vec<TurnMetrics> = guard
            .iter()
            .filter(|t| t.session_id == session_id)
            .cloned()
            .collect();
        turns.sort_by_key(|t| t.turn_number);
        Ok(turns)
    }

    async fn get_summary(&self, session_id: &str) -> Result<TelemetrySummary, String> {
        let guard = self.cache.read().await;
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
    use crate::db::Database;
    use tempfile::tempdir;

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
            prefill_ms: Some(1500 + turn_number as u64),
            model_load_ms: None,
            decode_tok_per_sec: Some(22.5),
            prefill_tok_per_sec: Some(600.0),
            context_limit_tokens: Some(3072),
            inference_count: Some(1),
            // A cold first turn: whole prompt decoded, nothing reused.
            prefilled_tokens: Some(100 * turn_number),
            reused_prefix_tokens: Some(0),
            // Unmeasured on purpose (no `ProviderStats`); tests needing values set them.
            reasoning_tokens: None,
            reengagements: None,
        }
    }

    #[tokio::test]
    async fn record_and_retrieve_turn() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let telemetry = SqliteTelemetry::new(db.logs).await.unwrap();

        telemetry.record_turn(make_turn("sess-1", 1)).await.unwrap();

        let turns = telemetry.get_turns("sess-1").await.unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].prompt_tokens, 100);
    }

    #[tokio::test]
    async fn tool_metadata_round_trips() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let telemetry = SqliteTelemetry::new(db.logs).await.unwrap();

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

    /// Must reopen the database: reads come from the cache, so only a reopen exercises `load_all`.
    #[tokio::test]
    async fn reasoning_cost_and_re_engagements_survive_sqlite() {
        let tmp = tempdir().unwrap();

        {
            let db = Database::init(tmp.path()).await.unwrap();
            let telemetry = SqliteTelemetry::new(db.logs).await.unwrap();

            let mut measured = make_turn("sess-1", 1);
            measured.reasoning_tokens = Some(0);
            measured.reengagements = Some(2);
            telemetry.record_turn(measured).await.unwrap();

            let mut thought = make_turn("sess-1", 2);
            thought.reasoning_tokens = Some(156);
            thought.reengagements = Some(0);
            telemetry.record_turn(thought).await.unwrap();

            // Unmeasured — `make_turn`'s default, i.e. every historical row.
            telemetry.record_turn(make_turn("sess-1", 3)).await.unwrap();
        }

        let db = Database::init(tmp.path()).await.unwrap();
        let telemetry = SqliteTelemetry::new(db.logs).await.unwrap();
        let turns = telemetry.get_turns("sess-1").await.unwrap();
        assert_eq!(turns.len(), 3, "rows did not survive the reopen at all");

        assert_eq!(
            turns[0].reasoning_tokens,
            Some(0),
            "a measured zero must not come back as NULL"
        );
        assert_eq!(
            turns[2].reasoning_tokens, None,
            "an unmeasured turn must not come back as a measured zero"
        );

        assert_eq!(turns[0].reengagements, Some(2));
        assert_eq!(turns[1].reasoning_tokens, Some(156));
        assert_eq!(turns[1].reengagements, Some(0));
        assert_eq!(turns[2].reengagements, None);
    }

    #[tokio::test]
    async fn telemetry_survives_restart() {
        let tmp = tempdir().unwrap();

        {
            let db = Database::init(tmp.path()).await.unwrap();
            let telemetry = SqliteTelemetry::new(db.logs).await.unwrap();
            telemetry.record_turn(make_turn("sess-1", 1)).await.unwrap();
            telemetry.record_turn(make_turn("sess-1", 2)).await.unwrap();
        }

        let db = Database::init(tmp.path()).await.unwrap();
        let telemetry = SqliteTelemetry::new(db.logs).await.unwrap();

        let turns = telemetry.get_turns("sess-1").await.unwrap();
        assert_eq!(turns.len(), 2);

        let summary = telemetry.get_summary("sess-1").await.unwrap();
        assert_eq!(summary.total_turns, 2);
    }
}
