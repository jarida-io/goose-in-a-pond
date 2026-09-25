//! Runtime half of the inference lane: job registry and the single slot. Each background
//! loop asks the lane; one job holds [`LaneSlot`] at a time, and least-recently-run wins.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pond_core::user_data::services::inference_lane::{
    self, JobState, LaneDecision, LaneInputs, LaneJob,
};

/// Proof of owning the inference slot; a drop releases it, so early returns can't wedge it.
pub struct LaneSlot<'a> {
    _guard: tokio::sync::MutexGuard<'a, ()>,
    lane: &'a InferenceLane,
    job: LaneJob,
}

/// Dropping also records the run, whatever the outcome: a job with no recorded run counts
/// as infinitely starved in `select_next` and would win every tie forever.
impl Drop for LaneSlot<'_> {
    fn drop(&mut self) {
        // Lock order is safe: `acquire` releases `last_run` before it takes the slot.
        match self.lane.last_run.lock() {
            Ok(mut last_run) => {
                last_run.insert(self.job, Instant::now());
            }
            // Recover a poisoned map: panicking inside a drop would abort the process.
            Err(poisoned) => {
                poisoned.into_inner().insert(self.job, Instant::now());
            }
        }
    }
}

/// What a job currently wants, refreshed on each of its ticks so others' decisions see it.
#[derive(Debug, Clone, Copy)]
struct Registration {
    enabled: bool,
    interval_floor: Duration,
    /// This job's own quiet requirement — see `JobState::idle_threshold`.
    idle_threshold: Duration,
    /// May run before the pond has served a turn; see `JobState::exempt_from_activity_gate`.
    exempt_from_activity_gate: bool,
}

/// The shared inference slot and the registry of what wants it.
pub struct InferenceLane {
    slot: tokio::sync::Mutex<()>,
    /// Std, so [`LaneSlot`]'s `Drop` can lock it; never held across an await.
    last_run: std::sync::Mutex<HashMap<LaneJob, Instant>>,
    registry: tokio::sync::Mutex<HashMap<LaneJob, Registration>>,
}

impl InferenceLane {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            slot: tokio::sync::Mutex::new(()),
            last_run: std::sync::Mutex::new(HashMap::new()),
            registry: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    /// The slot for `job` if it wins this tick, else `None`. Uses `try_lock`, not a wait: a
    /// queued job would run after the idle conditions that qualified it had passed.
    pub async fn acquire(
        &self,
        job: LaneJob,
        enabled: bool,
        interval_floor: Duration,
        saw_activity_since_start: bool,
        idle_for: Duration,
        idle_threshold: Duration,
        // Per job, not lane-wide: relaxing the lane-wide gate would qualify every job at once.
        exempt_from_activity_gate: bool,
    ) -> Option<LaneSlot<'_>> {
        {
            let mut registry = self.registry.lock().await;
            registry.insert(
                job,
                Registration {
                    enabled,
                    interval_floor,
                    idle_threshold,
                    exempt_from_activity_gate,
                },
            );
        }

        let decision = {
            let registry = self.registry.lock().await;
            let last_run = self
                .last_run
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let mut states: Vec<JobState> = registry
                .iter()
                .map(|(&j, reg)| JobState {
                    job: j,
                    enabled: reg.enabled,
                    since_last_run: last_run.get(&j).map(|t| now.duration_since(*t)),
                    interval_floor: reg.interval_floor,
                    idle_threshold: reg.idle_threshold,
                    exempt_from_activity_gate: reg.exempt_from_activity_gate,
                })
                .collect();

            // Ties break by position, so undo HashMap order; `LaneJob: Ord` is declaration order.
            states.sort_unstable_by_key(|s| s.job);

            inference_lane::select_next(LaneInputs {
                saw_activity_since_start,
                idle_for,
                jobs: &states,
            })
        };

