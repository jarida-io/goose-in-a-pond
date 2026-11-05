//! SchedulerPort — schedule recurring tasks via cron expressions.

use crate::user_data::domain::schedule::{MissedRunAnswer, Schedule, ScheduleRun, TaskKind};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// Request payload for creating a new scheduled task.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CreateScheduleRequest {
    pub id: String,
    pub label: String,
    /// 6-field cron with leading seconds (`"0 0 8 * * *"`), or a sentinel (`"@event"`, `"@once"`).
    pub cron: String,
    /// Fire once at this instant, then delete. When set, `cron` is a sentinel, never parsed.
    #[serde(default)]
    pub fire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Fire once at `cron`'s next occurrence, then delete. Ignored if `fire_at` is set;
    /// `cron` must still parse (it's read once for the instant, then discarded).
    #[serde(default)]
    pub once: bool,
    /// IANA timezone (e.g. `"Africa/Nairobi"`).
    pub timezone: String,
    /// What to do on each fire.
    pub kind: TaskKind,
    /// Ask before running a fire time missed while the pond was down.
    #[serde(default)]
    pub ask_if_missed: bool,
}

/// Partial update of a scheduled task: only provided fields change.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct UpdateScheduleRequest {
    pub label: Option<String>,
    pub cron: Option<String>,
    pub timezone: Option<String>,
    pub kind: Option<TaskKind>,
    /// `Some` converts to a one-shot at this instant. A `cron` without `fire_at`/`once`
    /// converts a one-shot back to recurring, clearing the stored `fire_at`.
    #[serde(default)]
    pub fire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Convert to a one-shot at `cron`'s next occurrence; ignored if `fire_at` is set.
    #[serde(default)]
    pub once: bool,
    #[serde(default)]
    pub ask_if_missed: Option<bool>,
}

#[async_trait]
pub trait SchedulerPort: Send + Sync {
    async fn create_task(&self, req: CreateScheduleRequest) -> Result<Schedule>;
    async fn list_tasks(&self) -> Result<Vec<Schedule>>;
    async fn delete_task(&self, id: &str) -> Result<()>;
    async fn pause_task(&self, id: &str) -> Result<()>;
    /// Re-arm a paused task: a recurring one at its next occurrence, a one-shot at its
    /// `fire_at`. A one-shot whose `fire_at` passed while paused is refused with
    /// [`OneShotTimePassed`](crate::user_data::domain::schedule::OneShotTimePassed) and stays
    /// paused. Resuming a task that is not paused is a no-op.
    async fn resume_task(&self, id: &str) -> Result<()>;
    async fn run_now(&self, id: &str) -> Result<()>;

    /// Update an existing schedule. Only non-None fields are changed.
    async fn update_task(&self, id: &str, req: UpdateScheduleRequest) -> Result<Schedule>;

    /// Retrieve execution history for a schedule, most recent first.
    async fn get_runs(&self, schedule_id: &str, limit: u32) -> Result<Vec<ScheduleRun>>;

    /// List schedules sorted by next fire time (soonest first).
    async fn list_upcoming(&self, limit: u32) -> Result<Vec<Schedule>>;

    /// Settle the missed run `id` is asking about: run it now or record it skipped. The first
    /// answer wins; a later one is refused with
    /// [`NoMissedRun`](crate::user_data::domain::schedule::NoMissedRun).
    async fn answer_missed_run(&self, id: &str, _answer: MissedRunAnswer) -> Result<()> {
        anyhow::bail!("schedule '{id}': this scheduler does not ask about missed runs")
    }

    /// Inject the real executor once the agent exists (breaks a circular init dependency).
    async fn set_executor(
        &self,
        executor: Arc<dyn crate::user_data::ports::schedule_execution::ScheduleExecutor>,
    ) -> Result<()>;
}

// ── Backward-compatible aliases ────────────────────────────────────────────────
// Remove once all call sites use the new names.

/// Deprecated — use [`CreateScheduleRequest`] instead.
pub type CreateTaskRequest = CreateScheduleRequest;

/// Deprecated — use [`Schedule`] instead.
pub type ScheduledTask = Schedule;
