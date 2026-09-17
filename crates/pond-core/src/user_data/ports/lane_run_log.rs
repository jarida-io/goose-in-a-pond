//! Port for the inference lane's durable clock.
//!
//! # What it is for
//!
//! The lane's scheduler is entirely driven by one number per job: how long
//! since that job last ran. `select_next` ranks by it, and
//! `consolidation_schedule::should_run` gates on it against the job's interval
//! floor. Until this port existed that number lived only in the runner's
//! process memory, so a restart set every job back to "never" — which both
//! ranks highest and skips the floor. Migration `0059_lane_job_runs.sql`
//! carries the reasoning at length.
//!
//! # The two methods are not symmetric, on purpose
//!
//! [`load`](LaneRunLog::load) is called once, during wiring, and its result
//! seeds the runner. [`record`](LaneRunLog::record) is called from a drain task
//! fed by the slot guard's `Drop`, which is synchronous and cannot await — so
//! the write is necessarily behind a channel and necessarily after the fact.
//! That is why `record` takes the moment as an argument rather than reading a
//! clock: the stamp must be the instant the slot was released, not the instant
//! the writer got round to it.
//!
//! # A failed write is not a failed run
//!
//! Neither method may be load-bearing for a pass. If `load` fails the pond
//! starts with an empty clock, which is exactly the behaviour every release
//! before this one had. If `record` fails the in-memory clock still advanced,
//! so the lane schedules correctly for the life of the process and only a
//! restart loses the stamp. Both are degradations back to the old behaviour,
//! and a lane that refused to run because it could not write a log row would be
//! a worse pond than one that forgets.

use crate::user_data::services::inference_lane::LaneJob;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// No default bodies, for the reason every port in this module says: a
/// decorator that forgot `record` and inherited `Ok(())` would make every run
/// report itself logged and store nothing, and the symptom — a lane that
/// forgets across restarts — is the exact thing this port was added to fix, so
/// it would read as "the feature does not work" rather than as a bug.
#[async_trait]
pub trait LaneRunLog: Send + Sync {
    /// Every job's last run, as of now. Called once, at wiring time.
    ///
    /// Rows whose `job` no longer names a [`LaneJob`] are skipped rather than
    /// refused: a release that deletes a job must not be a release that will
    /// not start.
    async fn load(&self) -> Result<Vec<(LaneJob, DateTime<Utc>)>>;

    /// Stamp one job's run. Replaces any previous stamp for that job.
    ///
    /// `at` is the moment the slot was released, passed in rather than read
    /// here — see this module's docs.
    async fn record(&self, job: LaneJob, at: DateTime<Utc>) -> Result<()>;
}