        match decision {
            LaneDecision::Run(winner) if winner == job => {}
            LaneDecision::Run(winner) => {
                tracing::trace!(
                    asked = job.as_str(),
                    running = winner.as_str(),
                    "inference lane: another job is more starved"
                );
                return None;
            }
            LaneDecision::Idle(reason) => {
                tracing::trace!(
                    asked = job.as_str(),
                    reason = reason.as_str(),
                    "inference lane: no job may run"
                );
                return None;
            }
        }

        match self.slot.try_lock() {
            Ok(guard) => {
                tracing::debug!(job = job.as_str(), "inference lane: slot acquired");
                Some(LaneSlot {
                    _guard: guard,
                    lane: self,
                    job,
                })
            }
            Err(_) => {
                tracing::trace!(
                    asked = job.as_str(),
                    "inference lane: slot busy, standing down until the next tick"
                );
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE_THRESHOLD: Duration = Duration::from_secs(900);
    const LONG_IDLE: Duration = Duration::from_secs(3600);

    /// Not exempt: these tests cover the shared gate and tie-break, which exempt jobs skip.
    async fn ask(lane: &InferenceLane, job: LaneJob) -> Option<LaneSlot<'_>> {
        lane.acquire(
            job,
            true,
            Duration::ZERO,
            true,
            LONG_IDLE,
            IDLE_THRESHOLD,
            false,
        )
        .await
    }

    #[tokio::test]
    async fn a_second_job_cannot_take_a_held_slot() {
        let lane = InferenceLane::new();
        let held = ask(&lane, LaneJob::Consolidation)
            .await
            .expect("first caller should win an empty lane");

        assert!(
            ask(&lane, LaneJob::Titling).await.is_none(),
            "titling took the slot while consolidation held it"
        );

        drop(held);
    }

    #[tokio::test]
    async fn releasing_the_slot_lets_the_next_job_in() {
        let lane = InferenceLane::new();
        drop(
            ask(&lane, LaneJob::Consolidation)
                .await
                .expect("first caller wins"),
        );

        assert!(
            ask(&lane, LaneJob::Titling).await.is_some(),
            "the slot was not released"
        );
    }

    #[tokio::test]
    async fn a_dropped_slot_does_not_wedge_the_lane() {
        // Same job on purpose: another could just lose the tie, proving nothing about the slot.
        let lane = InferenceLane::new();
        {
            let _slot = ask(&lane, LaneJob::Consolidation).await.expect("wins");
        }
        assert!(
            ask(&lane, LaneJob::Consolidation).await.is_some(),
            "dropping a slot without finish() left the lane wedged"
        );
    }

    #[tokio::test]
    async fn a_job_that_bails_early_still_spends_its_budget() {
        // The bailer must be the earlier-declared job, or declaration-order ties hide the bug.
        let lane = InferenceLane::new();
        {
            let _slot = ask(&lane, LaneJob::Consolidation)
                .await
                .expect("wins an empty lane");
            // ...body bails here. No bookkeeping call, just a drop.
        }

        assert!(
            ask(&lane, LaneJob::Titling).await.is_some(),
            "consolidation bailed early and kept its never-run status, so it \
             out-starves every job that HAS run and wins every tie forever — \
             titling can no longer take the slot at all"
        );
    }

    #[tokio::test]
    async fn the_shared_gate_refuses_everyone_mid_conversation() {
        let lane = InferenceLane::new();
        let slot = lane
            .acquire(
                LaneJob::Consolidation,
                true,
                Duration::ZERO,
                true,
                Duration::from_secs(5), // user active 5s ago
                IDLE_THRESHOLD,
                false, // not exempt — the gate is the subject
            )
            .await;
        assert!(slot.is_none());
    }

    #[tokio::test]
    async fn a_job_registers_even_when_it_loses_so_others_can_see_it() {
        // Titling wins here (empty lane); what matters is its registration persists.
        let lane = InferenceLane::new();
        drop(ask(&lane, LaneJob::Titling).await.expect("wins"));

        let registry = lane.registry.lock().await;
        assert!(registry.contains_key(&LaneJob::Titling));
    }
}
