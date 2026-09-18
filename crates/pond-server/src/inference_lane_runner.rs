//! The runtime half of the inference lane.
//!
//! [`pond_core::user_data::services::inference_lane`] decides *which* background
//! job may spend inference next. This owns the part that cannot be a pure
//! function: the registry of what each job currently wants, and the single slot
//! that makes "one at a time" true rather than merely intended.
//!
//! # Why a coordinator rather than one big loop
//!
//! Each background job keeps its own poll cadence, its own body and its own
//! cancellation watcher — those differ enough (60s vs 5min, one sweep vs one
//! pipeline) that merging them would be a rewrite with no gain. What they must
//! NOT keep is a private answer to "may I run now?", because that answer has to
//! account for jobs this loop has never heard of. So each loop asks the lane
//! instead, and the lane is the only thing that says yes.
//!
//! # The two guarantees
//!
//! 1. **At most one job runs at a time**, because [`LaneSlot`] is a `Mutex` guard
//!    and a job holds it for the whole of its run. This is what replaces the
//!    pairwise "stand down while consolidation is mid-run" check that titling
//!    used to carry — a check that only ever ran in one direction.
//! 2. **No job starves**, because the decision is least-recently-run rather than
//!    a fixed priority. See `inference_lane::select_next`.

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

/// Proof that the holder owns the inference slot.
///
/// Dropping it releases the slot, so a job that returns early — or panics —
/// cannot wedge the lane. That is the reason this is a guard and not a boolean:
/// every early return in a job body is a path somebody would have had to
/// remember to write a release on.
pub struct LaneSlot<'a> {
    _guard: tokio::sync::MutexGuard<'a, ()>,
    lane: &'a InferenceLane,
    job: LaneJob,
}

/// Spending the interval budget is tied to the guard's lifetime, exactly like
/// releasing the slot, and for the same reason.
///
/// This used to be an explicit `finish()` the job body had to call, which made
/// the release automatic and the spend manual — and every early return was then
/// a path somebody had to remember. Two of them were missed immediately: the
/// titling loop returns early when no LLM provider is configured and when the
/// session list read fails, both AFTER taking the slot.
///
/// The consequence was not a missed pass, it was permanent starvation. A job
/// that never records a run keeps `since_last_run: None`, which
/// `select_next` treats as infinitely starved — so it wins every tie against
/// every job that HAS run, on every tick, forever. On any pond without a
/// provider configured, titling would have taken the slot and silently locked
/// consolidation out for the life of the process: the precise failure the lane
/// was built to make impossible.
///
/// An attempt spends the budget whatever the outcome — cancelled, or having
/// found nothing to do — because retrying a fruitless expensive pass on every
/// tick is the churn the interval floor exists to prevent.
impl Drop for LaneSlot<'_> {
    fn drop(&mut self) {
        // A std mutex, not tokio's, so this is lockable from `drop`. Safe
        // against the lock order in `acquire`, which releases `last_run` before
        // it ever reaches for the slot.
        //
        // A wall clock and not an `Instant`, because this stamp outlives the
        // process: see `InferenceLane::last_run`.
        let at = Utc::now();
        match self.lane.last_run.lock() {
            Ok(mut last_run) => {
                last_run.insert(self.job, at);
            }
            // A poisoned lock means another thread panicked mid-insert. Recover
            // the map rather than panicking again inside a drop, which would
            // abort the process.
            Err(poisoned) => {
                poisoned.into_inner().insert(self.job, at);
            }
        }

        // And to disk, through a channel, because `drop` cannot await. An
        // unbounded sender never blocks and never yields, which is what makes
        // it safe here: the alternative -- spawning a task -- needs a runtime
        // that a test dropping this guard is not guaranteed to be inside.
        //
        // The send is deliberately unchecked. A closed channel means the writer
        // task is gone, which happens at shutdown; the in-memory clock above
        // has already advanced, so the lane keeps scheduling correctly and only
        // a restart loses this one stamp.
        if let Some(runs) = &self.lane.runs {
            let _ = runs.send((self.job, at));
        }

        // And the holder, in the same Drop for the same reason: a job that
        // returned early -- or panicked -- must not leave the watcher saying it
        // is still running. Cleared unconditionally rather than compared
        // against `self.job`, because the only way a different job could be in
        // there is a bug, and holding a stale name is worse than clearing one.
        match self.lane.holder.lock() {
            Ok(mut holder) => *holder = None,
            Err(poisoned) => *poisoned.into_inner() = None,
        }
    }
}

/// What a job currently wants, refreshed by that job on every one of its ticks.
///
/// Held by the lane so that a decision made on one job's tick can account for
/// every other job's cadence, including jobs whose own poll is minutes away.
#[derive(Debug, Clone, Copy)]
struct Registration {
    enabled: bool,
    interval_floor: Duration,
    /// This job's own quiet requirement — see `JobState::idle_threshold`.
    idle_threshold: Duration,
    /// May this job run before the pond has served a turn — see
    /// `JobState::exempt_from_activity_gate`.
    exempt_from_activity_gate: bool,
}

