//! Where the idea engine's jobs are logged and its open ones are found.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::engine_job::{EngineJob, JobClass, JobKind};

/// The log is the engine's and crosses members; only `recent` and the reads that take a
/// `profile_id` serve one member.
#[async_trait]
pub trait EngineJobRepository: Send + Sync {
    async fn add(&self, job: &EngineJob) -> Result<()>;

    async fn get(&self, id: &str) -> Result<Option<EngineJob>>;

    /// The job a long task runs.
    async fn by_task(&self, task_id: &str) -> Result<Option<EngineJob>>;

    /// Writes every field of a stored job; `false` when there is none by that id.
    async fn save(&self, job: &EngineJob) -> Result<bool>;

    /// Every queued or running job, in the order `engine_job::sort_for_running` gives.
    async fn open(&self) -> Result<Vec<EngineJob>>;

    /// The open jobs of one class, in running order.
    async fn open_by_class(&self, class: JobClass) -> Result<Vec<EngineJob>>;

    /// The one open job that makes or refreshes this deliverable.
    async fn open_for_deliverable(&self, deliverable_id: &str) -> Result<Option<EngineJob>>;

    /// The one open capture or ledger job whose subject is this capture.
    async fn open_for_capture(&self, capture_id: &str) -> Result<Option<EngineJob>>;

    /// The member's open jobs of `kind`; a fact sheet has at most one.
    async fn open_of_kind(&self, profile_id: &str, kind: JobKind) -> Result<Vec<EngineJob>>;

    /// How many open jobs of `class` the member has.
    async fn count_open(&self, profile_id: &str, class: JobClass) -> Result<usize>;

    /// The member's jobs, newest first.
    async fn recent(&self, profile_id: &str, limit: usize) -> Result<Vec<EngineJob>>;

    /// When the member's newest finished job of `kind` finished, whatever it settled as.
    async fn last_finished(&self, profile_id: &str, kind: JobKind)
        -> Result<Option<DateTime<Utc>>>;

    /// Deletes jobs that finished before `before`; returns how many went.
    async fn prune_finished(&self, before: DateTime<Utc>) -> Result<u64>;
}
