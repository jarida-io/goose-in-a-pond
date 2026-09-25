//! Driven port: run a child agent under limits set by [`crate::shared::domain::orchestration`].
//! No method has a default body: a plausible default would hide a deleted override.

use crate::shared::domain::orchestration::{TaskRun, TaskSpec};
use anyhow::Result;
use async_trait::async_trait;

/// Driven Port: child agent execution.
#[async_trait]
pub trait Orchestrator: Send + Sync {
    /// Start the run `spec` describes; it alone carries the parent and the child's authority.
    /// Returns once the run has an id, possibly already terminal, else `Running` for polling.
    async fn spawn(&self, spec: TaskSpec) -> Result<TaskRun>;

    /// Current state of a run; an unknown id (e.g. model-hallucinated) is `Ok(None)`, not an error.
    async fn poll(&self, task_id: &str) -> Result<Option<TaskRun>>;

    /// Stop a run. Idempotent: cancelling a finished or unknown run is `Ok`.
    /// Record `Cancelled` by re-checking the token: Goose's child loop returns `Ok` on cancel.
    async fn cancel(&self, task_id: &str) -> Result<()>;

    /// Every run belonging to one parent session, terminal ones included.
    async fn list(&self, parent_session_id: &str) -> Result<Vec<TaskRun>>;

    /// Cancel every child of a parent session, returning how many were stopped.
    /// Whatever ends a parent (cancelled turn, deleted session, shutdown) must call this.
    async fn cancel_children_of(&self, parent_session_id: &str) -> Result<usize>;
}