/// One job's doorbell, and whether anybody is home to hear it.
///
/// Created for every job up front, whether or not its loop exists in this
/// process, so `wake` can tell "no loop here" from "unknown job" without a
/// second lookup. `present` is set by the loop's owner at spawn time via
/// [`InferenceLane::claim`].
struct Wake {
    /// Rung by a person pressing Run now. A tick on this bell waives the quiet
    /// period, the interval floor and the activity gate.
    by_hand: Arc<tokio::sync::Notify>,
    /// Rung by the lane itself when this job is the one that should run next.
    ///
    /// A SECOND bell, and that separation is the whole point. There was one,
    /// and `Cadence::for_tick` zeroed every gate for anything that rang it — so
    /// a nudge sent on it would make every scheduled tick read as a person
    /// standing at the panel, and the pond would do background work in the
    /// middle of a conversation.
    scheduled: Arc<tokio::sync::Notify>,
    present: std::sync::atomic::AtomicBool,
}

/// What the lane was last told about the household, and when.
///
/// The lane is handed `saw_activity_since_start` and `idle_for` by whichever
/// job is ticking; it does not own the activity clock and must not, because
/// then it would need the session store and stop being a decision. So it
/// REMEMBERS the last reading, and the status snapshot extrapolates from it.
///
/// The extrapolation is bounded and one-directional: idle only grows unless
/// somebody came back, and if they did, the next tick corrects it. The shortest
/// poll on the lane is 60s, so a snapshot is at worst that stale — and it says
/// so rather than presenting a stale reading as live.
#[derive(Clone, Copy)]
struct Observation {
    at: Instant,
    saw_activity_since_start: bool,
    idle_for: Duration,
}

/// What has happened to one job since this process started.
///
/// # Why counters and not log lines
///
/// The status route can already say why a job is refused RIGHT NOW
/// (`LaneJobStatus::blocked_by`), and that is an instant, not a history. It
/// cannot tell "eligible and losing the tie-break 1,340 times" from "switched
/// off" — the two look identical in a snapshot, and the first is the defect
/// step one was written for.
///
/// The obvious alternative was to log every refusal. Seven jobs on polls from
/// 30 seconds up is on the order of ten thousand lines a day, written to the
/// same eMMC the GGUFs live on, to answer a question a `u32` answers. So the
/// refusal sites stay at `trace!` and this carries the history instead. The
/// GRANT moves to `info!`: about twenty-six lines a day on the pond measured
/// today, and the one line whose absence was the whole symptom.
///
/// Kept OUTSIDE `Registration`, which `acquire` overwrites on every single
/// tick — counters stored there would reset before anybody could read them.
#[derive(Debug, Clone, Default)]
struct Tally {
    /// Times this job took the slot.
    granted: u32,
    /// Times the lane rang this job's scheduled bell because it should have
    /// been running. The measurement for step one: on a pond where this stays
    /// at zero while `lost_to` climbs, the nudge is not reaching anybody.
    nudged: u32,
    /// Times it asked while another job held the slot.
    slot_busy: u32,
    /// Times its own gate refused it, by reason.
    disabled: u32,
    no_activity_since_start: u32,
    still_active: u32,
    interval_floor: u32,
    /// Who beat it, and how often. The answer to "why am I never picked?".
    lost_to: std::collections::BTreeMap<LaneJob, u32>,
}

/// The shared inference slot and the registry of what wants it.
pub struct InferenceLane {
    slot: tokio::sync::Mutex<()>,
    /// Std rather than tokio so [`LaneSlot`]'s `Drop` can record a run. Only
    /// ever held for a map insert or read, never across an await.
    ///
    /// A wall clock, not an `Instant`, and that is the whole point: this map is
    /// seeded from `lane_job_runs` at boot, so it has to hold a time that means
    /// something in another process. Reconstructing an `Instant` from a stored
    /// age would also be a real hazard rather than an aesthetic one --
    /// `Instant::now() - age` panics outright on a machine whose monotonic
    /// clock started later than the age being subtracted, which is every Jetson
    /// boot.
    ///
    /// The cost is that this clock can now go backwards (an NTP step, a board
    /// with no RTC). `elapsed_since` is where that is handled.
    last_run: std::sync::Mutex<HashMap<LaneJob, DateTime<Utc>>>,
    /// Where released slots are written down. `None` on a lane with no log --
    /// every test, and any pond whose log failed to open.
    runs: Option<tokio::sync::mpsc::UnboundedSender<(LaneJob, DateTime<Utc>)>>,
    /// Which job holds the slot right now, and since when.
    ///
    /// The lane could already say the slot was BUSY -- `try_lock` answers
    /// that -- but not by whom, which is the one thing somebody watching this
    /// screen wants to know. "Something is using the model" and "the memory
    /// engine is using the model" are different sentences to a household
    /// wondering why the pond is slow.
    ///
    /// Std, held only for a write or a copy, never across an await. Set where
    /// the slot is taken and cleared in `LaneSlot`'s `Drop`, so it cannot drift
    /// from the guard: every path out of a job body, panic included, clears it.
    holder: std::sync::Mutex<Option<(LaneJob, Instant)>>,
    registry: tokio::sync::Mutex<HashMap<LaneJob, Registration>>,
    /// One doorbell per job. Fixed at construction and never mutated, so no
    /// lock: the only mutable part is each slot's `present` flag, which is an
    /// atomic.
    wake: HashMap<LaneJob, Wake>,
    /// Std, and held only for a copy. Written on every `acquire`, read by the
    /// status snapshot.
    observed: std::sync::Mutex<Option<Observation>>,
    /// What has happened to each job since boot. See [`Tally`].
    tallies: std::sync::Mutex<HashMap<LaneJob, Tally>>,
}

