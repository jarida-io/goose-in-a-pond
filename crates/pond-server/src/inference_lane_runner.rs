//! Runtime half of the inference lane: job registry and the single slot. Each background
//! loop asks the lane; one job holds [`LaneSlot`] at a time, and least-recently-run wins.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pond_core::user_data::ports::lane_control::{
    LaneJobHistory, LaneJobStatus, LaneSnapshot, WakeOutcome,
};
use pond_core::user_data::services::consolidation_schedule;
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
        let at = Utc::now();
        match self.lane.last_run.lock() {
            Ok(mut last_run) => {
                last_run.insert(self.job, at);
            }
            // Recover a poisoned map: panicking inside a drop would abort the process.
            Err(poisoned) => {
                poisoned.into_inner().insert(self.job, at);
            }
        }

        // Persisted via an unbounded send, as `drop` can't await or assume a runtime to spawn on.
        // Unchecked: a closed channel means shutdown, and loses only this stamp.
        if let Some(runs) = &self.lane.runs {
            let _ = runs.send((self.job, at));
        }

        // Cleared in Drop (panics too), unconditionally: a stale holder is worse than none.
        match self.lane.holder.lock() {
            Ok(mut holder) => *holder = None,
            Err(poisoned) => *poisoned.into_inner() = None,
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

/// A job's doorbells, made for every job so `wake` can tell "no loop here" from "unknown job".
struct Wake {
    /// A Run-now press: waives the quiet period, interval floor and activity gate.
    by_hand: Arc<tokio::sync::Notify>,
    /// The lane's nudge; kept apart from `by_hand` so a nudge never waives the gates.
    scheduled: Arc<tokio::sync::Notify>,
    present: std::sync::atomic::AtomicBool,
}

/// The last activity reading a job handed the lane, which owns no activity clock of its own.
#[derive(Clone, Copy)]
struct Observation {
    at: Instant,
    saw_activity_since_start: bool,
    idle_for: Duration,
}

/// Per-job counts since boot, instead of logging every refusal to the eMMC. Kept outside
/// `Registration`, which `acquire` overwrites every tick.
#[derive(Debug, Clone, Default)]
struct Tally {
    /// Times this job took the slot.
    granted: u32,
    /// Times the lane rang this job's scheduled bell.
    nudged: u32,
    /// Times it asked while another job held the slot.
    slot_busy: u32,
    /// Times its own gate refused it, by reason.
    disabled: u32,
    no_activity_since_start: u32,
    still_active: u32,
    interval_floor: u32,
    /// Who beat it, and how often.
    lost_to: std::collections::BTreeMap<LaneJob, u32>,
}

/// The shared inference slot and the registry of what wants it.
pub struct InferenceLane {
    slot: tokio::sync::Mutex<()>,
    /// Std, so [`LaneSlot`]'s `Drop` can lock it; never held across an await. Wall clock, not
    /// `Instant`: seeded from `lane_job_runs`, and `Instant::now() - age` can panic after boot.
    last_run: std::sync::Mutex<HashMap<LaneJob, DateTime<Utc>>>,
    /// Run log sink; `None` in tests and when the log failed to open.
    runs: Option<tokio::sync::mpsc::UnboundedSender<(LaneJob, DateTime<Utc>)>>,
    /// Which job holds the slot, and since when. Std, never held across an await; set on taking
    /// the slot and cleared in `LaneSlot`'s `Drop`, so it can't drift from the guard.
    holder: std::sync::Mutex<Option<(LaneJob, Instant)>>,
    registry: tokio::sync::Mutex<HashMap<LaneJob, Registration>>,
    /// Fixed at construction, so no lock; only each `present` flag changes, atomically.
    wake: HashMap<LaneJob, Wake>,
    /// Std, held only for a copy; written on every `acquire`, read by the status snapshot.
    observed: std::sync::Mutex<Option<Observation>>,
    /// What has happened to each job since boot. See [`Tally`].
    tallies: std::sync::Mutex<HashMap<LaneJob, Tally>>,
    /// Whether a turn is being answered; see [`InferenceLane::watch_turns`]. Unset in tests and
    /// before the API state exists, when no turn can be running.
    turns: std::sync::OnceLock<TurnProbe>,
}

type TurnProbe = Box<dyn Fn() -> bool + Send + Sync>;

/// Saturating elapsed time between wall-clock stamps. A future `then` (NTP step, RTC-less boot)
/// reads as zero, holding the job behind its floor rather than firing every job at once.
fn elapsed_since(now: DateTime<Utc>, then: DateTime<Utc>) -> Duration {
    now.signed_duration_since(then)
        .to_std()
        .unwrap_or(Duration::ZERO)
}

impl InferenceLane {
    /// A lane with no memory of previous processes and nowhere to write.
    #[cfg(test)]
    pub fn new() -> Arc<Self> {
        Self::restored(HashMap::new(), None)
    }

    /// Seeded from the durable log and writing new runs back; the caller does the fallible load.
    pub fn restored(
        last_run: HashMap<LaneJob, DateTime<Utc>>,
        runs: Option<tokio::sync::mpsc::UnboundedSender<(LaneJob, DateTime<Utc>)>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            slot: tokio::sync::Mutex::new(()),
            last_run: std::sync::Mutex::new(last_run),
            runs,
            holder: std::sync::Mutex::new(None),
            registry: tokio::sync::Mutex::new(HashMap::new()),
            wake: LaneJob::ALL
                .iter()
                .map(|&job| {
                    (
                        job,
                        Wake {
                            by_hand: Arc::new(tokio::sync::Notify::new()),
                            scheduled: Arc::new(tokio::sync::Notify::new()),
                            present: std::sync::atomic::AtomicBool::new(false),
                        },
                    )
                })
                .collect(),
            observed: std::sync::Mutex::new(None),
            tallies: std::sync::Mutex::new(HashMap::new()),
            turns: std::sync::OnceLock::new(),
        })
    }

    /// Lets the lane see a turn in flight. Jobs measure quiet from activity, which is stamped
    /// when a turn starts and again when it ends, so a turn answering for longer than a job's
    /// quiet threshold read as quiet in between: on the Orin the summary refresh started two
    /// minutes into a turn that was compacting, and slowed it. Set once; later calls are ignored.
    pub fn watch_turns(&self, turn_running: impl Fn() -> bool + Send + Sync + 'static) {
        let _ = self.turns.set(Box::new(turn_running));
    }

    fn turn_running(&self) -> bool {
        self.turns.get().is_some_and(|running| running())
    }

    fn tally(&self, job: LaneJob, f: impl FnOnce(&mut Tally)) {
        let mut tallies = self
            .tallies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(tallies.entry(job).or_default());
    }

    /// Takes `job`'s bells and marks its loop present; call once at spawn, never per tick.
    pub fn claim(&self, job: LaneJob) -> Bells {
        let slot = self
            .wake
            .get(&job)
            .expect("every LaneJob has a doorbell: the map is built from LaneJob::ALL");
        slot.present
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Bells {
            by_hand: slot.by_hand.clone(),
            scheduled: slot.scheduled.clone(),
        }
    }

    /// The slot for `job` if it wins this tick, else `None`. Uses `try_lock`, not a wait: a
    /// queued job would run after the idle conditions that qualified it had passed.
    pub async fn acquire(
        &self,
        job: LaneJob,
        enabled: bool,
        // The STANDING cadence, never a waived one: it lands in the shared registry.
        cadence: Cadence,
        // Hand-asked tick: applies to this job's own gate only, never the registry.
        waive: bool,
        saw_activity_since_start: bool,
        idle_for: Duration,
    ) -> Option<LaneSlot<'_>> {
        // A turn in flight is no quiet at all, however long ago its request arrived; a hand-asked
        // tick still waives the quiet period, as it always has.
        let idle_for = if self.turn_running() {
            Duration::ZERO
        } else {
            idle_for
        };

        {
            let mut registry = self.registry.lock().await;
            registry.insert(
                job,
                Registration {
                    enabled,
                    interval_floor: cadence.interval_floor,
                    idle_threshold: cadence.idle_threshold,
                    exempt_from_activity_gate: cadence.exempt_from_activity_gate,
                },
            );
        }

        // Recorded first, so a tick that refuses every job still updates the snapshot.
        if let Ok(mut observed) = self.observed.lock() {
            *observed = Some(Observation {
                at: Instant::now(),
                saw_activity_since_start,
                idle_for,
            });
        }

        let (decision, own_verdict) = {
            let registry = self.registry.lock().await;
            let last_run = self
                .last_run
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Utc::now();
            let mut states: Vec<JobState> = registry
                .iter()
                .map(|(&j, reg)| JobState {
                    job: j,
                    enabled: reg.enabled,
                    since_last_run: last_run.get(&j).map(|t| elapsed_since(now, *t)),
                    interval_floor: reg.interval_floor,
                    idle_threshold: reg.idle_threshold,
                    exempt_from_activity_gate: reg.exempt_from_activity_gate,
                })
                .collect();

            // Ties break by position, so undo HashMap order; `LaneJob: Ord` is declaration order.
            states.sort_unstable_by_key(|s| s.job);

            // Waiver on the asker's copy only, so it never reaches another job's tick.
            if waive {
                if let Some(asker) = states.iter_mut().find(|s| s.job == job) {
                    asker.interval_floor = Duration::ZERO;
                    asker.idle_threshold = Duration::ZERO;
                    asker.exempt_from_activity_gate = true;
                }
            }

            // The asker's own verdict; `select_next` gives the reason that covers most jobs.
            let mine = states.iter().find(|s| s.job == job).map(|s| {
                consolidation_schedule::should_run(consolidation_schedule::GateInputs {
                    enabled: s.enabled,
                    saw_activity_since_start: saw_activity_since_start
                        || s.exempt_from_activity_gate,
                    idle_for,
                    idle_threshold: s.idle_threshold,
                    since_last_run: s.since_last_run,
                    interval_floor: s.interval_floor,
                })
            });

            let decision = inference_lane::select_next(LaneInputs {
                saw_activity_since_start,
                idle_for,
                jobs: &states,
                // Ties go to the asker, not the first-declared job asleep.
                asking: Some(job),
            });
            (decision, mine)
        };

        if let Some(consolidation_schedule::GateDecision::Skip(reason)) = own_verdict {
            self.tally(job, |t| match reason {
                consolidation_schedule::SkipReason::Disabled => t.disabled += 1,
                consolidation_schedule::SkipReason::NoActivitySinceStart => {
                    t.no_activity_since_start += 1
                }
                consolidation_schedule::SkipReason::StillActive => t.still_active += 1,
                consolidation_schedule::SkipReason::IntervalFloor => t.interval_floor += 1,
            });
        }

        match decision {
            LaneDecision::Run(winner) if winner == job => {}
            LaneDecision::Run(winner) => {
                // Wake the winner, which may be asleep on a long poll. Its scheduled bell buys
                // an early look only: `Tick::Scheduled` applies every gate.
                if let Some(slot) = self.wake.get(&winner) {
                    slot.scheduled.notify_one();
                    self.tally(winner, |t| t.nudged += 1);
                }
                self.tally(job, |t| *t.lost_to.entry(winner).or_insert(0) += 1);
                tracing::trace!(
                    asked = job.as_str(),
                    running = winner.as_str(),
                    "inference lane: another job is more starved -- nudged it"
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
                // Info, not debug: ~26 lines a day, and `debug` never reaches the shipped log.
                tracing::info!(job = job.as_str(), "inference lane: slot acquired");
                self.tally(job, |t| t.granted += 1);
                // Set here, not by the caller, so the holder always matches the guard.
                if let Ok(mut holder) = self.holder.lock() {
                    *holder = Some((job, Instant::now()));
                }
                Some(LaneSlot {
                    _guard: guard,
                    lane: self,
                    job,
                })
            }
            Err(_) => {
                self.tally(job, |t| t.slot_busy += 1);
                tracing::trace!(
                    asked = job.as_str(),
                    "inference lane: slot busy, standing down until the next tick"
                );
                None
            }
        }
    }
}

