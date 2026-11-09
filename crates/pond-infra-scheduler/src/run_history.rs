//! JSON-file execution history for scheduled tasks, capped per schedule.

use anyhow::Result;
use chrono::{DateTime, Utc};
use pond_core::user_data::domain::schedule::{RunStatus, ScheduleRun};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRuns {
    runs: Vec<ScheduleRun>,
}

/// Why a run that was going when the Pond stopped did not finish, as the history shows it.
pub const INTERRUPTED_BY_RESTART: &str =
    "The Pond restarted while this ran, so it did not finish. Run it again with Run now.";

pub struct JsonRunHistory {
    path: PathBuf,
    state: Mutex<Vec<ScheduleRun>>,
    max_runs_per_schedule: usize,
}

impl JsonRunHistory {
    /// Opens the history, once, as the scheduler starts. A run still marked running belongs to
    /// the process before this one, which stopped (a restart, a crash, a power cut) before the run
    /// could finish; nothing will ever finish it, so it is closed here as failed, saying why.
    /// Without this it read "running" for good, and the day's run was lost without a word.
    pub async fn new(path: PathBuf, max_runs_per_schedule: u32) -> Result<Self> {
        let mut runs = if path.exists() {
            let json = tokio::fs::read_to_string(&path).await?;
            let persisted: PersistedRuns =
                serde_json::from_str(&json).unwrap_or(PersistedRuns { runs: vec![] });
            persisted.runs
        } else {
            vec![]
        };

        let mut interrupted = 0;
        for run in runs.iter_mut().filter(|r| r.status == RunStatus::Running) {
            run.status = RunStatus::Failed;
            run.error = Some(INTERRUPTED_BY_RESTART.to_string());
            interrupted += 1;
            tracing::warn!(
                kind = "schedule_run_interrupted",
                schedule_id = %run.schedule_id,
                started_at = %run.started_at,
                "a scheduled run was going when the Pond stopped; it is recorded as failed"
            );
        }

        let history = Self {
            path,
            state: Mutex::new(runs),
            max_runs_per_schedule: max_runs_per_schedule.max(1) as usize,
        };
        if interrupted > 0 {
            let guard = history.state.lock().await;
            if let Err(e) = history.save_inner(&guard).await {
                tracing::warn!(error = %e, "could not save the interrupted runs; they read failed until the next save");
            }
        }
        Ok(history)
    }

    /// Record that a scheduled task has started.  Returns the run ID.
    pub async fn record_start(&self, schedule_id: &str) -> String {
        self.start(schedule_id, None).await
    }

    /// Record the start of a missed run someone chose to make late, due at `due_at`.
    pub async fn record_late_start(&self, schedule_id: &str, due_at: DateTime<Utc>) -> String {
        self.start(schedule_id, Some(due_at)).await
    }

    /// Record a missed run that did not happen, as one finished entry dated when it was due.
    pub async fn record_skipped(&self, schedule_id: &str, due_at: DateTime<Utc>, why: &str) {
        let run = ScheduleRun {
            id: uuid::Uuid::new_v4().to_string(),
            schedule_id: schedule_id.to_string(),
            status: RunStatus::Skipped,
            result: Some(why.to_string()),
            error: None,
            started_at: due_at,
            finished_at: Some(Utc::now()),
            duration_ms: None,
            caught_up_from: None,
        };
        let mut guard = self.state.lock().await;
        guard.push(run);
        self.prune(&mut guard);
        let _ = self.save_inner(&guard).await;
    }

    async fn start(&self, schedule_id: &str, caught_up_from: Option<DateTime<Utc>>) -> String {
        let run_id = uuid::Uuid::new_v4().to_string();
        let run = ScheduleRun {
            id: run_id.clone(),
            schedule_id: schedule_id.to_string(),
            status: RunStatus::Running,
            result: None,
            error: None,
            started_at: Utc::now(),
            finished_at: None,
            duration_ms: None,
            caught_up_from,
        };

        let mut guard = self.state.lock().await;
        guard.push(run);
        // Best-effort save — don't fail the task if persistence fails.
        let _ = self.save_inner(&guard).await;
        run_id
    }