/// How long ago `then` was, from `now`, without ever panicking or wrapping.
///
/// The lane's clock is wall time now, so both directions of skew are reachable
/// and neither may take the pond down:
///
/// - **Backwards** (`then` is in the future): an NTP step, or a board with no
///   RTC that boots at the epoch and syncs a moment later. Answering
///   `Duration::ZERO` reads as "it just ran", which holds the job behind its
///   interval floor until the clock is believable again. The other direction --
///   treating a future stamp as a long wait -- would let every job fire at
///   once on exactly the boot where the pond has least to spare.
/// - **Forwards** by a lot: the same board after its first sync. The stamps
///   look ancient and the jobs look starved, which is the honest reading: a lot
///   of real time probably did pass.
fn elapsed_since(now: DateTime<Utc>, then: DateTime<Utc>) -> Duration {
    now.signed_duration_since(then)
        .to_std()
        .unwrap_or(Duration::ZERO)
}

impl InferenceLane {
    /// A lane with no memory of previous processes and nowhere to write.
    pub fn new() -> Arc<Self> {
        Self::restored(HashMap::new(), None)
    }

    /// How long since this job last ran, from the same clock `acquire` reads.
    ///
    /// For a job whose own gate runs BEFORE it asks for the slot. The reviewer
    /// is the one: its gate layers three refusals the lane knows nothing about
    /// -- the orchestrator toggle, a run already in flight, and the daily cap
    /// on interrupting a household -- so it decides whether there is anything
    /// worth doing before it asks for the machine.
    ///
    /// The point of exposing this rather than letting it keep its own clock is
    /// that two clocks disagreeing is worse than one being wrong. It kept an
    /// `Option<Instant>` in its own stack frame, which meant its interval floor
    /// reset on every restart while the lane's did not -- so the two halves of
    /// one decision could answer differently about the same run.
    pub fn since_last_run(&self, job: LaneJob) -> Option<Duration> {
        let last_run = self
            .last_run
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        last_run.get(&job).map(|t| elapsed_since(Utc::now(), *t))
    }

