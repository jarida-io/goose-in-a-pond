//! Port for seeing the inference lane and asking it to run something now.
//!
//! # Why this exists
//!
//! The lane decides which background job may spend inference, and until now it
//! could not explain itself. Every refusal path in the runtime half logs at
//! `trace!` and only a success logs at `debug!`, so from outside the process a
//! job that is eligible and losing the tie-break is indistinguishable from one
//! that is switched off. On a real pond that produced a memory engine which had
//! never completed a pass -- 958 conversations, zero cursors, zero attempts --
//! with nothing anywhere saying why.
//!
//! It also could not be hurried. `select_next` is least-recently-run with
//! declaration order as the tie-break, and a job that has never run skips the
//! interval-floor check entirely, so several never-run jobs form a queue that
//! drains only as fast as each one's OWN poll: 60s, then 5 minutes, then 15.
//! `last_run` is in-process, so the queue restarts from the top on every boot.
//! A household that restarts the app more often than the queue drains never
//! reaches the end of it, and the job declared last is the one that never runs.
//!
//! Both halves of that are answered here: [`snapshot`](LaneControl::snapshot)
//! says what each job is waiting for, and [`wake`](LaneControl::wake) lets a
//! person put one at the front.
//!
//! # Waking is not running
//!
//! `wake` asks a job's own loop to take its next tick immediately and to skip
//! the quiet period, exactly as the Reindex button already does for index
//! maintenance. It does NOT do the work in the caller's task, and it does not
//! bypass the slot. That is deliberate and it is the whole point of the lane:
//! one job at a time. The precedent for the other choice is in the tree and it
//! is the wrong one -- `POST /sessions/retitle` runs its model calls inline in
//! the request handler, taking no slot, so it can decode beside whichever
//! background job is already holding the machine.
//!
//! So `wake` returns promptly and reports only whether there was a loop to
//! wake. What happened is read back from `snapshot`.
//!
//! # No default bodies
//!
//! Both methods are required. A decorator that forgot `wake` and inherited a
//! defaulted `Ok(false)` would give a household a button that reports "nothing
//! here to run" on a pond where the job is running perfectly well.

use crate::user_data::services::consolidation_schedule::SkipReason;
use crate::user_data::services::inference_lane::LaneJob;
use async_trait::async_trait;

/// One job, as the lane currently sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneJobStatus {
    pub job: LaneJob,
    /// Whether a loop for this job exists in THIS process.
    ///
    /// False is a real answer, not a missing one: several jobs are spawned only
    /// when their dependency exists -- extraction needs an embedder, the index
    /// sweep needs an embedder and a vector model, the reviewer needs an
    /// installed orchestrator and a role that parses. A pond without one is not
    /// broken and the panel must be able to say which it is.
    pub present: bool,
    /// Whether the job has asked for the slot at least once in this process.
    ///
    /// Distinct from `present`: a loop that exists but has not reached its
    /// first tick has registered nothing, so the lane cannot yet report its
    /// cadence. Telling the two apart is what stops "no data" reading as "off".
    pub registered: bool,
    /// The job's live enable toggle, as of its last tick.
    pub enabled: bool,
    /// Seconds since it last ran IN THIS PROCESS. `None` means never, which the
    /// lane treats as infinitely starved.
    pub since_last_run_secs: Option<u64>,
    /// Its own minimum spacing, as of its last tick.
    pub interval_floor_secs: u64,
    /// How quiet it wants before it will take the slot, as of its last tick.
    pub idle_threshold_secs: u64,
    /// Why it would not run right now, or `None` if it would.
    ///
    /// The per-job answer, unlike [`LaneSnapshot::idle_reason`], which is the
    /// one reason that explains the most jobs.
    pub blocked_by: Option<SkipReason>,
    /// What has happened to this job since the process started.
    ///
    /// `blocked_by` is an INSTANT and this is the history, and the difference
    /// is the whole reason this exists: a snapshot cannot tell "eligible and
    /// losing the tie-break 1,340 times" from "switched off", and the first is
    /// a defect while the second is a setting.
    pub history: LaneJobHistory,
}

/// One job's counters since boot. All zero for a job that has never ticked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneJobHistory {
    /// Times it took the slot.
    pub granted: u32,
    /// Times the lane rang its scheduled bell because it should have been
    /// running. THE measurement for the nudge: if this stays at zero while
    /// `lost_to_total` climbs, the nudge is not reaching anybody.
    pub nudged: u32,
    /// Times it asked while another job held the slot.
    pub slot_busy: u32,
    /// Times its own gate refused it, by reason, in `SkipReason` order:
    /// disabled, no activity since start, still active, interval floor.
    pub refused: [u32; 4],
    /// Times another job was picked over it, and which job most often.
    pub lost_to_total: u32,
    pub lost_to_most: Option<(LaneJob, u32)>,
}

/// The lane, right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneSnapshot {
    /// Every job in [`LaneJob::ALL`] order, present or not — a job missing from
    /// this list would be a job nobody can see is missing.
    pub jobs: Vec<LaneJobStatus>,
    /// Which job would take the slot if a tick happened this instant.
    pub would_run: Option<LaneJob>,
    /// Why nothing would, when nothing would.
    pub idle_reason: Option<SkipReason>,
    /// Seconds of household quiet the lane is currently reading.
    pub idle_for_secs: u64,
    /// Whether any turn has been served since this process started. False means
    /// every non-exempt job is standing down whatever else is true.
    pub saw_activity_since_start: bool,
    /// Whether a job is holding the slot at this instant.
    pub slot_busy: bool,
    /// WHICH job is holding it, when one is.
    ///
    /// The distinction the watcher exists for. "Something is using the model"
    /// and "the memory engine is using the model" are different sentences to
    /// somebody wondering why the pond is slow, and only the second one tells
    /// them whether to wait or to go and turn something off.
    pub running: Option<LaneJob>,
    /// How long it has held the slot. A pass that has been running for four
    /// minutes and one that started two seconds ago look the same without it.
    pub running_for_secs: Option<u64>,
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

    /// Ask one job to take its next tick now, skipping the quiet period.
    ///
    /// Does not wait for the job to finish and does not bypass the slot.
    async fn wake(&self, job: LaneJob) -> WakeOutcome;
}
