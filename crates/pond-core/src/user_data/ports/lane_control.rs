//! Port for seeing the inference lane and asking it to run something now.
//!
//! No default bodies: a decorator inheriting a default `wake` would call a running job absent.

use crate::user_data::services::consolidation_schedule::SkipReason;
use crate::user_data::services::inference_lane::LaneJob;
use async_trait::async_trait;

/// One job, as the lane currently sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneJobStatus {
    pub job: LaneJob,
    /// Whether this process spawned a loop for the job; some spawn only when a dependency exists.
    pub present: bool,
    /// Whether the job has asked for the slot yet; until then its cadence fields are unknown.
    pub registered: bool,
    /// The job's live enable toggle, as of its last tick.
    pub enabled: bool,
    /// Seconds since it last ran, across restarts; `None` = never, treated as infinitely starved.
    pub since_last_run_secs: Option<u64>,
    /// Its own minimum spacing, as of its last tick.
    pub interval_floor_secs: u64,
    /// How quiet it wants before it will take the slot, as of its last tick.
    pub idle_threshold_secs: u64,
    /// Why it would not run right now, or `None` if it would.
    pub blocked_by: Option<SkipReason>,
    /// Since-boot history; unlike the instant `blocked_by`, it tells "always losing" from "off".
    pub history: LaneJobHistory,
}

/// One job's counters since boot. All zero for a job that has never ticked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneJobHistory {
    /// Times it took the slot.
    pub granted: u32,
    /// Times the lane rang its scheduled bell because it won a tick another job asked for.
    pub nudged: u32,
    /// Times it asked while another job held the slot.
    pub slot_busy: u32,
    /// Refusals by its own gate, indexed in `SkipReason` declaration order.
    pub refused: [u32; 4],
    /// Times another job was picked over it, and which job most often.
    pub lost_to_total: u32,
    pub lost_to_most: Option<(LaneJob, u32)>,
}

/// The lane, right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneSnapshot {
    /// Every job in [`LaneJob::ALL`] order, present or not, so an absent job stays visible.
    pub jobs: Vec<LaneJobStatus>,
    /// Which job would take the slot if a tick happened this instant.
    pub would_run: Option<LaneJob>,
    /// Why nothing would run, when nothing would: the reason covering the most jobs.
    pub idle_reason: Option<SkipReason>,
    /// Seconds of household quiet the lane is currently reading.
    pub idle_for_secs: u64,
    /// Whether any turn was served since start; while false every non-exempt job stands down.
    pub saw_activity_since_start: bool,
    /// Whether a job is holding the slot at this instant.
    pub slot_busy: bool,
    /// Which job holds the slot, when one does.
    pub running: Option<LaneJob>,
    /// How long `running` has held the slot.
    pub running_for_secs: Option<u64>,
    /// How long the embedding slot has been embedding, while it is; it runs beside the lane.
    pub embedding_slot_running_for_secs: Option<u64>,
}

/// What asking for a job to run now did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeOutcome {
    /// Its loop was asked to take its next tick immediately.
    Woken,
    /// No loop for this job in this process, so there was nothing to ask.
    NotPresent,
}

#[async_trait]
pub trait LaneControl: Send + Sync {
    /// What every job is doing and waiting for.
    async fn snapshot(&self) -> LaneSnapshot;

    /// Ask one job to tick now, skipping the quiet period; neither waits nor bypasses the slot.
    async fn wake(&self, job: LaneJob) -> WakeOutcome;
}