    /// Record that a run has finished (completed or failed).
    pub async fn record_finish(
        &self,
        run_id: &str,
        status: RunStatus,
        result: Option<String>,
        error: Option<String>,
    ) {
        let mut guard = self.state.lock().await;
        if let Some(run) = guard.iter_mut().find(|r| r.id == run_id) {
            let now = Utc::now();
            let duration_ms = (now - run.started_at).num_milliseconds().max(0) as u64;
            run.status = status;
            run.result = result;
            run.error = error;
            run.finished_at = Some(now);
            run.duration_ms = Some(duration_ms);
        }

        self.prune(&mut guard);

        let _ = self.save_inner(&guard).await;
    }

    /// Get recent runs for a schedule, most recent first.
    pub async fn get_runs(&self, schedule_id: &str, limit: u32) -> Vec<ScheduleRun> {
        let guard = self.state.lock().await;
        let mut runs: Vec<_> = guard
            .iter()
            .filter(|r| r.schedule_id == schedule_id)
            .cloned()
            .collect();
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        runs.truncate(limit as usize);
        runs
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    fn prune(&self, runs: &mut Vec<ScheduleRun>) {
        // Sort newest-first so we can count from the front.
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));

        // Group by schedule_id, keep only the newest MAX_RUNS_PER_SCHEDULE each.
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        runs.retain(|r| {
            let count = counts.entry(r.schedule_id.clone()).or_insert(0);
            *count += 1;
            *count <= self.max_runs_per_schedule
        });
    }

    async fn save_inner(&self, runs: &[ScheduleRun]) -> Result<()> {
        let persisted = PersistedRuns {
            runs: runs.to_vec(),
        };
        let json = serde_json::to_string_pretty(&persisted)?;
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&self.path, json).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_late_run_keeps_when_it_was_due() {
        let tmp = tempfile::tempdir().unwrap();
        let history = JsonRunHistory::new(tmp.path().join("runs.json"), 10)
            .await
            .unwrap();
        let due = Utc::now() - chrono::Duration::hours(3);
        history.record_late_start("morning", due).await;
        history.record_start("morning").await;

        let reopened = JsonRunHistory::new(tmp.path().join("runs.json"), 10)
            .await
            .unwrap();
        let runs = reopened.get_runs("morning", 10).await;
        let late: Vec<_> = runs.iter().filter_map(|r| r.caught_up_from).collect();
        assert_eq!(late, [due]);
    }

    #[tokio::test]
    async fn a_run_going_when_the_pond_stopped_is_closed_as_failed_on_the_next_start() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("runs.json");
        let before = JsonRunHistory::new(path.clone(), 10).await.unwrap();
        let finished = before.record_start("brief").await;
        before
            .record_finish(&finished, RunStatus::Completed, Some("done".into()), None)
            .await;
        before.record_start("brief").await;
        drop(before);

        let after = JsonRunHistory::new(path.clone(), 10).await.unwrap();
        let runs = after.get_runs("brief", 10).await;
        assert!(runs.iter().all(|r| r.status != RunStatus::Running));
        let interrupted: Vec<_> = runs
            .iter()
            .filter(|r| r.error.as_deref() == Some(INTERRUPTED_BY_RESTART))
            .collect();
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].status, RunStatus::Failed);
        assert_eq!(
            runs.iter()
                .filter(|r| r.status == RunStatus::Completed)
                .count(),
            1,
            "a finished run is left as it was"
        );

        // Saved, so a third start finds nothing left to close.
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("\"running\""), "{saved}");
    }

    #[tokio::test]
    async fn a_skipped_run_is_dated_when_it_was_due_and_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let history = JsonRunHistory::new(tmp.path().join("runs.json"), 10)
            .await
            .unwrap();
        let due = Utc::now() - chrono::Duration::hours(3);
        history
            .record_skipped("morning", due, "Skipped by a household member")
            .await;

        let runs = history.get_runs("morning", 10).await;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Skipped);
        assert_eq!(runs[0].started_at, due);
        assert_eq!(
            runs[0].result.as_deref(),
            Some("Skipped by a household member")
        );
    }
}