/// A job's standing gates, as the shared registry holds them; a Run-now waiver never lands here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    pub interval_floor: Duration,
    pub idle_threshold: Duration,
    pub exempt_from_activity_gate: bool,
}

impl Cadence {
    pub fn new(
        interval_floor: Duration,
        idle_threshold: Duration,
        exempt_from_activity_gate: bool,
    ) -> Self {
        Self {
            interval_floor,
            idle_threshold,
            exempt_from_activity_gate,
        }
    }
}

/// Both of a job's bells, handed over by [`InferenceLane::claim`].
pub struct Bells {
    by_hand: Arc<tokio::sync::Notify>,
    scheduled: Arc<tokio::sync::Notify>,
}

impl Bells {
    /// The bell a person rings, for callers that need it alone (the Reindex button).
    pub fn hand_bell(&self) -> Arc<tokio::sync::Notify> {
        self.by_hand.clone()
    }

    /// A nudge that keeps every gate: for work that arrived and should run once it is polite to.
    pub fn nudge_bell(&self) -> Arc<tokio::sync::Notify> {
        self.scheduled.clone()
    }

    /// Which bell rang; for the sweep, whose own `select!` can't use `wait_for_tick`.
    pub async fn rang(&self) -> Tick {
        tokio::select! {
            _ = self.by_hand.notified() => Tick::HandAsked,
            _ = self.scheduled.notified() => Tick::Scheduled,
        }
    }
}

