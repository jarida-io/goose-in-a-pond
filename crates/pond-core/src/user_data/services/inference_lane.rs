//! One lane for every background job that spends inference.
//!
//! # Why this exists
//!
//! Inference is the scarcest resource on a pond. On a Jetson Orin Nano there is
//! exactly one model resident in GPU memory, decode is memory-bandwidth-bound,
//! and two background jobs running at once do not each take half as long — they
//! evict each other's KV cache and both get slower than either alone, while the
//! household waits behind them for its next answer.
//!
//! Before this module there was no lane: memory consolidation, session titling
//! and the proactive reviewer each ran their own `tokio::spawn` loop, each
//! re-derived the same [`consolidation_schedule`] gate from its own copy of the
//! inputs, and coordination between them was *pairwise and by hand*. The titling
//! loop carried a literal "stand down while consolidation is mid-run" check
//! against consolidation's cancel token. That is O(n²) checks in the number of
//! jobs, every one of them written by whoever added the newest job, and every
//! one of them a place to forget a direction: A yields to B, B never learns to
//! yield to A, and both run.
//!
//! This module replaces that with a single question asked once per tick — *which
//! one job may run now?* — whose answer is a single job by construction. Nothing
//! can run concurrently because [`select_next`] cannot return two things.
//!
//! # What belongs here and what does not
//!
//! Only jobs that **spend inference**. A periodic job that is pure SQL — memory
//! decay, log pruning, run-history cleanup — does not contend for the slot and
//! must NOT be put in the lane: it would sit behind an LLM call for no reason,
//! and idle-gating it would mean a busy household never gets its logs pruned.
//!
//! This module is also deliberately free of `tokio`, clocks and repositories: it
//! decides, and the caller acts. That is what makes the starvation and
//! mutual-exclusion properties testable in microseconds instead of via a
//! scheduler that has to be waited on.

use std::time::Duration;

use super::consolidation_schedule::{self, GateInputs, SkipReason};

/// A background job that spends inference.
///
/// Adding a variant is the whole registration step — [`select_next`] needs no
/// change, because the lane does not rank jobs by identity. See the module docs
/// for why a pure-SQL job does not belong here.
/// `Ord` is derived and therefore follows declaration order. That is not
/// decoration: [`select_next`] documents declaration order as its tie-break, and
/// a caller holding its jobs in a `HashMap` (as the server's registry does)
/// would otherwise hand them over in an arbitrary order and make ties resolve
/// differently between runs. Callers sort by this before deciding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LaneJob {
    /// Merge and score the memory store.
    Consolidation,
    /// Give conversations readable names.
    Titling,
    /// Look at recent household events and propose something.
    ProactiveReview,
    /// Keep each active conversation's rolling summary current.
    SummaryRefresh,
    /// Embed what the personal-context index cannot yet reach, and prune what it
    /// should no longer hold.
    ///
    /// Declared after [`SummaryRefresh`] on purpose. Order is only the tie-break,
    /// but the one tie worth deciding is this pair: the refresh WRITES rolling
    /// summaries and this job embeds them, so on an exact tie the summary should
    /// exist before something tries to index it.
    ///
    /// A pass is mostly SQL — adoption and orphan pruning — around an embedding
    /// step that is not. It is in the lane for that middle part: embedding is a
    /// forward pass through a second model, and the module's rule is about
    /// spending inference, not about how many lines do. Splitting the pass to
    /// keep the SQL out of the lane would break the adopt-embed-prune ordering
    /// that `run_index_maintenance` asserts, to save a few milliseconds of
    /// `COUNT(*)`.
    IndexMaintenance,
    /// Read one window of one conversation and decide what is worth
    /// remembering about the person in it.
    ///
    /// Declared LAST, and that is not a style choice: declaration order is the
    /// tie-break, and this is the job that should lose every tie it is in. It
    /// is the most expensive per pass (three model calls of ~2,240 tokens
    /// each), the least urgent (a conversation from last March does not get
    /// staler), and the only one whose work is safely resumable -- its cursor
    /// means a lost tick costs nothing but the tick. The summary refresh, by
    /// contrast, maintains the context the NEXT turn is answered from.
    ///
    /// It is in the lane rather than keeping its own loop for the obvious
    /// reason and one less obvious one: it spends the single inference slot,
    /// and it is the job most likely to be mid-call when somebody comes back.
    MemoryExtraction,
}

