//! JSON-file execution history for scheduled tasks, capped per schedule.

use anyhow::Result;
use chrono::Utc;
use pond_core::user_data::domain::schedule::{RunStatus, ScheduleRun};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRuns {
    runs: Vec<ScheduleRun>,
}

pub struct JsonRunHistory {
    path: PathBuf,
    state: Mutex<Vec<ScheduleRun>>,
    max_runs_per_schedule: usize,
}

impl JsonRunHistory {
    pub async fn new(path: PathBuf, max_runs_per_schedule: u32) -> Result<Self> {
        let runs = if path.exists() {
            let json = tokio::fs::read_to_string(&path).await?;
            let persisted: PersistedRuns =
                serde_json::from_str(&json).unwrap_or(PersistedRuns { runs: vec![] });
            persisted.runs
        } else {
            vec![]
        };

        Ok(Self {
            path,
            state: Mutex::new(runs),
            max_runs_per_schedule: max_runs_per_schedule.max(1) as usize,
        })
    }

    /// Record that a scheduled task has started.  Returns the run ID.
    pub async fn record_start(&self, schedule_id: &str) -> String {
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