/// What woke this tick, which decides which gates apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    /// The job's own timer. Every gate applies.
    Poll,
    /// Run now: quiet, floor and activity gate are waived, never the slot.
    HandAsked,
    /// The lane's nudge that this job should run next; every gate applies, as on `Poll`.
    Scheduled,
}

impl Tick {
    /// Does this tick waive the politeness gates?
    pub fn waives(self) -> bool {
        matches!(self, Tick::HandAsked)
    }
}

/// Every lane loop waits here, not on a bare `sleep`, so a bell can cut its poll short.
pub async fn wait_for_tick(poll: Duration, bells: &Bells) -> Tick {
    tokio::select! {
        _ = tokio::time::sleep(poll) => Tick::Poll,
        _ = bells.by_hand.notified() => Tick::HandAsked,
        _ = bells.scheduled.notified() => Tick::Scheduled,
    }
}

/// Snapshot freshness: `present` is exact; per-job fields are as of that job's last tick;
/// `idle_for` and `saw_activity_since_start` come from any job's last tick, extrapolated.
#[async_trait::async_trait]
impl pond_core::user_data::ports::lane_control::LaneControl for InferenceLane {
    async fn snapshot(&self) -> LaneSnapshot {
        use pond_core::user_data::services::consolidation_schedule::{self as sched, GateInputs};

        // No tick yet: nothing has observed the household, so no activity and zero idle.
        let observed = self.observed.lock().ok().and_then(|o| *o);
        let (saw_activity_since_start, idle_for) = match observed {
            // Extrapolated; if somebody came back, the next tick corrects it.
            Some(o) => (o.saw_activity_since_start, o.idle_for + o.at.elapsed()),
            None => (false, Duration::ZERO),
        };

        let tallies = self
            .tallies
            .lock()
            .map(|t| t.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone());

        let running: Option<(LaneJob, Instant)> = self
            .holder
            .lock()
            .map(|h| *h)
            .unwrap_or_else(|poisoned| *poisoned.into_inner());

        let registry = self.registry.lock().await;
        let last_run = self
            .last_run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Utc::now();

        // Registered jobs only: an unticked job has no cadence, and a default would invent one.
        let mut states: Vec<JobState> = registry
            .iter()
            .map(|(&job, reg)| JobState {
                job,
                enabled: reg.enabled,
                since_last_run: last_run.get(&job).map(|t| elapsed_since(now, *t)),
                interval_floor: reg.interval_floor,
                idle_threshold: reg.idle_threshold,
                exempt_from_activity_gate: reg.exempt_from_activity_gate,
            })
            .collect();
        states.sort_unstable_by_key(|s| s.job);

        let decision = inference_lane::select_next(LaneInputs {
            saw_activity_since_start,
            idle_for,
            jobs: &states,
            // A read, not an ask: naming a job would bias `would_run` toward it.
            asking: None,
        });

        // Every job in ALL order, present or not, so a missing loop is visible.
        let jobs = LaneJob::ALL
            .iter()
            .map(|&job| {
                let present = self
                    .wake
                    .get(&job)
                    .is_some_and(|w| w.present.load(std::sync::atomic::Ordering::Relaxed));
                let state = states.iter().find(|s| s.job == job);
                let blocked_by = state.and_then(|s| {
                    match sched::should_run(GateInputs {
                        enabled: s.enabled,
                        saw_activity_since_start: saw_activity_since_start
                            || s.exempt_from_activity_gate,
                        idle_for,
                        idle_threshold: s.idle_threshold,
                        since_last_run: s.since_last_run,
                        interval_floor: s.interval_floor,
                    }) {
                        sched::GateDecision::Run => None,
                        sched::GateDecision::Skip(reason) => Some(reason),
                    }
                });
                let tally = tallies.get(&job).cloned().unwrap_or_default();
                let lost_to_most = tally
                    .lost_to
                    .iter()
                    .max_by_key(|(_, n)| **n)
                    .map(|(j, n)| (*j, *n));
                LaneJobStatus {
                    history: LaneJobHistory {
                        granted: tally.granted,
                        nudged: tally.nudged,
                        slot_busy: tally.slot_busy,
                        refused: [
                            tally.disabled,
                            tally.no_activity_since_start,
                            tally.still_active,
                            tally.interval_floor,
                        ],
                        lost_to_total: tally.lost_to.values().sum(),
                        lost_to_most,
                    },
                    job,
                    present,
                    registered: state.is_some(),
                    enabled: state.is_some_and(|s| s.enabled),
                    // From the clock, not `state`: the log dates a job before its loop ticks.
                    since_last_run_secs: last_run
                        .get(&job)
                        .map(|t| elapsed_since(now, *t).as_secs()),
                    interval_floor_secs: state.map(|s| s.interval_floor.as_secs()).unwrap_or(0),
                    idle_threshold_secs: state.map(|s| s.idle_threshold.as_secs()).unwrap_or(0),
                    blocked_by,
                }
            })
            .collect();

        LaneSnapshot {
            jobs,
            would_run: decision.job(),
            idle_reason: match decision {
                LaneDecision::Idle(reason) => Some(reason),
                LaneDecision::Run(_) => None,
            },
            idle_for_secs: idle_for.as_secs(),
            saw_activity_since_start,
            running: running.map(|(job, _)| job),
            running_for_secs: running.map(|(_, since)| since.elapsed().as_secs()),
            // From the holder: a status read must never take the slot itself.
            slot_busy: running.is_some(),
        }
    }

