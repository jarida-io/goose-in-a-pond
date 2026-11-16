//! The idea engine's server side: the minute tick that queues jobs and opens their long tasks,
//! the inline steps that are one model call each, and what follows a job's steps. The long-task
//! runner calls in here for a task whose id marks it as an engine job's.

mod deliverable;
mod fact_sheet;
mod finish;
mod inline;
mod model;
mod notices;
mod open;
mod questions;
mod tick;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::Notify;

use pond_core::context::retrieval_service::{PersonalContextRetrieval, Recollection};
use pond_core::mcp::ports::notification::MemberNotifier;
use pond_core::shared::domain::long_task::{LongTask, PlanStep, StepState};
use pond_core::shared::ports::long_task::LongTaskRepository;
use pond_core::shared::services::goose_thread::GooseThread;
use pond_core::shared::services::long_task_runner::{next_run, NextRun};
use pond_core::user_data::domain::engine_job::{is_engine_task, EngineJob, JobKind, JobState};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::anchors::AnchorRepository;
use pond_core::user_data::ports::captures::CaptureRepository;
use pond_core::user_data::ports::dont_list::DontListRepository;
use pond_core::user_data::ports::engine_jobs::EngineJobRepository;
use pond_core::user_data::ports::fact_sheet::FactSheetRepository;
use pond_core::user_data::ports::interests::InterestRepository;
use pond_core::user_data::ports::member_agents::MemberAgentRepository;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::questions::QuestionRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::services::engine::is_inline_role;
use pond_core::user_data::services::engine::queue::run_order;
use pond_core::user_data::services::fact_sheet::ToolHint;

pub use finish::{at_finish, give_up, instead_of_planning};
pub use inline::{run_inline_step, runs, CANNOT_RUN};
pub use model::ProviderModel;
pub use notices::tell_member;
pub use tick::{run_ticks, tick, TickReport, EVERYDAY_TITLE};

/// The window assumed before any turn has reported one.
const ASSUMED_WINDOW_TOKENS: usize = 32_768;
const CHARS_PER_TOKEN: f64 = 3.6;

/// Why one model call gave no reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStop {
    /// A person's turn took the model: the call is asked again later.
    Cut,
    Failed(String),
}

/// The model as an inline step calls it.
#[async_trait]
pub trait EngineModel: Send + Sync {
    /// One call with no tools, on the inference bus's Task lane.
    async fn complete(&self, system: &str, user: String) -> Result<String, ModelStop>;
}

/// What a member's stored context holds that bears on some words.
#[async_trait]
pub trait Recall: Send + Sync {
    async fn recall(&self, query: &str, profile_id: &str, limit: usize) -> Vec<Recollection>;
}

#[async_trait]
impl Recall for PersonalContextRetrieval {
    async fn recall(&self, query: &str, profile_id: &str, limit: usize) -> Vec<Recollection> {
        PersonalContextRetrieval::recall(
            self,
            query,
            &ProfileScope::Owner(profile_id.to_string()),
            limit,
        )
        .await
    }
}

/// The tools this pond registers, each with what it is for.
pub type ToolList = Arc<dyn Fn() -> Vec<ToolHint> + Send + Sync>;

/// What the engine reads, writes and calls.
#[derive(Clone)]
pub struct EngineDeps {
    pub tasks: Arc<dyn LongTaskRepository>,
    pub jobs: Arc<dyn EngineJobRepository>,
    pub anchors: Arc<dyn AnchorRepository>,
    pub facts: Arc<dyn FactSheetRepository>,
    pub dont: Arc<dyn DontListRepository>,
    pub questions: Arc<dyn QuestionRepository>,
    pub captures: Arc<dyn CaptureRepository>,
    pub agents: Arc<dyn MemberAgentRepository>,
    pub profiles: Arc<dyn ProfileRepository + Send + Sync>,
    pub settings: Arc<dyn SettingsRepository + Send + Sync>,
    pub memories: Arc<dyn MemoryRepository + Send + Sync>,
    pub sessions: Arc<dyn SessionStorage>,
    pub interests: Arc<dyn InterestRepository>,
    pub model: Arc<dyn EngineModel>,
    /// `None` on a pond with no embedder: relevance then falls back to recency.
    pub recall: Option<Arc<dyn Recall>>,
    pub tools: ToolList,
    pub notifier: Arc<dyn MemberNotifier>,
    pub thread: Option<Arc<GooseThread>>,
    /// The long-task runner's bell, rung when a job's task is ready to run.
    pub bell: Option<Arc<Notify>>,
}

