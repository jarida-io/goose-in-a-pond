//! SQLite adapter for [`LaneRunLog`] — the inference lane's durable clock.
//!
//! One row per job, replaced in place. See `0059_lane_job_runs.sql` for why the
//! lane needs this to survive a restart at all.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use pond_core::user_data::ports::lane_run_log::LaneRunLog;
use pond_core::user_data::services::inference_lane::LaneJob;
use sqlx::{Pool, Sqlite};

pub struct SqliteLaneRunLog {
    pool: Pool<Sqlite>,
}

impl SqliteLaneRunLog {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

/// RFC3339, second resolution. The lane's smallest interval floor is measured
/// in minutes, so sub-second precision would be stored and never read.
fn sql_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Accepts both this adapter's RFC3339 and SQLite's own `datetime('now')`
/// spelling, for the same reason `sqlite_suggestion_queue` does: a row written
/// by a future path leaning on a column default must not be unreadable.
fn parse_ts(raw: &str) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(dt.with_timezone(&Utc));
    }
    chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .map(|naive| naive.and_utc())
        .with_context(|| format!("unreadable lane run timestamp: {raw}"))
}

#[async_trait]
impl LaneRunLog for SqliteLaneRunLog {
    async fn load(&self) -> Result<Vec<(LaneJob, DateTime<Utc>)>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT job, last_run_at FROM lane_job_runs")
                .fetch_all(&self.pool)
                .await
                .context("reading the lane run log")?;

        let mut out = Vec::with_capacity(rows.len());
        for (job, raw) in rows {
            // Two separate skips, and they are different failures. An
            // unrecognised job name is expected after a release that removed a
            // job, so it is silent. An unparseable stamp is a corrupt row, so
            // it warns — but neither may stop the pond starting, because the
            // fallback in both cases is the empty clock every release before
            // this one booted with.
            let Some(job) = LaneJob::from_wire(&job) else {
                continue;
            };
            match parse_ts(&raw) {
                Ok(at) => out.push((job, at)),
                Err(e) => tracing::warn!(
                    job = job.as_str(),
                    raw = %raw,
                    error = %e,
                    "discarding an unreadable lane run stamp"
                ),
            }
        }
        Ok(out)
    }

    async fn record(&self, job: LaneJob, at: DateTime<Utc>) -> Result<()> {
        sqlx::query(
            "INSERT INTO lane_job_runs (job, last_run_at) VALUES (?, ?) \
             ON CONFLICT(job) DO UPDATE SET last_run_at = excluded.last_run_at",
        )
        .bind(job.as_str())
        .bind(sql_ts(at))
        .execute(&self.pool)
        .await
        .with_context(|| format!("recording a lane run for {}", job.as_str()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn log() -> SqliteLaneRunLog {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::migrate!("migrations/system")
            .run(&pool)
            .await
            .unwrap();
        SqliteLaneRunLog::new(pool)
    }

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + secs, 0).unwrap()
    }

    #[tokio::test]
    async fn a_pond_that_has_never_run_a_job_loads_an_empty_clock() {
        // The starting state of every existing pond, and it must not be an
        // error: "never" is a reading, not a failure.
        assert!(log().await.load().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_recorded_run_survives_a_reread() {
        let l = log().await;
        l.record(LaneJob::Titling, at(0)).await.unwrap();
        assert_eq!(l.load().await.unwrap(), vec![(LaneJob::Titling, at(0))]);
    }

    #[tokio::test]
    async fn a_second_run_replaces_the_first_rather_than_adding_a_row() {
        // The whole table is "when did this last run". Two rows for one job
        // would make `load` ambiguous and let the older stamp win by map
        // insertion order.
        let l = log().await;
        l.record(LaneJob::Consolidation, at(0)).await.unwrap();
        l.record(LaneJob::Consolidation, at(600)).await.unwrap();
        let loaded = l.load().await.unwrap();
        assert_eq!(loaded, vec![(LaneJob::Consolidation, at(600))]);
    }

    #[tokio::test]
    async fn each_job_keeps_its_own_stamp() {
        let l = log().await;
        l.record(LaneJob::Titling, at(0)).await.unwrap();
        l.record(LaneJob::MemoryExtraction, at(60)).await.unwrap();
        let mut loaded = l.load().await.unwrap();
        loaded.sort_by_key(|(j, _)| *j);
        assert_eq!(
            loaded,
            vec![
                (LaneJob::Titling, at(0)),
                (LaneJob::MemoryExtraction, at(60)),
            ]
        );
    }

    #[tokio::test]
    async fn every_job_round_trips_under_its_wire_name() {
        // The primary key is `as_str()` and the loader is `from_wire`. A job
        // whose two spellings disagree would silently never load its own stamp
        // — it would write a row and read back nothing, which presents as "the
        // durable clock does not work" for that one job only.
        let l = log().await;
        for (i, job) in LaneJob::ALL.iter().enumerate() {
            l.record(*job, at(i as i64)).await.unwrap();
        }
        let loaded = l.load().await.unwrap();
        assert_eq!(loaded.len(), LaneJob::ALL.len());
        for (i, job) in LaneJob::ALL.iter().enumerate() {
            assert!(
                loaded.contains(&(*job, at(i as i64))),
                "{} did not round-trip",
                job.as_str()
            );
        }
    }

    #[tokio::test]
    async fn a_job_this_release_no_longer_has_is_skipped_not_fatal() {
        let l = log().await;
        l.record(LaneJob::Titling, at(0)).await.unwrap();
        sqlx::query("INSERT INTO lane_job_runs (job, last_run_at) VALUES (?, ?)")
            .bind("a_job_from_a_later_release")
            .bind(sql_ts(at(60)))
            .execute(&l.pool)
            .await
            .unwrap();
        // The real row still arrives. Refusing the load here would turn a
        // downgrade into a pond that will not start.
        assert_eq!(l.load().await.unwrap(), vec![(LaneJob::Titling, at(0))]);
    }

    #[tokio::test]
    async fn an_unreadable_stamp_is_dropped_and_the_rest_survive() {
        let l = log().await;
        l.record(LaneJob::Titling, at(0)).await.unwrap();
        sqlx::query("INSERT INTO lane_job_runs (job, last_run_at) VALUES (?, ?)")
            .bind(LaneJob::Consolidation.as_str())
            .bind("not a time")
            .execute(&l.pool)
            .await
            .unwrap();
        let loaded = l.load().await.unwrap();
        assert_eq!(loaded, vec![(LaneJob::Titling, at(0))]);
    }

    #[tokio::test]
    async fn a_stamp_written_by_the_column_default_spelling_is_still_read() {
        // Nothing writes this spelling today. The guard is that the next thing
        // to touch this table — a backfill, a trigger, a hand-run UPDATE —
        // cannot make a row the loader silently discards.
        let l = log().await;
        sqlx::query("INSERT INTO lane_job_runs (job, last_run_at) VALUES (?, ?)")
            .bind(LaneJob::IndexMaintenance.as_str())
            .bind("2026-09-17 10:00:00")
            .execute(&l.pool)
            .await
            .unwrap();
        let loaded = l.load().await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, LaneJob::IndexMaintenance);
        assert_eq!(loaded[0].1.to_rfc3339(), "2026-09-17T10:00:00+00:00");
    }
}