impl LaneJob {
    /// Short, stable label for structured logs and metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            LaneJob::Consolidation => "consolidation",
            LaneJob::Titling => "titling",
            LaneJob::ProactiveReview => "proactive_review",
            LaneJob::SummaryRefresh => "summary_refresh",
            LaneJob::IndexMaintenance => "index_maintenance",
            LaneJob::MemoryExtraction => "memory_extraction",
        }
    }
}

/// One job's own readiness, independent of the shared gate.
#[derive(Debug, Clone, Copy)]
pub struct JobState {
    pub job: LaneJob,
    /// This job's live enable toggle, re-read per tick so switching a feature
    /// off takes effect on the next tick rather than at the next restart.
    pub enabled: bool,
    /// Time since this job last ran in this process; `None` if it never has.
    pub since_last_run: Option<Duration>,
    /// The job's own minimum spacing. Consolidation's comes from
    /// `memory_consolidation_interval_hours`; a cheap bounded job like titling
    /// can set this to its poll period, which makes it "every tick it is
    /// eligible".
    pub interval_floor: Duration,
    /// How quiet it must be before THIS job may take the slot.
    ///
    /// Per-job rather than shared, and the summary refresh is why. Consolidation
    /// and titling are background chores: they want a long quiet (15 min) because
    /// nothing is lost by waiting for one. The rolling-summary refresh is not a
    /// chore — it maintains the context the NEXT turn will be answered from, so
    /// it is meant to run in the gaps between turns and uses a ~30s threshold.
    /// Forcing it to the chore threshold would starve it during exactly the
    /// conversation it exists to serve.
    ///
    /// This does not weaken exclusion: no two jobs can run at once regardless of
    /// their thresholds, because there is one slot. The threshold answers "may
    /// background work take the slot from a person right now?", which is a
    /// different question from "may two jobs run together?", and only the second
    /// has one right answer for every job.
    pub idle_threshold: Duration,
    /// Whether this job may run on a pond that has served no turn since boot.
    ///
    /// PER-JOB, and that is the whole point. `saw_activity_since_start` is one
    /// value for the lane, so a caller that relaxed it to let ITSELF run
    /// relaxed it for every other registered job at the same time -- and since
    /// a never-run job sorts as maximally starved and the tie-break is
    /// declaration order, the tick went to whichever job was declared first,
    /// whose own task then refused it. The lane deadlocked while looking busy.
    ///
    /// The exemption exists because "nobody has chatted" is not the same as
    /// "there is nothing to do": mail arrives from a connector, so the index
    /// has real work on a pond that has served no turn at all.
    pub exempt_from_activity_gate: bool,
}

/// Everything the lane needs for one tick.
///
/// The activity readings are shared because they describe the household, not a
/// job. What each job does with them — how much quiet it insists on — is its own
/// (`JobState::idle_threshold`).
#[derive(Debug, Clone, Copy)]
pub struct LaneInputs<'a> {
    /// The "never on startup" guard — see
    /// [`consolidation_schedule::saw_activity_since_start`].
    pub saw_activity_since_start: bool,
    /// Time since the most recent user activity, from either source.
    pub idle_for: Duration,
    /// The registered jobs. Order is the tie-break and nothing else.
    pub jobs: &'a [JobState],
}

/// The lane's verdict for one tick.
///
/// `Run` carries exactly one job. That is the mutual-exclusion guarantee, and it
/// is structural rather than a rule someone has to remember: there is no shape
/// of this type that names two jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneDecision {
    Run(LaneJob),
    /// Nothing ran, and why. When several jobs were skipped for different
    /// reasons this reports the one that blocked the *most* jobs, which is the
    /// answer to "why is nothing happening?".
    Idle(SkipReason),
}