impl EngineDeps {
    fn ring(&self) {
        if let Some(bell) = &self.bell {
            bell.notify_one();
        }
    }

    /// The household's timezone and where it is; UTC and nowhere when unset.
    async fn clock_and_place(&self) -> (String, Option<String>) {
        match self.settings.get().await {
            Ok(settings) => {
                let place = settings.weather_location_name.trim().to_string();
                (settings.timezone, (!place.is_empty()).then_some(place))
            }
            Err(_) => (String::new(), None),
        }
    }
}

/// Characters of the model's window, as the engine last reported it.
fn window_chars() -> usize {
    let tokens =
        pond_core::shared::services::context_window::tokens().unwrap_or(ASSUMED_WINDOW_TOKENS);
    (tokens as f64 * CHARS_PER_TOKEN) as usize
}

/// Characters a prompt beside `system` may fill, leaving a quarter of the window for the reply.
fn prompt_budget(system: &str) -> usize {
    (window_chars() / 4 * 3).saturating_sub(system.chars().count())
}

fn is_failed_inline(step: &PlanStep) -> bool {
    step.state() == StepState::Failed && is_inline_role(step.role())
}

/// Whether the engine, not the planner, decides what follows steps that cannot go on: always
/// for a job planned in code, and for any job once an inline step has failed for good.
fn plans_itself(kind: Option<JobKind>, steps: &[PlanStep]) -> bool {
    kind != Some(JobKind::Deliverable) || steps.iter().any(is_failed_inline)
}

/// Whether the next run of an engine task needs no helper. A job not yet started is read with
/// the deadline [`begin`] will give it.
fn needs_no_helper(
    task: &LongTask,
    job: Option<&EngineJob>,
    steps: &[PlanStep],
    now: DateTime<Utc>,
) -> bool {
    let mut probe = task.clone();
    if let Some(job) = job.filter(|job| job.state == JobState::Queued) {
        probe.set_deadline(Some(now + job.kind.time_cap()), now);
    }
    match next_run(&probe, steps, 0, now) {
        NextRun::Do(step) => is_inline_role(step.role()),
        NextRun::Finish | NextRun::GiveUp(_) | NextRun::SkipForTime(_) => true,
        NextRun::Plan(_) => plans_itself(job.map(|job| job.kind), steps),
        NextRun::Wait => false,
    }
}

/// The task the one slot takes next from `open`, and whether its next run needs no helper.
/// With helpers off, the first engine task that can go on without one.
pub async fn next_task(
    deps: &EngineDeps,
    open: &[LongTask],
    helpers_on: bool,
    now: DateTime<Utc>,
) -> (Option<LongTask>, bool) {
    let jobs = deps.jobs.open().await.unwrap_or_default();
    let job_of = |task: &LongTask| {
        jobs.iter()
            .find(|job| job.task_id.as_deref() == Some(task.id()))
    };
    for task in run_order(open, &jobs) {
        let inline = is_engine_task(task.id()) && {
            let steps = deps.tasks.list_steps(task.id()).await.unwrap_or_default();
            needs_no_helper(task, job_of(task), &steps, now)
        };
        if helpers_on || inline {
            return (Some(task.clone()), inline);
        }
    }
    (None, false)
}

/// Starts the clock of a job the runner takes for the first time: its time cap runs from now,
/// not from when its task was opened. Returns the task as it now stands.
pub async fn begin(deps: &EngineDeps, task: LongTask, now: DateTime<Utc>) -> LongTask {
    if !is_engine_task(task.id()) {
        return task;
    }
    let Ok(Some(mut job)) = deps.jobs.by_task(task.id()).await else {
        return task;
    };
    if job.state != JobState::Queued || job.start(Some(task.id()), now).is_err() {
        return task;
    }
    if let Err(e) = deps.jobs.save(&job).await {
        tracing::warn!(job = %job.id, error = %e, "an engine job could not be marked running");
        return task;
    }
    let deadline = now + job.kind.time_cap();
    match deps.tasks.set_deadline(task.id(), Some(deadline)).await {
        Ok(true) => deps
            .tasks
            .get_task(task.id())
            .await
            .ok()
            .flatten()
            .unwrap_or(task),
        _ => task,
    }
}