    /// A lane seeded from the durable log, writing new runs back to it.
    ///
    /// Taking the history by value rather than reading the port here keeps this
    /// crate's lane free of the repository, and keeps the one fallible step --
    /// the load -- at the wiring site where a failure can be logged and
    /// downgraded to `new()`.
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
        })
    }

    /// Count something that happened to a job. One place, so a poisoned lock is
    /// recovered once rather than at seven call sites.
    fn tally(&self, job: LaneJob, f: impl FnOnce(&mut Tally)) {
        let mut tallies = self
            .tallies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(tallies.entry(job).or_default());
    }

    /// Take this job's doorbell, and record that a loop for it exists here.
    ///
    /// Called once, at spawn time, by whoever owns the loop — NOT on every
    /// tick. A loop that is spawned conditionally (extraction needs an embedder,
    /// the index sweep needs an embedder and a vector model) simply never
    /// claims, and the status route then reports `present: false` for it rather
    /// than leaving a household pressing a button that has nothing to ring.
    ///
    /// The returned pair is what the loop selects on beside its own sleep: one
    /// bell a person rings, one the lane rings. `wait_for_tick` takes both and
    /// reports which rang, because they mean different things about the gates.
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

    /// Ask for the inference slot on behalf of `job`.
    ///
    /// `Some` means this job won the tick and now holds the slot until the
    /// guard is dropped. `None` means either the shared
    /// gate refused every job (the household is mid-conversation, or nothing has
    /// happened since boot), another job was more starved, or another job is
    /// mid-run right now.
    ///
    /// The last of those is why this uses `try_lock` rather than waiting: a job
    /// that queued for the slot would run *after* the conditions that qualified
    /// it had passed — the household could be back, and the whole point of the
    /// idle gate is that background work yields to people. Missing a pass is
    /// correct; the next tick is a minute away.
    pub async fn acquire(
        &self,
        job: LaneJob,
        enabled: bool,
        // The job's STANDING cadence — never a waived one. This is what lands
        // in the shared registry and what every other job's tick will evaluate
        // this job against, so a value that is only true for one tick must not
        // reach it. See `Cadence::new`.
        cadence: Cadence,
        // True only on a hand-asked tick, and applied ONLY to this job's own
        // gate, below. Deliberately not part of `Cadence`: it was, and one press
        // of Run now then stamped "exempt from everything" into the registry for
        // up to a whole poll period.
        waive: bool,
        saw_activity_since_start: bool,
        idle_for: Duration,
    ) -> Option<LaneSlot<'_>> {
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

        // Remembered before the decision, so a snapshot taken between two ticks
        // still has the freshest reading any job has taken -- including from a
        // tick that went on to refuse every job.
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

            // The registry is a HashMap, so this vec arrives in whatever order
            // hashing produced. `select_next` breaks ties by position, so
            // handing it an arbitrary order would make two equally-starved jobs
            // resolve differently between runs — a coin flip that presents as a
            // job which "sometimes doesn't run". Sorting restores the documented
            // tie-break; `LaneJob: Ord` is declaration order.
            states.sort_unstable_by_key(|s| s.job);

            // The waiver, applied to exactly one entry: the job that is asking.
            // Patched here rather than written into the registry so it lasts
            // for this decision and no longer -- and so a hand-asked tick can
            // never make a job look eligible to somebody else's tick.
            if waive {
                if let Some(asker) = states.iter_mut().find(|s| s.job == job) {
                    asker.interval_floor = Duration::ZERO;
                    asker.idle_threshold = Duration::ZERO;
                    asker.exempt_from_activity_gate = true;
                }
            }

            // The asker's OWN verdict, recomputed. `select_next` returns the
            // single reason that explains the most jobs, which is the right
            // thing for "why is nothing happening" and the wrong thing for
            // "why am I never picked" -- the question these counters answer.
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
                // This call IS somebody asking, so an equal wait goes to them
                // rather than to whichever job happens to be declared first and
                // asleep. See `LaneInputs::asking`.
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
                // THE ANSWER IS COMPUTED AND THROWN AWAY -- that was the defect.
                //
                // The lane names the job that should run, and the caller, which
                // is not that job, returns `None`. The winner is asleep on its
                // own timer, which may be fifteen minutes away, and nothing
                // tells it. Measured on a real pond: 28 hand-wakes against 26
                // grants, because a tick whose winner was not the caller
                // produced no run, no reschedule and no retry.
                //
                // So ring the winner's SCHEDULED bell. That bell buys an early
                // look and nothing else: `Tick::Scheduled` applies every gate
                // exactly as `Tick::Poll` does, so the winner re-enters through
                // the same quiet period and interval floor and may still be
                // refused. What it cannot now do is sleep through its own turn.
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

        // Won the decision — now take the slot, or stand down. See the doc
        // comment for why this does not wait.
        match self.slot.try_lock() {
            Ok(guard) => {
                // INFO, not debug. About twenty-six lines a day on the pond
                // measured today, and the one line whose absence was the whole
                // symptom -- a shipped log that never said the lane granted
                // anything, because at `debug` it never reached the file.
                tracing::info!(job = job.as_str(), "inference lane: slot acquired");
                self.tally(job, |t| t.granted += 1);
                // Recorded here rather than by the caller, so it cannot be
                // forgotten by a job body: taking the guard and being named as
                // the holder are the same event.
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

/// What one tick uses for its cadence, once it is known whether a person asked
/// for it.
///
/// A hand-asked tick is not background work. The idle gate exists to stop
/// chores stealing the machine from somebody who is using it, and here the
/// person using it IS the reason to run — so quiet and the interval floor both
/// drop to zero, and the activity gate is waived. The index sweep has worked
/// this way since the Reindex button shipped; this is that rule, named, so the
/// other five jobs get it identically rather than each re-deriving it.
///
/// What it does NOT touch is the slot. Exclusion is the lane's one hard
/// guarantee and a button must not be able to buy its way past it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadence {
    pub interval_floor: Duration,
    pub idle_threshold: Duration,
    pub exempt_from_activity_gate: bool,
}

impl Cadence {
    /// A job's STANDING cadence — what it always wants, on any tick.
    ///
    /// There used to be a `for_tick(asked, ..)` that returned zeros when a
    /// person had pressed Run now, and the job then passed those zeros to
    /// `acquire`, which wrote them into the SHARED registry. So one press
    /// stamped "always eligible, exempt from every gate" onto that job for up
    /// to its whole poll period, and every OTHER job's tick then evaluated it
    /// as runnable in the middle of a conversation.
    ///
    /// The waiver is now a separate argument to `acquire`, applied only while
    /// evaluating the asker's own gate. The registry never sees it, so it
    /// cannot outlive the tick that earned it.
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
    /// The bell a PERSON rings, for a caller that needs it on its own.
    ///
    /// One caller: `AppState::index_reindex`, which the Reindex button rings
    /// after clearing the index. It is the hand bell and not the scheduled one
    /// because somebody is standing there watching an empty panel — the quiet
    /// period exists to keep chores off the machine while a person is using it,
    /// and here the person IS the reason to run.
    pub fn hand_bell(&self) -> Arc<tokio::sync::Notify> {
        self.by_hand.clone()
    }

    /// Wait on both, and say which rang. The sweep has its own `select!` with a
    /// cancellation arm, so it cannot use `wait_for_tick`.
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
    /// A person pressed Run now. Quiet, floor and the activity gate are waived
    /// — the person IS the activity — but never the slot.
    HandAsked,
    /// The lane rang: this job is the one that should run next. Every gate
    /// applies, exactly as on `Poll`. The bell buys an early LOOK, never a
    /// relaxed rule.
    Scheduled,
}

impl Tick {
    /// Does this tick waive the politeness gates?
    pub fn waives(self) -> bool {
        matches!(self, Tick::HandAsked)
    }
}

/// Sleep until this job's next tick, and report whether a person asked for it.
///
/// Every lane loop waits here instead of on a bare `sleep`, which is what makes
/// "run it now" possible at all: without a doorbell to select on, the shortest
/// wait a button could produce is the job's own poll — fifteen minutes for the
/// index sweep.
pub async fn wait_for_tick(poll: Duration, bells: &Bells) -> Tick {
    tokio::select! {
        _ = tokio::time::sleep(poll) => Tick::Poll,
        _ = bells.by_hand.notified() => Tick::HandAsked,
        _ = bells.scheduled.notified() => Tick::Scheduled,
    }
}

/// Seeing the lane, and asking it to run something now.
///
/// The port's own docs carry the reasoning; what is worth saying here is where
/// each answer comes from, because they have three different freshnesses:
///
///   `present`    construction time, from `claim`. Never stale.
///   `enabled`, the two cadences, `blocked_by`
///                that job's LAST tick. Up to its own poll old — 15 minutes for
///                the index sweep.
///   `idle_for`, `saw_activity_since_start`
///                the last tick of ANY job, extrapolated. At worst 60s old,
///                because the shortest poll on the lane is 60s.
///
/// A snapshot therefore describes what the next tick would decide, not what is
/// true to the millisecond, and the route says so rather than implying live
/// numbers.
#[async_trait::async_trait]
impl pond_core::user_data::ports::lane_control::LaneControl for InferenceLane {
    async fn snapshot(&self) -> LaneSnapshot {
        use pond_core::user_data::services::consolidation_schedule::{self as sched, GateInputs};

        // No job has ticked yet. Reporting `idle_for: 0` and
        // `saw_activity: false` is the honest reading of that: nothing has
        // observed the household, and the "never on startup" guard is exactly
        // what a process in this state is subject to.
        let observed = self.observed.lock().ok().and_then(|o| *o);
        let (saw_activity_since_start, idle_for) = match observed {
            // Idle only grows unless somebody came back, and if they did the
            // next tick corrects this downward. Growing it is the direction
            // that cannot invent quiet the pond has not had.
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

        // Only registered jobs go to `select_next` — a job whose loop has never
        // ticked has told the lane nothing, and inventing a default cadence for
        // it would put a job in the decision that never asked to be there.
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
            // Nobody is asking: this is a read. Naming a job here would make
            // `would_run` report whoever the status route happened to mention.
            asking: None,
        });

        // Every job in ALL order, present or not. A job missing from the list
        // would be a job nobody can see is missing, which is the failure this
        // whole surface exists to end.
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
                    // From the clock, not from `state`. A job whose loop has
                    // not ticked yet this process is unregistered, but the
                    // durable log may well know when it last ran -- and
                    // reporting "never" because a loop is still starting is
                    // exactly the lie this clock was made durable to end.
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
            // `try_lock` rather than `lock`: this is a status read and must not
            // queue behind the job it is reporting on. Taking it proves nobody
            // else holds it, and the guard is dropped at the end of the
            // expression -- it is the raw mutex guard, not `LaneSlot`, so
            // nothing records a run.
            running: running.map(|(job, _)| job),
            running_for_secs: running.map(|(_, since)| since.elapsed().as_secs()),
            // Derived from the holder rather than from `try_lock`. The old
            // version took the real mutex to answer a status question, which
            // meant a read could briefly hold the thing it was reporting on;
            // and it could only ever say BUSY, never by whom.
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
        // `notify_one`, matching the Reindex precedent: there is one loop per
        // job, and this variant holds a permit if the loop is mid-pass, so a
        // press during a run still gets a fresh tick afterwards rather than
        // being swallowed.
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

    /// A job that has something to say "not now" about must still ASK.
    ///
    /// This is the invariant every lane loop keeps by construction and the
    /// reason none of the reviewer's own refusals is an early return. A job
    /// that stops calling `acquire` leaves its last registration behind; the
    /// clock keeps running; and because a never-run or long-ago-run job carries
    /// the longest apparent wait, the stale entry then wins every tie-break it
    /// is offered. Step 1 made that strictly worse: the losing job now NUDGES
    /// the winner, so the lane would wake a job that has already decided not to
    /// run, over and over, while the job that actually wanted the slot refused
    /// itself.
    ///
    /// `acquire(enabled: false)` is how a job says "not me" without leaving
    /// that hole -- it refreshes the registration and takes the job out of the
    /// running in the same call.
    #[tokio::test]
    async fn a_job_that_stands_down_neither_wins_nor_is_nudged() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        let standing_down = lane.claim(LaneJob::ProactiveReview);
        let _wants_it = lane.claim(LaneJob::MemoryExtraction);

        // Extraction runs once, so it has a real -- and therefore SHORTER --
        // wait than the job that has never run. Without the stand-down, the
        // never-run reviewer wins on `Duration::MAX` and on declaration order.
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

        // And the doorbell stays silent. A nudge here would be the lane waking
        // a job to decline again.
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

    /// The control: the same reviewer, asking in earnest, DOES win.
    ///
    /// Without this, the test above would pass if `ProactiveReview` were simply
    /// unable to win a tie-break for some other reason -- a missing doorbell, a
    /// registry that ignores it, a declaration-order accident.
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

    /// The clock is wall time now, so both directions of skew are reachable.
    #[test]
    fn a_clock_that_went_backwards_reads_as_just_ran_not_as_a_long_wait() {
        let t = DateTime::from_timestamp(1_760_000_000, 0).unwrap();

        assert_eq!(
            elapsed_since(t + Duration::from_secs(90), t),
            Duration::from_secs(90),
            "ordinary forward time"
        );
        // An NTP step, or a board with no RTC that boots at the epoch. The
        // saturating direction matters: `Duration::MAX` here would make every
        // job maximally starved on exactly the boot with least to spare, and a
        // panicking subtraction would take the pond down.
        assert_eq!(
            elapsed_since(t, t + Duration::from_secs(90)),
            Duration::ZERO,
            "a stamp in the future must read as a recent run"
        );
    }

    /// THE DEFECT STEP 3 EXISTS FOR.
    ///
    /// `should_run` skips the interval floor entirely when `since_last_run` is
    /// `None`, because a job that has never run cannot be too soon. That is the
    /// right reading of "never" and the wrong reading of "ran a minute ago, in
    /// the process before this one" — so before the clock was durable, every
    /// restart handed all seven jobs a free pass through their own floors.
    #[tokio::test]
    async fn a_restart_does_not_hand_a_job_a_free_pass_through_its_interval_floor() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let a_minute_ago = Utc::now() - Duration::from_secs(60);
        let lane = InferenceLane::restored(
            HashMap::from([(LaneJob::Consolidation, a_minute_ago)]),
            None,
        );
        let _bell = lane.claim(LaneJob::Consolidation);

        // A day's floor, one minute since the last run. Asking registers the
        // cadence; the answer must be no.
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

    /// The control for the test above: with no stamp, the same job runs.
    ///
    /// Without this, that assertion would still pass if the floor were being
    /// enforced by something other than the restored clock — a disabled job, a
    /// busy slot, a gate that refuses everything.
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

    /// A restored job must not outrank a job that really has never run.
    ///
    /// `select_next` ranks `None` as `Duration::MAX`. Before the clock was
    /// durable, every job was `None` after a restart and the tie-break fell
    /// through to declaration order — so the lane's ordering was decided by the
    /// enum, not by need.
    #[tokio::test]
    async fn a_job_that_never_ran_still_outranks_one_restored_from_disk() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        // Titling is declared BEFORE memory extraction, so if the restored
        // stamp were ignored titling would win on declaration order and this
        // test would pass for the wrong reason. Restoring titling — the job
        // that would otherwise win — is what makes the assertion about the
        // clock.
        let lane = InferenceLane::restored(
            HashMap::from([(LaneJob::Titling, Utc::now() - Duration::from_secs(60))]),
            None,
        );
        let _t = lane.claim(LaneJob::Titling);
        let _m = lane.claim(LaneJob::MemoryExtraction);

        // Register both without running either, by asking while the slot is
        // held by a third job.
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

    /// Releasing the slot posts the run to the writer, once, naming the job.
    ///
    /// The send is in `Drop`, which is the only place that can see every path
    /// out of a job body. A version that recorded at the grant instead would
    /// stamp runs that then panicked, and one that recorded in the job bodies
    /// would miss every early return — the failure the guard already exists to
    /// prevent for the in-memory clock.
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

    /// A refused tick is not a run, and must not be written down as one.
    #[tokio::test]
    async fn a_tick_that_won_nothing_posts_nothing() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let lane = InferenceLane::restored(HashMap::new(), Some(tx));
        let _bell = lane.claim(LaneJob::Titling);
        let _other = lane.claim(LaneJob::Consolidation);

        let held = ask(&lane, LaneJob::Consolidation).await.expect("free slot");
        assert!(ask(&lane, LaneJob::Titling).await.is_none());
        // Only consolidation's own release should arrive, and only after it is
        // dropped -- nothing from the refusal.
        assert!(rx.try_recv().is_err(), "a refusal is not a run");
        drop(held);
        assert_eq!(rx.try_recv().unwrap().0, LaneJob::Consolidation);
    }

    /// A restored job reports its age before its loop has ticked once.
    ///
    /// Boot order is: wire the lane, then start the loops. In the window
    /// between, every job is unregistered — and reporting "never" for a job the
    /// log can date is the exact lie the durable clock was added to end, shown
    /// on the one screen somebody opens to ask why nothing is running.
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

    /// A lane with nowhere to write still keeps its own clock.
    ///
    /// Every test above this file's wiring uses `InferenceLane::new()`, and a
    /// pond whose log failed to open gets the same lane. Neither may be a lane
    /// that stops scheduling.
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

    /// Which gates a tick waives, by where it came from.
    ///
    /// The scheduled bell buys an EARLY LOOK and nothing else: it applies every
    /// gate exactly as the job's own timer does. Only a person waives them.
    #[test]
    fn only_a_person_waives_the_gates() {
        assert!(Tick::HandAsked.waives());
        assert!(!Tick::Poll.waives());
        assert!(
            !Tick::Scheduled.waives(),
            "a nudge from the lane must not read as somebody standing at the panel"
        );
    }

    /// THE DEFECT THIS STEP EXISTS FOR.
    ///
    /// A tick whose winner is not the caller used to compute the right answer
    /// and throw it away: no run, no reschedule, no retry, and the winner
    /// asleep on a timer up to fifteen minutes out. Measured on a real pond:
    /// 28 hand-wakes against 26 grants, because essentially every grant came
    /// from a person pressing a button.
    ///
    /// Now the loser rings the winner's SCHEDULED bell on its way out.
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

        // Consolidation has to be REGISTERED to be pickable -- `claim` only
        // records that a loop exists; the registry is written by `acquire`.
        // Asking while another job holds the slot registers it without running
        // it, so it keeps the never-run wait that makes it the winner below.
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

        // Now: consolidation never ran, extraction ran a moment ago. So the
        // lane names consolidation, and extraction -- asking on its own 60s
        // timer -- loses.
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

        // The winner's scheduled bell now holds a permit, so its own loop wakes
        // at once rather than sleeping out its poll.
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

    /// THE DISTINCTION THE COUNTERS EXIST TO MAKE.
    ///
    /// A snapshot cannot tell a job that is eligible and losing the tie-break
    /// from one that is switched off — `blocked_by` is `None` for BOTH, since
    /// losing is not a gate refusal. On a real pond that is the difference
    /// between a defect and a setting, and it took a log-file archaeology dig
    /// to answer it once.
    #[tokio::test]
    async fn losing_and_being_switched_off_are_different_numbers() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        let _w = lane.claim(LaneJob::Consolidation);
        let _l = lane.claim(LaneJob::MemoryExtraction);
        let _t = lane.claim(LaneJob::Titling);

        // Extraction runs once so it has a real wait; consolidation registers
        // without running, so it stays infinitely starved and wins from here.
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

        // Extraction loses three ticks in a row.
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
        // The instant says nothing is wrong, which is exactly the blind spot.
        assert_eq!(
            extraction.blocked_by, None,
            "losing is not a gate refusal, so the snapshot alone cannot see it"
        );
        assert_eq!(
            extraction.history.refused,
            [0, 0, 0, 0],
            "and no gate refused it either"
        );

        // The winner was nudged once per loss -- the measurement for step one.
        let consolidation = snapshot
            .jobs
            .iter()
            .find(|j| j.job == LaneJob::Consolidation)
            .expect("listed");
        assert_eq!(consolidation.history.nudged, 3);
    }

    /// A job refused by its own gate counts under that reason, and nowhere
    /// else. Without this the two paths could both land in `lost_to`.
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

    /// The control: a tick the caller WINS must not ring anybody. Without this,
    /// a nudge on every tick would be indistinguishable from the fix.
    #[tokio::test]
    async fn a_winning_tick_nudges_nobody() {
        let lane = InferenceLane::new();
        let _mine = lane.claim(LaneJob::Consolidation);
        let others = lane.claim(LaneJob::MemoryExtraction);

        let won = ask(&lane, LaneJob::Consolidation).await;
        assert!(won.is_some(), "the only registered asker wins");
        drop(won);

        // Nothing rang for extraction, so this must time out.
        let nudged = tokio::time::timeout(Duration::from_millis(300), async {
            wait_for_tick(Duration::from_secs(3600), &others).await
        })
        .await;
        assert!(nudged.is_err(), "a winning tick must not wake anybody else");
    }

    /// The registry poisoning the nudge would have amplified.
    ///
    /// One press of Run now used to write `floor: 0, idle: 0, exempt: true`
    /// into the SHARED registry, where it sat for up to that job's whole poll
    /// period — so every other job's tick then evaluated it as eligible in the
    /// middle of a conversation. The waiver is now a separate argument applied
    /// to the asker's own gate and never stored.
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

    /// A job whose loop never spawned -- no embedder, no orchestrator, a CLI
    /// process -- has nothing to wake, and the button must say so rather than
    /// reporting success into the void.
    #[tokio::test]
    async fn waking_a_job_with_no_loop_reports_it_rather_than_pretending() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        assert_eq!(
            lane.wake(LaneJob::MemoryExtraction).await,
            WakeOutcome::NotPresent
        );

        // Vacuity control for the assertion above: claiming is what changes the
        // answer, so if `wake` were hardcoded to `NotPresent` this would fail.
        let _bell = lane.claim(LaneJob::MemoryExtraction);
        assert_eq!(
            lane.wake(LaneJob::MemoryExtraction).await,
            WakeOutcome::Woken
        );
        // And only that job. A doorbell wired to every loop would make one
        // button run all six.
        assert_eq!(lane.wake(LaneJob::Titling).await, WakeOutcome::NotPresent);
    }

    /// The doorbell has to reach a loop that is already asleep on its poll, or
    /// the shortest wait a button could produce is the job's own cadence --
    /// fifteen minutes for the index sweep.
    #[tokio::test]
    async fn a_wake_cuts_short_a_poll_the_loop_is_already_asleep_on() {
        use pond_core::user_data::ports::lane_control::LaneControl;

        let lane = InferenceLane::new();
        let bell = lane.claim(LaneJob::Titling);

        // An hour, so a pass can only come from the doorbell.
        let ticking =
            tokio::spawn(async move { wait_for_tick(Duration::from_secs(3600), &bell).await });
        // Yield until the task is parked on the select; without this the notify
        // can land before there is a waiter and `notify_one`'s permit is what
        // saves the test rather than the mechanism under test.
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

    /// Exclusion is the lane's one hard guarantee, and a button must not buy
    /// its way past it. `Cadence::for_tick` waives the POLITENESS gates; the
    /// slot is not one of them.
    #[tokio::test]
    async fn a_hand_asked_job_still_cannot_take_a_slot_another_job_holds() {
        let lane = InferenceLane::new();
        let _bell = lane.claim(LaneJob::MemoryExtraction);

        let held = ask(&lane, LaneJob::Consolidation)
            .await
            .expect("the first asker wins the free slot");

        // Everything a hand-asked extraction tick would pass in: no floor, no
        // quiet, exempt from the activity gate. It still gets nothing.
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

    /// The watcher's whole point: not that something is running, but what.
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

        // Dropping the guard clears it, on the same Drop that records the run.
        // A job that returned early -- or panicked -- must not leave the
        // watcher saying it is still going.
        drop(held);
        let after = lane.snapshot().await;
        assert!(!after.slot_busy);
        assert_eq!(after.running, None, "the holder is cleared with the guard");
    }

    /// The status route's answer has to name every job, including the ones with
    /// no loop here -- a job missing from the list is a job nobody can see is
    /// missing.
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

    /// `present` and `registered` are different facts and the panel renders them
    /// differently: a loop that exists but has not ticked has told the lane
    /// nothing, and "no data" must not read as "switched off".
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

    /// `false` for the exemption: these tests are about the shared gate and the
    /// tie-break between jobs, both of which an exempt job skips entirely.
    async fn ask(lane: &InferenceLane, job: LaneJob) -> Option<LaneSlot<'_>> {
        lane.acquire(
            job,
            true,
            Cadence::new(Duration::ZERO, IDLE_THRESHOLD, false),
            // Never waived: these tests are about the shared gate and the
            // tie-break between jobs, both of which a waiver skips entirely.
            false,
            true,
            LONG_IDLE,
        )
        .await
    }

    #[tokio::test]
    async fn a_second_job_cannot_take_a_held_slot() {
        // The guarantee the pairwise stand-down checks were reaching for, now
        // enforced by the type rather than by each job remembering to look.
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
        // An early return in a job body drops the guard without calling finish.
        // The slot must come back even though the interval budget was not spent.
        //
        // Re-asks with the SAME job on purpose. Asking with a different one
        // would conflate two things: a job can also be refused because it lost
        // the decision, and with both jobs never-run they tie and the earlier
        // one wins — so a passing assertion there would say nothing about the
        // slot. The same job cannot lose to itself, which leaves the slot as
        // the only thing under test.
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
        // The starvation bug, in the shape it actually occurred: titling takes
        // the slot, finds no LLM provider configured, and returns early without
        // any explicit bookkeeping call.
        //
        // If that leaves it with `since_last_run: None` it counts as infinitely
        // starved and wins every subsequent tie forever, locking consolidation
        // out for the life of the process. The assertion is therefore about the
        // NEXT decision, not about any state the lane exposes.
        // The bailing job must be the EARLIER-declared one. Ties break by
        // declaration order, so if the later job bailed, the earlier one would
        // win the next tick regardless of whether the budget was spent, and the
        // assertion would hold with the bug fully present. The first version of
        // this test made exactly that mistake and passed against the bug.
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
    async fn a_job_registers_even_when_it_loses_so_others_can_see_it() {
        // Titling asks first and loses nothing (empty lane), but the point is
        // that its registration persists: consolidation's later tick must be
        // decided against a lane that knows titling exists.
        let lane = InferenceLane::new();
        drop(ask(&lane, LaneJob::Titling).await.expect("wins"));

        let registry = lane.registry.lock().await;
        assert!(registry.contains_key(&LaneJob::Titling));
    }
}