impl LaneDecision {
    pub fn job(self) -> Option<LaneJob> {
        match self {
            LaneDecision::Run(job) => Some(job),
            LaneDecision::Idle(_) => None,
        }
    }
}

/// Pick the one job that may run this tick.
///
/// **Least-recently-run wins**, with declaration order as the tie-break. A job
/// that has never run counts as infinitely starved and therefore outranks every
/// job that has.
///
/// Not priority ordering, and the difference matters: titling is eligible every
/// five minutes while consolidation is eligible every few hours, so under a
/// fixed priority with titling above it, consolidation would lose every tick it
/// was ever eligible for and never run at all. Least-recently-run cannot starve
/// anything — a job that keeps losing keeps accumulating the very quantity the
/// comparison is on. Each job's own `interval_floor` still bounds how *often* it
/// can win, so fairness here does not mean "equally often".
pub fn select_next(inputs: LaneInputs<'_>) -> LaneDecision {
    let mut best: Option<(&JobState, Duration)> = None;
    // Tracks why jobs were skipped, so an idle lane can say something better
    // than "nothing to do". Ordered by how much a reader can act on it.
    let mut blocked: Option<SkipReason> = None;

    for state in inputs.jobs {
        let decision = consolidation_schedule::should_run(GateInputs {
            enabled: state.enabled,
            // The lane-wide observation, OR this one job's exemption. Never the
            // other way round: one job's exemption must not qualify the rest.
            saw_activity_since_start: inputs.saw_activity_since_start
                || state.exempt_from_activity_gate,
            idle_for: inputs.idle_for,
            idle_threshold: state.idle_threshold,
            since_last_run: state.since_last_run,
            interval_floor: state.interval_floor,
        });

        match decision {
            consolidation_schedule::GateDecision::Skip(reason) => {
                blocked = Some(match blocked {
                    None => reason,
                    Some(existing) => most_informative(existing, reason),
                });
            }
            consolidation_schedule::GateDecision::Run => {
                // `None` means never run, which is the most starved a job can
                // be — `Duration::MAX` is how that orders against real waits.
                let waited = state.since_last_run.unwrap_or(Duration::MAX);
                let wins = match best {
                    // Strictly greater, so an equal wait leaves the earlier
                    // declaration in place: the tie-break is order, not luck.
                    Some((_, best_waited)) => waited > best_waited,
                    None => true,
                };
                if wins {
                    best = Some((state, waited));
                }
            }
        }
    }

    match best {
        Some((state, _)) => LaneDecision::Run(state.job),
        // An empty lane is not "disabled", but reporting the reason of a job
        // that does not exist would be worse. Disabled is the honest default:
        // a lane with no jobs registered is off.
        None => LaneDecision::Idle(blocked.unwrap_or(SkipReason::Disabled)),
    }
}

