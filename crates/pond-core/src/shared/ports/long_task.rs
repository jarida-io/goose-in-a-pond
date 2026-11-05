//! Driven port: the store of long-running tasks, their plan steps and the artifacts steps write.
//! No method has a default body: a plausible default would hide a missing override.

use crate::shared::domain::long_task::{
    AnswerShape, Artifact, ArtifactSummary, LongTask, LongTaskState, PlanStep, ValidatedPlan,
};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Whose tasks a listing returns. Not an `Option<&str>`: `ProfileScope::owner_id()` is `None`
/// for a guest too, and a `None` meaning "everyone" would hand a guest the household's tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongTaskOwnerFilter<'a> {
    /// Every task on the pond; for the runner, never for a member's view.
    Everyone,
    /// Tasks this member owns.
    Member(&'a str),
    /// Tasks nobody owns: started at household or guest scope with no member named.
    Unowned,
}

#[async_trait]
pub trait LongTaskRepository: Send + Sync {
    /// Store a new task; an id already present is an error.
    async fn create_task(&self, task: &LongTask) -> Result<()>;

    /// One task by id; unknown is `Ok(None)`.
    async fn get_task(&self, id: &str) -> Result<Option<LongTask>>;

    /// Tasks for `owner`, newest first, ties by insertion; finished ones only when asked.
    async fn list_tasks(
        &self,
        owner: LongTaskOwnerFilter<'_>,
        include_finished: bool,
    ) -> Result<Vec<LongTask>>;

    /// Move a task through [`LongTask::transition_to`] against its stored state, compare-and-set,
    /// so a concurrent move is an error rather than overwritten. `Ok(None)` if no such task; a
    /// refused move is an `Err` holding a `LongTaskStateRefused`.
    async fn set_task_state(
        &self,
        id: &str,
        state: LongTaskState,
        at: DateTime<Utc>,
    ) -> Result<Option<LongTask>>;

    /// Record the planner's reading of how long the answer should be, only on a task whose answer
    /// is still `Auto`, so a member's choice stands. `Ok(false)` when nothing changed.
    async fn decide_task_answer(&self, id: &str, answer: AnswerShape) -> Result<bool>;

    /// Write a replan atomically: delete every open step (Pending, Failed, Skipped,
    /// AwaitingApproval), keep Completed and Running ones untouched, insert the plan's steps.
    /// Refused, writing nothing, if the task is gone or finished, a kept step has left Completed
    /// or Running, or a new step's key now names one that has entered them.
    async fn replace_open_steps(&self, plan: &ValidatedPlan) -> Result<()>;

    /// A task's steps by ordinal, ties by insertion. An unreadable step is an error, not a gap,
    /// since a missing step changes what every other step waits for.
    async fn list_steps(&self, task_id: &str) -> Result<Vec<PlanStep>>;

    /// Persist a step's run fields (state, attempts, summary, failure, times) if its stored state
    /// may become the new one; the plan fields are written only by `replace_open_steps`.
    async fn update_step(&self, step: &PlanStep) -> Result<()>;

    /// Store a new artifact; its task must exist.
    async fn insert_artifact(&self, artifact: &Artifact) -> Result<()>;

    /// One artifact with its body, only if it belongs to `task_id`: a helper never reads
    /// another task's artifact, and a foreign id looks the same as an unknown one.
    async fn get_artifact(&self, task_id: &str, artifact_id: &str) -> Result<Option<Artifact>>;

    /// A task's artifacts without bodies, oldest first, ties by insertion.
    async fn list_artifacts(&self, task_id: &str) -> Result<Vec<ArtifactSummary>>;

    /// Delete a task with its steps and artifacts; `Ok(false)` if there was none.
    async fn delete_task(&self, id: &str) -> Result<bool>;

    /// Delete one artifact, only if it belongs to `task_id`; `Ok(false)` if there was none.
    async fn delete_artifact(&self, task_id: &str, artifact_id: &str) -> Result<bool>;
}
