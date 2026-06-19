//! SQLite-backed implementation of `TelemetryPort`.
//!
//! Persists `TurnMetrics` to the `turn_metrics` table in `pond_logs.db`
//! (migration 0005) so telemetry survives a restart. Writes go to SQLite
//! first, then to an in-memory cache that mirrors `InMemoryTelemetry` —
//! reads are served from the cache, which is hydrated from SQLite once at
//! construction time.

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
    /// Connects to `pond_logs.db` and hydrates the in-memory cache with
    /// every turn previously persisted, so queries right after a restart
    /// see the full history.
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
             context_utilization_pct, model_name, timestamp \
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
    }
}

#[async_trait::async_trait]
impl TelemetryPort for SqliteTelemetry {
    async fn record_turn(&self, metrics: TurnMetrics) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO turn_metrics \
             (session_id, turn_number, prompt_tokens, completion_tokens, ttft_ms, \
              total_latency_ms, tool_name, tool_latency_ms, tool_cache_hit, \
              context_utilization_pct, model_name, timestamp) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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

    #[tokio::test]
    async fn telemetry_survives_restart() {
        let tmp = tempdir().unwrap();

        {
            let db = Database::init(tmp.path()).await.unwrap();
            let telemetry = SqliteTelemetry::new(db.logs).await.unwrap();
            telemetry.record_turn(make_turn("sess-1", 1)).await.unwrap();
            telemetry.record_turn(make_turn("sess-1", 2)).await.unwrap();
        }

        // Reopen against the same on-disk database — simulates a process restart.
        let db = Database::init(tmp.path()).await.unwrap();
        let telemetry = SqliteTelemetry::new(db.logs).await.unwrap();

        let turns = telemetry.get_turns("sess-1").await.unwrap();
        assert_eq!(turns.len(), 2);

        let summary = telemetry.get_summary("sess-1").await.unwrap();
        assert_eq!(summary.total_turns, 2);
    }
}