/// Which of two skip reasons better explains an idle lane.
///
/// A reader asking "why is nothing running?" is best served by the condition
/// that is about the *whole lane* rather than one job's cadence: the household
/// being mid-conversation explains everything, whereas one job's interval floor
/// explains only that job.
fn most_informative(a: SkipReason, b: SkipReason) -> SkipReason {
    fn rank(r: SkipReason) -> u8 {
        match r {
            SkipReason::NoActivitySinceStart => 3,
            SkipReason::StillActive => 2,
            SkipReason::IntervalFloor => 1,
            SkipReason::Disabled => 0,
        }
    }
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE_THRESHOLD: Duration = Duration::from_secs(15 * 60);
    const LONG_IDLE: Duration = Duration::from_secs(60 * 60);

    fn job(j: LaneJob, since_last_run: Option<u64>, floor_secs: u64) -> JobState {
        JobState {
            job: j,
            enabled: true,
            since_last_run: since_last_run.map(Duration::from_secs),
            interval_floor: Duration::from_secs(floor_secs),
            idle_threshold: IDLE_THRESHOLD,
            exempt_from_activity_gate: false,
        }
    }

    /// One job's exemption must not qualify the others.
    ///
    /// Before this was per-job, the index sweep relaxed the lane-wide activity
    /// flag so IT could run on a pond nobody had chatted with. That relaxed the
    /// flag for every registered job, and on a fresh boot every job is
    /// never-run -- maximally starved -- so the tick went to whichever was
    /// declared first. `IndexMaintenance` is declared last, so it lost every
    /// tick to a job whose own task then refused the slot. Nothing ran, and
    /// nothing said so.
    #[test]
    fn an_exemption_belongs_to_one_job_and_does_not_qualify_the_rest() {
        let mut sweep = job(LaneJob::IndexMaintenance, None, 0);
        sweep.exempt_from_activity_gate = true;
        let jobs = [job(LaneJob::Consolidation, None, 0), sweep];

        let decision = select_next(LaneInputs {
            // Nobody has used this pond since boot.
            saw_activity_since_start: false,
            idle_for: LONG_IDLE,
            jobs: &jobs,
        });
        assert_eq!(
            decision,
            LaneDecision::Run(LaneJob::IndexMaintenance),
            "the exempt job must win, and must not have qualified consolidation"
        );
    }

    /// Without an exemption the gate still holds for everyone.
    #[test]
    fn no_job_runs_before_the_pond_has_been_used() {
        let jobs = [
            job(LaneJob::Consolidation, None, 0),
            job(LaneJob::IndexMaintenance, None, 0),
        ];
        let decision = select_next(LaneInputs {
            saw_activity_since_start: false,
            idle_for: LONG_IDLE,
            jobs: &jobs,
        });
        assert!(
            matches!(decision, LaneDecision::Idle(_)),
            "the startup guard must still hold when nothing is exempt"
        );
    }

    fn tick(jobs: &[JobState]) -> LaneDecision {
        select_next(LaneInputs {
            saw_activity_since_start: true,
            idle_for: LONG_IDLE,
            jobs,
        })
    }

    // ── The property the whole module exists for ───────────────────────────

    #[test]
    fn a_tick_can_never_start_two_jobs() {
        // Every job eligible, all wide open. Exactly one is chosen — this is the
        // replacement for the pairwise "stand down while X is mid-run" checks,
        // and it holds by the shape of the return type rather than by anyone
        // remembering to write the check.
        let jobs = [
            job(LaneJob::Consolidation, None, 0),
            job(LaneJob::Titling, None, 0),
            job(LaneJob::ProactiveReview, None, 0),
        ];
        assert!(tick(&jobs).job().is_some());
    }

    // ── The shared gate applies to the lane, not to one job ────────────────

    #[test]
    fn a_household_mid_conversation_blocks_every_job() {
        // The reason the gate is shared: any job taking the slot now is taking
        // it from the person typing.
        let jobs = [
            job(LaneJob::Consolidation, None, 0),
            job(LaneJob::Titling, None, 0),
        ];
        let decision = select_next(LaneInputs {
            saw_activity_since_start: true,
            idle_for: Duration::from_secs(30),
            jobs: &jobs,
        });
        assert_eq!(decision, LaneDecision::Idle(SkipReason::StillActive));
    }

    #[test]
    fn an_untouched_process_runs_nothing() {
        // "Never on startup", now inherited by every job at once rather than
        // re-derived per loop.
        let jobs = [job(LaneJob::Consolidation, None, 0)];
        let decision = select_next(LaneInputs {
            saw_activity_since_start: false,
            idle_for: LONG_IDLE,
            jobs: &jobs,
        });
        assert_eq!(
            decision,
            LaneDecision::Idle(SkipReason::NoActivitySinceStart)
        );
    }

    #[test]
    fn a_disabled_job_is_passed_over_not_run_late() {
        let jobs = [
            JobState {
                enabled: false,
                ..job(LaneJob::Consolidation, None, 0)
            },
            job(LaneJob::Titling, None, 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::Titling));
    }

    // ── Fairness: the reason this is not a priority queue ──────────────────

    #[test]
    fn the_longest_waiting_job_goes_first() {
        let jobs = [
            job(LaneJob::Consolidation, Some(100), 0),
            job(LaneJob::Titling, Some(900), 0),
            job(LaneJob::ProactiveReview, Some(400), 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::Titling));
    }

    #[test]
    fn a_job_that_has_never_run_outranks_every_job_that_has() {
        // `None` is infinitely starved. Without this a fresh job added to a
        // long-running pond would queue behind jobs that had run seconds ago.
        let jobs = [
            job(LaneJob::Consolidation, Some(86_400), 0),
            job(LaneJob::Titling, None, 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::Titling));
    }

    #[test]
    fn a_frequent_job_cannot_starve_a_rare_one() {
        // The concrete failure a priority queue would have: titling is eligible
        // every 5 minutes, consolidation every 6 hours. Simulate a day of ticks
        // and assert consolidation actually gets the slot.
        let mut titling_last: Option<u64> = Some(0);
        let mut consolidation_last: Option<u64> = Some(0);
        let mut consolidation_runs = 0;

        // One tick every 5 minutes for 24 hours.
        for tick_idx in 1..=288u64 {
            let now = tick_idx * 300;
            let jobs = [
                job(
                    LaneJob::Titling,
                    titling_last.map(|t| now - t),
                    300, // eligible every poll
                ),
                job(
                    LaneJob::Consolidation,
                    consolidation_last.map(|t| now - t),
                    6 * 3600,
                ),
            ];
            match tick(&jobs).job() {
                Some(LaneJob::Titling) => titling_last = Some(now),
                Some(LaneJob::Consolidation) => {
                    consolidation_last = Some(now);
                    consolidation_runs += 1;
                }
                _ => {}
            }
        }

        assert!(
            consolidation_runs >= 3,
            "consolidation was starved by titling — got {consolidation_runs} runs in 24h, \
             which is the exact failure a fixed priority ordering would produce"
        );
    }

    #[test]
    fn an_interval_floor_still_bounds_a_starved_job() {
        // Fairness must not override a job's own cadence: waiting longest does
        // not entitle a job to run before its floor has elapsed.
        let jobs = [
            job(LaneJob::Consolidation, Some(60), 6 * 3600),
            job(LaneJob::Titling, Some(10), 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::Titling));
    }

    #[test]
    fn equal_waits_break_by_declaration_order() {
        let jobs = [
            job(LaneJob::Titling, Some(500), 0),
            job(LaneJob::Consolidation, Some(500), 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::Titling));
    }

    /// The one tie in this lane whose direction is a real decision rather than
    /// an arbitrary one.
    ///
    /// `SummaryRefresh` WRITES rolling summaries; `IndexMaintenance` embeds them.
    /// Run the wrong way round on a tie and the sweep indexes the summary that
    /// existed a moment ago, then waits a full interval to notice the new one.
    /// Nothing breaks — the next pass repairs it — but the ordering is free, so
    /// it is worth being on the correct side of, and worth failing loudly if a
    /// later variant is inserted between them.
    #[test]
    fn a_summary_is_written_before_anything_tries_to_index_it() {
        assert!(
            LaneJob::SummaryRefresh < LaneJob::IndexMaintenance,
            "declaration order is the tie-break the runner sorts by, so this ordering IS the \
             policy: a variant added between these two silently reverses it"
        );

        // Presented in the order the runner produces, which sorts by `LaneJob`.
        let jobs = [
            job(LaneJob::SummaryRefresh, Some(500), 0),
            job(LaneJob::IndexMaintenance, Some(500), 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::SummaryRefresh));
    }

    /// Batch memory extraction is declared last, and must stay there.
    ///
    /// The tie-break is declaration order, so where this variant sits IS the
    /// policy for every tie it takes part in. Last is the correct place for
    /// three reasons that all point the same way: it is the most expensive pass
    /// in the lane, the least urgent (a conversation from last March does not
    /// get staler), and the only one that keeps a durable cursor -- so a tick it
    /// loses costs exactly that tick, whereas a tick the summary refresh loses
    /// is a tick the next turn is answered without its context.
    ///
    /// Inserting a variant after it silently reverses that, which is why this
    /// is asserted rather than left to the comment above.
    #[test]
    fn the_most_expensive_job_loses_every_tie() {
        assert!(
            LaneJob::IndexMaintenance < LaneJob::MemoryExtraction,
            "declaration order is the tie-break the runner sorts by, so this ordering IS the \
             policy: a variant added after MemoryExtraction silently reverses it"
        );

        // Presented in the order the runner produces, which sorts by `LaneJob`.
        let jobs = [
            job(LaneJob::IndexMaintenance, Some(500), 0),
            job(LaneJob::MemoryExtraction, Some(500), 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::IndexMaintenance));

        // Losing ties is not starving. The moment it has waited longer than the
        // other job, it wins outright -- which is the property that makes
        // "declared last" a cost ordering rather than a permanent refusal.
        let starved = [
            job(LaneJob::IndexMaintenance, Some(500), 0),
            job(LaneJob::MemoryExtraction, Some(9_000), 0),
        ];
        assert_eq!(tick(&starved), LaneDecision::Run(LaneJob::MemoryExtraction));
    }

    /// The index sweep is a lane citizen like any other, and the property that
    /// matters most is the one it would have broken by keeping its own loop:
    /// it cannot run while another job holds the slot.
    #[test]
    fn the_index_sweep_cannot_run_beside_another_job() {
        // Starved far longer than the other, so it wins the tick outright...
        let jobs = [
            job(LaneJob::Consolidation, Some(60), 0),
            job(LaneJob::IndexMaintenance, Some(9_000), 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::IndexMaintenance));

        // ...and winning is the whole grant. There is no shape of LaneDecision
        // that names two jobs, which is why the old bespoke loop -- which asked
        // nobody before embedding -- could contend with consolidation and this
        // cannot.
        assert_eq!(tick(&jobs).job(), Some(LaneJob::IndexMaintenance));
    }

    // ── Reporting ──────────────────────────────────────────────────────────

    #[test]
    fn an_idle_lane_reports_the_reason_that_explains_the_most() {
        // One job is merely waiting out its floor; the household is also active.
        // "Still active" explains the whole lane, so it is the one to report.
        let jobs = [
            job(LaneJob::Consolidation, Some(60), 6 * 3600),
            job(LaneJob::Titling, Some(10), 300),
        ];
        let decision = select_next(LaneInputs {
            saw_activity_since_start: true,
            idle_for: Duration::from_secs(5),
            jobs: &jobs,
        });
        assert_eq!(decision, LaneDecision::Idle(SkipReason::StillActive));
    }

    #[test]
    fn a_short_threshold_job_runs_in_a_gap_that_blocks_the_chores() {
        // The whole reason the threshold is per-job. The household paused for a
        // minute: far too short for consolidation to take the slot, but exactly
        // the gap the summary refresh is built for. A single shared threshold
        // would have starved it during the conversation it serves.
        let jobs = [
            job(LaneJob::Consolidation, None, 0),
            JobState {
                idle_threshold: Duration::from_secs(30),
                ..job(LaneJob::SummaryRefresh, None, 0)
            },
        ];
        let decision = select_next(LaneInputs {
            saw_activity_since_start: true,
            idle_for: Duration::from_secs(60),
            jobs: &jobs,
        });
        assert_eq!(decision, LaneDecision::Run(LaneJob::SummaryRefresh));
    }

    #[test]
    fn an_empty_lane_is_idle_rather_than_a_panic() {
        assert_eq!(tick(&[]), LaneDecision::Idle(SkipReason::Disabled));
    }
}