    async fn wake(&self, job: LaneJob) -> WakeOutcome {
        let Some(slot) = self.wake.get(&job) else {
            return WakeOutcome::NotPresent;
        };
        if !slot.present.load(std::sync::atomic::Ordering::Relaxed) {
            return WakeOutcome::NotPresent;
        }
        // `notify_one` keeps a permit, so a press mid-pass still gets a tick afterwards.
        slot.by_hand.notify_one();
        tracing::info!(job = job.as_str(), "inference lane: woken by hand");
        WakeOutcome::Woken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE_THRESHOLD: Duration = Duration::from_secs(900);
    const LONG_IDLE: Duration = Duration::from_secs(3600);

    // ── Standing down without leaving a hole ─────────────────────────────
    /// A job saying "not now" must still ask, via `acquire(enabled: false)`: a job that stops
    /// asking leaves a stale registration that wins every tie-break.
    #[tokio::test]
    async fn a_job_that_stands_down_neither_wins_nor_is_nudged() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        let standing_down = lane.claim(LaneJob::ProactiveReview);
        let _wants_it = lane.claim(LaneJob::MemoryExtraction);

        // Extraction runs once, so without the stand-down the never-run reviewer would win.
        drop(
            ask(&lane, LaneJob::MemoryExtraction)
                .await
                .expect("free slot"),
        );

        // The reviewer asks, and says it does not want the slot.
        assert!(
            lane.acquire(
                LaneJob::ProactiveReview,
                false,
                Cadence::new(Duration::ZERO, IDLE_THRESHOLD, false),
                false,
                true,
                LONG_IDLE,
            )
            .await
            .is_none(),
            "a job that stood down does not get the slot"
        );

        // It is registered, so the lane can see it -- and passed over.
        let snapshot = lane.snapshot().await;
        let reviewer = snapshot
            .jobs
            .iter()
            .find(|j| j.job == LaneJob::ProactiveReview)
            .unwrap();
        assert!(reviewer.registered, "standing down is still asking");
        assert!(!reviewer.enabled);
        assert_eq!(
            snapshot.would_run,
            Some(LaneJob::MemoryExtraction),
            "the job that wants the slot wins it, despite the shorter wait"
        );

        // No nudge: that would wake a job only to decline again.
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                standing_down.scheduled.notified(),
            )
            .await
            .is_err(),
            "a job that stood down must not be nudged"
        );
    }

    /// Control for the test above: rules out the reviewer being unable to win at all.
    #[tokio::test]
    async fn the_same_job_asking_in_earnest_takes_the_slot() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        let _reviewer = lane.claim(LaneJob::ProactiveReview);
        let _wants_it = lane.claim(LaneJob::MemoryExtraction);
        drop(
            ask(&lane, LaneJob::MemoryExtraction)
                .await
                .expect("free slot"),
        );

        assert!(
            ask(&lane, LaneJob::ProactiveReview).await.is_some(),
            "the never-run job wins on the longer wait"
        );
        let snapshot = lane.snapshot().await;
        assert!(
            snapshot
                .jobs
                .iter()
                .find(|j| j.job == LaneJob::ProactiveReview)
                .unwrap()
                .enabled
        );
    }

    // ── The durable clock ────────────────────────────────────────────────
    #[test]
    fn a_clock_that_went_backwards_reads_as_just_ran_not_as_a_long_wait() {
        let t = DateTime::from_timestamp(1_760_000_000, 0).unwrap();

        assert_eq!(
            elapsed_since(t + Duration::from_secs(90), t),
            Duration::from_secs(90),
            "ordinary forward time"
        );
        // NTP step or RTC-less boot: must not saturate to `Duration::MAX` or panic.
        assert_eq!(
            elapsed_since(t, t + Duration::from_secs(90)),
            Duration::ZERO,
            "a stamp in the future must read as a recent run"
        );
    }

    /// `should_run` skips the floor for `None`, so a restart must not read as "never ran".
    #[tokio::test]
    async fn a_restart_does_not_hand_a_job_a_free_pass_through_its_interval_floor() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let a_minute_ago = Utc::now() - Duration::from_secs(60);
        let lane = InferenceLane::restored(
            HashMap::from([(LaneJob::Consolidation, a_minute_ago)]),
            None,
        );
        let _bell = lane.claim(LaneJob::Consolidation);

        // A day's floor, a minute after the last run; asking also registers the cadence.
        let refused = lane
            .acquire(
                LaneJob::Consolidation,
                true,
                Cadence::new(Duration::from_secs(86_400), IDLE_THRESHOLD, false),
                false,
                true,
                LONG_IDLE,
            )
            .await;
        assert!(
            refused.is_none(),
            "a job that ran a minute before the restart is still inside its floor"
        );

        let snapshot = lane.snapshot().await;
        let job = snapshot
            .jobs
            .iter()
            .find(|j| j.job == LaneJob::Consolidation)
            .unwrap();
        assert_eq!(
            job.blocked_by,
            Some(pond_core::user_data::services::consolidation_schedule::SkipReason::IntervalFloor)
        );
        assert!(
            job.since_last_run_secs
                .is_some_and(|s| (55..=65).contains(&s)),
            "the age comes from the restored stamp, not from this process: {:?}",
            job.since_last_run_secs
        );
    }

    /// Control for the test above: shows the restored stamp is what refuses it.
    #[tokio::test]
    async fn without_a_restored_stamp_the_same_job_takes_the_slot() {
        let lane = InferenceLane::new();
        let _bell = lane.claim(LaneJob::Consolidation);
        assert!(
            lane.acquire(
                LaneJob::Consolidation,
                true,
                Cadence::new(Duration::from_secs(86_400), IDLE_THRESHOLD, false),
                false,
                true,
                LONG_IDLE,
            )
            .await
            .is_some(),
            "a job that has genuinely never run is not inside any floor"
        );
    }

    /// `select_next` ranks a never-run job's `None` as `Duration::MAX`.
    #[tokio::test]
    async fn a_job_that_never_ran_still_outranks_one_restored_from_disk() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        // Titling is declared first: restoring it means only the clock can make extraction win.
        let lane = InferenceLane::restored(
            HashMap::from([(LaneJob::Titling, Utc::now() - Duration::from_secs(60))]),
            None,
        );
        let _t = lane.claim(LaneJob::Titling);
        let _m = lane.claim(LaneJob::MemoryExtraction);

        // Register both without running either: a third job holds the slot.
        let _other = lane.claim(LaneJob::Consolidation);
        let held = ask(&lane, LaneJob::Consolidation).await.expect("free slot");
        assert!(ask(&lane, LaneJob::Titling).await.is_none());
        assert!(ask(&lane, LaneJob::MemoryExtraction).await.is_none());
        drop(held);

        let snapshot = lane.snapshot().await;
        assert_eq!(
            snapshot.would_run,
            Some(LaneJob::MemoryExtraction),
            "the job with no stamp is the starved one"
        );
    }

    #[tokio::test]
    async fn releasing_the_slot_posts_the_run_to_the_writer() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let lane = InferenceLane::restored(HashMap::new(), Some(tx));
        let _bell = lane.claim(LaneJob::Titling);

        let before = Utc::now();
        drop(ask(&lane, LaneJob::Titling).await.expect("free slot"));

        let (job, at) = rx.try_recv().expect("the release was posted");
        assert_eq!(job, LaneJob::Titling);
        assert!(at >= before && at <= Utc::now());
        assert!(rx.try_recv().is_err(), "one release, one message");
    }

    #[tokio::test]
    async fn a_tick_that_won_nothing_posts_nothing() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let lane = InferenceLane::restored(HashMap::new(), Some(tx));
        let _bell = lane.claim(LaneJob::Titling);
        let _other = lane.claim(LaneJob::Consolidation);

        let held = ask(&lane, LaneJob::Consolidation).await.expect("free slot");
        assert!(ask(&lane, LaneJob::Titling).await.is_none());
        assert!(rx.try_recv().is_err(), "a refusal is not a run");
        drop(held);
        assert_eq!(rx.try_recv().unwrap().0, LaneJob::Consolidation);
    }

    /// Boot wires the lane before starting loops, so every job starts unregistered.
    #[tokio::test]
    async fn a_restored_job_reports_its_age_before_its_loop_has_ticked() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::restored(
            HashMap::from([(LaneJob::Titling, Utc::now() - Duration::from_secs(300))]),
            None,
        );

        let snapshot = lane.snapshot().await;
        let job = snapshot
            .jobs
            .iter()
            .find(|j| j.job == LaneJob::Titling)
            .unwrap();
        assert!(!job.registered, "nothing has ticked yet");
        assert!(
            job.since_last_run_secs
                .is_some_and(|s| (295..=305).contains(&s)),
            "an unregistered job still knows when it last ran: {:?}",
            job.since_last_run_secs
        );
    }

    /// Tests and ponds whose run log failed to open both use this writer-less lane.
    #[tokio::test]
    async fn a_lane_with_no_writer_still_advances_its_own_clock() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        let _bell = lane.claim(LaneJob::Titling);
        drop(ask(&lane, LaneJob::Titling).await.expect("free slot"));

        let snapshot = lane.snapshot().await;
        let job = snapshot
            .jobs
            .iter()
            .find(|j| j.job == LaneJob::Titling)
            .unwrap();
        assert!(
            job.since_last_run_secs.is_some(),
            "the in-memory clock advanced even with no log behind it"
        );
    }

    // ── Running a job by hand ────────────────────────────────────────────
    #[test]
    fn only_a_person_waives_the_gates() {
        assert!(Tick::HandAsked.waives());
        assert!(!Tick::Poll.waives());
        assert!(
            !Tick::Scheduled.waives(),
            "a nudge from the lane must not read as somebody standing at the panel"
        );
    }

    #[tokio::test]
    async fn a_losing_tick_wakes_the_job_that_should_have_run() {
        let lane = InferenceLane::new();
        let winner_bells = lane.claim(LaneJob::Consolidation);
        let _loser_bells = lane.claim(LaneJob::MemoryExtraction);
        let _other = lane.claim(LaneJob::Titling);

        // Extraction runs once, so it has a real wait to lose on.
        drop(
            ask(&lane, LaneJob::MemoryExtraction)
                .await
                .expect("free slot"),
        );

        // `claim` doesn't register; asking while the slot is held registers it without a run.
        let held = ask(&lane, LaneJob::Titling).await.expect("free slot");
        assert!(
            lane.acquire(
                LaneJob::Consolidation,
                true,
                Cadence::new(Duration::ZERO, IDLE_THRESHOLD, false),
                false,
                true,
                LONG_IDLE,
            )
            .await
            .is_none(),
            "the slot is held, so this registers without running"
        );
        drop(held);

        // Consolidation never ran and extraction just did, so extraction loses.
        let lost = lane
            .acquire(
                LaneJob::MemoryExtraction,
                true,
                Cadence::new(Duration::ZERO, IDLE_THRESHOLD, false),
                false,
                true,
                LONG_IDLE,
            )
            .await;
        assert!(lost.is_none(), "the asker is not the winner");

        // The winner's bell holds a permit, so its loop wakes at once.
        let woke = tokio::time::timeout(Duration::from_secs(5), async {
            wait_for_tick(Duration::from_secs(3600), &winner_bells).await
        })
        .await
        .expect("the winner should have been nudged, not left asleep");
        assert_eq!(
            woke,
            Tick::Scheduled,
            "nudged by the lane, which waives nothing"
        );
    }

    #[tokio::test]
    async fn losing_and_being_switched_off_are_different_numbers() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        let _w = lane.claim(LaneJob::Consolidation);
        let _l = lane.claim(LaneJob::MemoryExtraction);
        let _t = lane.claim(LaneJob::Titling);

        // Extraction gets a real wait; consolidation registers unrun, so it wins from here.
        drop(ask(&lane, LaneJob::MemoryExtraction).await.expect("free"));
        let held = ask(&lane, LaneJob::Titling).await.expect("free");
        let _ = lane
            .acquire(
                LaneJob::Consolidation,
                true,
                Cadence::new(Duration::ZERO, IDLE_THRESHOLD, false),
                false,
                true,
                LONG_IDLE,
            )
            .await;
        drop(held);

        for _ in 0..3 {
            assert!(lane
                .acquire(
                    LaneJob::MemoryExtraction,
                    true,
                    Cadence::new(Duration::ZERO, IDLE_THRESHOLD, false),
                    false,
                    true,
                    LONG_IDLE,
                )
                .await
                .is_none());
        }

        let snapshot = lane.snapshot().await;
        let extraction = snapshot
            .jobs
            .iter()
            .find(|j| j.job == LaneJob::MemoryExtraction)
            .expect("listed");

        assert_eq!(extraction.history.lost_to_total, 3, "it lost three times");
        assert_eq!(
            extraction.history.lost_to_most,
            Some((LaneJob::Consolidation, 3)),
            "and it can say to whom"
        );
        assert_eq!(
            extraction.blocked_by, None,
            "losing is not a gate refusal, so the snapshot alone cannot see it"
        );
        assert_eq!(
            extraction.history.refused,
            [0, 0, 0, 0],
            "and no gate refused it either"
        );

        // The winner was nudged once per loss.
        let consolidation = snapshot
            .jobs
            .iter()
            .find(|j| j.job == LaneJob::Consolidation)
            .expect("listed");
        assert_eq!(consolidation.history.nudged, 3);
    }

    #[tokio::test]
    async fn a_gate_refusal_is_counted_under_its_own_reason() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        let _c = lane.claim(LaneJob::Consolidation);
        assert!(lane
            .acquire(
                LaneJob::Consolidation,
                true,
                Cadence::new(Duration::ZERO, IDLE_THRESHOLD, false),
                false,
                false, // nobody has used this pond since boot
                LONG_IDLE,
            )
            .await
            .is_none());

        let snapshot = lane.snapshot().await;
        let job = snapshot
            .jobs
            .iter()
            .find(|j| j.job == LaneJob::Consolidation)
            .expect("listed");
        assert_eq!(
            job.history.refused,
            [0, 1, 0, 0],
            "no_activity_since_start, and not any other reason"
        );
        assert_eq!(
            job.history.lost_to_total, 0,
            "it did not lose; it was refused"
        );
        assert_eq!(job.history.granted, 0);
    }

    /// Control for the losing-tick test: rules out nudging on every tick.
    #[tokio::test]
    async fn a_winning_tick_nudges_nobody() {
        let lane = InferenceLane::new();
        let _mine = lane.claim(LaneJob::Consolidation);
        let others = lane.claim(LaneJob::MemoryExtraction);

        let won = ask(&lane, LaneJob::Consolidation).await;
        assert!(won.is_some(), "the only registered asker wins");
        drop(won);

        let nudged = tokio::time::timeout(Duration::from_millis(300), async {
            wait_for_tick(Duration::from_secs(3600), &others).await
        })
        .await;
        assert!(nudged.is_err(), "a winning tick must not wake anybody else");
    }

    #[tokio::test]
    async fn a_hand_asked_tick_does_not_poison_the_shared_registry() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        let _bells = lane.claim(LaneJob::Titling);
        let floor = Duration::from_secs(5 * 60);
        let quiet = Duration::from_secs(15 * 60);

        // A hand-asked tick: waived, and it runs.
        let slot = lane
            .acquire(
                LaneJob::Titling,
                true,
                Cadence::new(floor, quiet, false),
                true,
                false,
                Duration::ZERO,
            )
            .await;
        assert!(slot.is_some(), "a press runs it on a pond with no activity");
        drop(slot);

        // What the registry kept is the STANDING cadence, not the waiver.
        let snapshot = lane.snapshot().await;
        let titling = snapshot
            .jobs
            .iter()
            .find(|j| j.job == LaneJob::Titling)
            .expect("listed");
        assert_eq!(titling.interval_floor_secs, floor.as_secs());
        assert_eq!(titling.idle_threshold_secs, quiet.as_secs());
    }

    #[tokio::test]
    async fn waking_a_job_with_no_loop_reports_it_rather_than_pretending() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        assert_eq!(
            lane.wake(LaneJob::MemoryExtraction).await,
            WakeOutcome::NotPresent
        );

        // Control: a `wake` hardcoded to `NotPresent` would fail here.
        let _bell = lane.claim(LaneJob::MemoryExtraction);
        assert_eq!(
            lane.wake(LaneJob::MemoryExtraction).await,
            WakeOutcome::Woken
        );
        // Only the claimed job: one button must not wake every loop.
        assert_eq!(lane.wake(LaneJob::Titling).await, WakeOutcome::NotPresent);
    }

    #[tokio::test]
    async fn a_wake_cuts_short_a_poll_the_loop_is_already_asleep_on() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        let bell = lane.claim(LaneJob::Titling);

        // An hour, so a pass can only come from the doorbell.
        let ticking =
            tokio::spawn(async move { wait_for_tick(Duration::from_secs(3600), &bell).await });
        // Park the task first, or `notify_one`'s stored permit would pass the test instead.
        tokio::task::yield_now().await;

        lane.wake(LaneJob::Titling).await;
        let tick = tokio::time::timeout(Duration::from_secs(5), ticking)
            .await
            .expect("the doorbell should cut the hour short")
            .expect("the tick task should not panic");
        assert_eq!(
            tick,
            Tick::HandAsked,
            "a tick woken by a person reports itself as such, and only then waives"
        );
    }

    #[tokio::test]
    async fn a_hand_asked_job_still_cannot_take_a_slot_another_job_holds() {
        let lane = InferenceLane::new();
        let _bell = lane.claim(LaneJob::MemoryExtraction);

        let held = ask(&lane, LaneJob::Consolidation)
            .await
            .expect("the first asker wins the free slot");

        // Every gate waived; the held slot still refuses it.
        let asked = lane
            .acquire(
                LaneJob::MemoryExtraction,
                true,
                Cadence::new(Duration::ZERO, Duration::ZERO, true),
                true,
                false,
                Duration::ZERO,
            )
            .await;
        assert!(
            asked.is_none(),
            "a hand-asked job must queue behind the slot, not decode beside it"
        );

        drop(held);
    }

    #[tokio::test]
    async fn the_snapshot_names_the_job_holding_the_slot() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        let _bell = lane.claim(LaneJob::Titling);

        let idle = lane.snapshot().await;
        assert!(!idle.slot_busy);
        assert_eq!(idle.running, None);
        assert_eq!(idle.running_for_secs, None);

        let held = ask(&lane, LaneJob::Titling)
            .await
            .expect("wins the free slot");
        let busy = lane.snapshot().await;
        assert!(busy.slot_busy);
        assert_eq!(busy.running, Some(LaneJob::Titling));
        assert!(busy.running_for_secs.is_some(), "and for how long");

        drop(held);
        let after = lane.snapshot().await;
        assert!(!after.slot_busy);
        assert_eq!(after.running, None, "the holder is cleared with the guard");
    }

    #[tokio::test]
    async fn the_snapshot_names_every_job_present_or_not() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        let _bell = lane.claim(LaneJob::Titling);

        let snapshot = lane.snapshot().await;
        assert_eq!(snapshot.jobs.len(), LaneJob::ALL.len());
        for (status, &job) in snapshot.jobs.iter().zip(LaneJob::ALL) {
            assert_eq!(status.job, job, "jobs are reported in ALL order");
        }

        let titling = snapshot
            .jobs
            .iter()
            .find(|j| j.job == LaneJob::Titling)
            .unwrap();
        assert!(titling.present, "a claimed job has a loop here");
        assert!(
            !titling.registered,
            "claiming is not asking: nothing has reached a tick yet"
        );

        let other = snapshot
            .jobs
            .iter()
            .find(|j| j.job == LaneJob::Consolidation)
            .unwrap();
        assert!(!other.present, "an unclaimed job has no loop here");
    }

    /// `present` means a loop exists; `registered` means it has asked at least once.
    #[tokio::test]
    async fn asking_once_is_what_makes_a_job_registered() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        let _bell = lane.claim(LaneJob::Titling);
        drop(
            ask(&lane, LaneJob::Titling)
                .await
                .expect("wins the free slot"),
        );

        let snapshot = lane.snapshot().await;
        let titling = snapshot
            .jobs
            .iter()
            .find(|j| j.job == LaneJob::Titling)
            .unwrap();
        assert!(titling.registered, "a job that has asked is registered");
        assert!(
            titling.since_last_run_secs.is_some(),
            "and it has run, so it is no longer infinitely starved"
        );
    }

    /// Not exempt: these tests cover the shared gate and tie-break, which exempt jobs skip.
    async fn ask(lane: &InferenceLane, job: LaneJob) -> Option<LaneSlot<'_>> {
        lane.acquire(
            job,
            true,
            Cadence::new(Duration::ZERO, IDLE_THRESHOLD, false),
            // Not waived, for the same reason.
            false,
            true,
            LONG_IDLE,
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
                // Not exempt — the gate is the subject of this test.
                Cadence::new(Duration::ZERO, IDLE_THRESHOLD, false),
                false, // and not waived, or there would be no gate to refuse
                true,
                Duration::from_secs(5), // user active 5s ago
            )
            .await;
        assert!(slot.is_none());
    }

    #[tokio::test]
    async fn a_turn_in_flight_is_not_quiet_however_long_it_has_run() {
        use pond_core::user_data::ports::lane_control::LaneControl;
        use std::sync::atomic::{AtomicBool, Ordering};

        let running = Arc::new(AtomicBool::new(true));
        let lane = InferenceLane::new();
        let probe = running.clone();
        lane.watch_turns(move || probe.load(Ordering::Relaxed));
        let cadence = Cadence::new(Duration::ZERO, IDLE_THRESHOLD, false);

        // An hour since the request arrived, and the turn is still being answered.
        assert!(lane
            .acquire(
                LaneJob::SummaryRefresh,
                true,
                cadence,
                false,
                true,
                LONG_IDLE
            )
            .await
            .is_none());
        let snapshot = lane.snapshot().await;
        let job = snapshot
            .jobs
            .iter()
            .find(|j| j.job == LaneJob::SummaryRefresh)
            .expect("listed");
        assert_eq!(
            job.history.refused,
            [0, 0, 1, 0],
            "still_active, nothing else"
        );

        running.store(false, Ordering::Relaxed);
        assert!(
            lane.acquire(
                LaneJob::SummaryRefresh,
                true,
                cadence,
                false,
                true,
                LONG_IDLE
            )
            .await
            .is_some(),
            "the same quiet with the turn over"
        );
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
