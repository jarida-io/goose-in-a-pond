//! One lane for every background job that spends inference: at most one runs per tick.
//!
//! Concurrent jobs on the single resident model evict each other's KV cache and both slow down.
//! Pure-SQL jobs (decay, pruning) must stay out: behind the idle gate a busy pond never prunes.

use std::time::Duration;

use super::consolidation_schedule::{self, GateInputs, SkipReason};

/// A background job that spends inference. Declaration order is the derived `Ord` and
/// [`select_next`]'s tie-break; callers sort jobs by it (a `HashMap` would randomise ties).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LaneJob {
    /// Compact a conversation near the edge of its window while the household is quiet, so its
    /// next turn does not wait for the summary. First, to win ties: a returning user feels it.
    Compaction,
    /// Merge and score the memory store.
    Consolidation,
    /// Give conversations readable names.
    Titling,
    /// Run the next step of a long task a member started. Before the pond's own proposals: a
    /// member asked for this work.
    LongTaskStep,
    /// Look at recent household events and propose something.
    ProactiveReview,
    /// Keep each active conversation's rolling summary current.
    SummaryRefresh,
    /// Embed what the personal-context index lacks and prune what it should drop. Declared after
    /// `SummaryRefresh` so on a tie a summary is written before it is indexed.
    IndexMaintenance,
    /// Mine one window of one conversation for what to remember about its person.
    /// Declared late to lose ties: the costliest pass, not urgent, and resumable by its cursor.
    MemoryExtraction,
    /// Compose questions out of the household's own memories.
    /// Declared after `MemoryExtraction` because it reads what extraction writes.
    SuggestionGeneration,
}

impl LaneJob {
    /// Every job, in declaration (tie-break) order; `every_job_is_in_all` catches omissions.
    pub const ALL: &'static [LaneJob] = &[
        LaneJob::Compaction,
        LaneJob::Consolidation,
        LaneJob::Titling,
        LaneJob::LongTaskStep,
        LaneJob::ProactiveReview,
        LaneJob::SummaryRefresh,
        LaneJob::IndexMaintenance,
        LaneJob::MemoryExtraction,
        LaneJob::SuggestionGeneration,
    ];

    /// Stable label for logs, metrics and `POST /lane/jobs/{job}/run`; renaming breaks scripts.
    pub fn as_str(self) -> &'static str {
        match self {
            LaneJob::Compaction => "compaction",
            LaneJob::Consolidation => "consolidation",
            LaneJob::Titling => "titling",
            LaneJob::LongTaskStep => "long_task_step",
            LaneJob::ProactiveReview => "proactive_review",
            LaneJob::SummaryRefresh => "summary_refresh",
            LaneJob::IndexMaintenance => "index_maintenance",
            LaneJob::MemoryExtraction => "memory_extraction",
            LaneJob::SuggestionGeneration => "suggestion_generation",
        }
    }

    /// What a household calls it. The panel shows this; the wire never does.
    pub fn title(self) -> &'static str {
        match self {
            LaneJob::Compaction => "Make room in long conversations",
            LaneJob::Consolidation => "Tidy the memory store",
            LaneJob::Titling => "Name conversations",
            LaneJob::LongTaskStep => "Work on long tasks",
            LaneJob::ProactiveReview => "Look for something to suggest",
            LaneJob::SummaryRefresh => "Refresh conversation summaries",
            LaneJob::IndexMaintenance => "Maintain the search index",
            LaneJob::MemoryExtraction => "Read conversations for memories",
            LaneJob::SuggestionGeneration => "Think of things to suggest",
        }
    }

    /// Parse a wire name back; `None` otherwise, so an unknown job in a URL is a 404.
    pub fn from_wire(name: &str) -> Option<LaneJob> {
        LaneJob::ALL.iter().copied().find(|j| j.as_str() == name)
    }
}

/// One job's own readiness, independent of the shared gate.
#[derive(Debug, Clone, Copy)]
pub struct JobState {
    pub job: LaneJob,
    /// This job's live enable toggle, re-read per tick so it applies without a restart.
    pub enabled: bool,
    /// Time since this job last ran in this process; `None` if it never has.
    pub since_last_run: Option<Duration>,
    /// Minimum spacing between this job's runs; its poll period here means "whenever eligible".
    pub interval_floor: Duration,
    /// How quiet it must be before THIS job may take the slot. Per-job so the summary refresh
    /// (~30 s) can run between turns while chores wait out a long quiet; exclusion is unaffected.
    pub idle_threshold: Duration,
    /// Whether this job may run on a pond that has served no turn since boot (e.g. indexing
    /// connector mail). Per-job: relaxing the lane-wide flag instead deadlocks the lane.
    pub exempt_from_activity_gate: bool,
}

/// Everything the lane needs for one tick.
#[derive(Debug, Clone, Copy)]
pub struct LaneInputs<'a> {
    /// The "never on startup" guard; see [`consolidation_schedule::saw_activity_since_start`].
    pub saw_activity_since_start: bool,
    /// Time since the most recent user activity, from either source.
    pub idle_for: Duration,
    /// The registered jobs. Order is the tie-break and nothing else.
    pub jobs: &'a [JobState],
    /// The job polling now, or `None` for a status read (which must not bias the answer).
    /// Wins exact ties, so an asleep never-run job can't take ticks it would never use.
    pub asking: Option<LaneJob>,
}

/// The lane's verdict for one tick; `Run` holding one job is the mutual-exclusion guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneDecision {
    Run(LaneJob),
    /// Nothing ran; carries the most lane-wide skip reason (see `most_informative`).
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

/// How long a chore may wait behind a member's long task before it gets one turn anyway. Each such
/// turn costs the task its slot for a minute or so, and a deep-research task runs about 20.
pub const CHORE_PATIENCE: Duration = Duration::from_secs(30 * 60);

/// Who goes first between two jobs that may run, before their waits are compared: a compaction a
/// returning member waits on, then a member's long task, then the pond's own chores, which keep
/// their order among themselves. A chore that has waited [`CHORE_PATIENCE`] since it last ran goes
/// before the long task once, so a long line of tasks never starves it.
fn precedence(a: &JobState, b: &JobState) -> std::cmp::Ordering {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let starved = |s: &JobState| s.since_last_run.is_some_and(|d| d >= CHORE_PATIENCE);
    match (a.job, b.job) {
        (LaneJob::Compaction, LaneJob::Compaction) => Equal,
        (LaneJob::Compaction, _) => Less,
        (_, LaneJob::Compaction) => Greater,
        (LaneJob::LongTaskStep, LaneJob::LongTaskStep) => Equal,
        (LaneJob::LongTaskStep, _) => {
            if starved(b) {
                Greater
            } else {
                Less
            }
        }
        (_, LaneJob::LongTaskStep) => {
            if starved(a) {
                Less
            } else {
                Greater
            }
        }
        _ => Equal,
    }
}

/// Pick the one job that may run this tick: by [`precedence`], then least-recently-run (never-run
/// beats all), ties in declaration order. Not a priority queue, which would starve rare jobs.
pub fn select_next(inputs: LaneInputs<'_>) -> LaneDecision {
    let mut best: Option<(&JobState, Duration)> = None;
    let mut blocked: Option<SkipReason> = None;

    for state in inputs.jobs {
        let decision = consolidation_schedule::should_run(GateInputs {
            enabled: state.enabled,
            // Per-job OR: one job's exemption must not qualify the rest.
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
                let waited = state.since_last_run.unwrap_or(Duration::MAX);
                let wins = match best {
                    Some((leader, best_waited)) => match precedence(state, leader) {
                        std::cmp::Ordering::Less => true,
                        std::cmp::Ordering::Greater => false,
                        // Equal waits go to the asker, else stay with the earlier-declared job.
                        std::cmp::Ordering::Equal => {
                            waited > best_waited
                                || (waited == best_waited && inputs.asking == Some(state.job))
                        }
                    },
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
        // A lane with no jobs registered is off.
        None => LaneDecision::Idle(blocked.unwrap_or(SkipReason::Disabled)),
    }
}

/// Which of two skip reasons better explains an idle lane: lane-wide beats one job's cadence.
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

/// Quiet before a compaction pass for the conversation the household is in, when that
/// conversation's next turn would start by compacting it anyway: enough for the end of an answer
/// (its last sentences sent to speech) to clear the model first. That turn waits for the pass
/// rather than stopping it.
pub const COMPACTION_QUIET_FOR_ITS_TURN: Duration = Duration::from_secs(5);
/// Quiet before a pass that only gets ahead of the conversation the household is in, so a pause
/// in a conversation is not mistaken for its end.
pub const COMPACTION_QUIET: Duration = Duration::from_secs(30);
/// Quiet before a pass for any other conversation. Its summary call sets aside the conversation
/// being talked in, whose next turn then pays to get it back, and that next turn stops the pass
/// anyway. Measured on a Jetson Orin, such a pass started 5 s after every turn of the current
/// conversation, was stopped by the next one, and cost each of them 6-13 s more to the first
/// token. Two minutes is longer than the pauses between turns there (14-19 s, at most 108 s).
pub const COMPACTION_QUIET_FOR_ANOTHER: Duration = Duration::from_secs(120);
/// A pass stopped because someone came back needs twice the quiet next time, doubling at most
/// this many times, so a conversation that keeps being interrupted stops costing the one in use.
pub const COMPACTION_QUIET_MAX_DOUBLINGS: u32 = 4;

/// How long the household must be quiet before a compaction pass for one conversation starts.
/// `in_use`: it is the conversation of the latest turn. `its_turn_compacts`: goose would compact
/// it at the start of its next turn. `times_stopped`: passes for it stopped since it last
/// compacted.
pub fn compaction_quiet(in_use: bool, its_turn_compacts: bool, times_stopped: u32) -> Duration {
    let base = match (in_use, its_turn_compacts) {
        (true, true) => COMPACTION_QUIET_FOR_ITS_TURN,
        (true, false) => COMPACTION_QUIET,
        (false, _) => COMPACTION_QUIET_FOR_ANOTHER,
    };
    base * (1u32 << times_stopped.min(COMPACTION_QUIET_MAX_DOUBLINGS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_conversation_in_use_is_compacted_after_a_short_quiet() {
        assert_eq!(compaction_quiet(true, true, 0), Duration::from_secs(5));
        assert_eq!(compaction_quiet(true, false, 0), Duration::from_secs(30));
        assert_eq!(
            compaction_quiet(false, true, 0),
            Duration::from_secs(120),
            "another conversation waits out the pauses between turns, even one that would \
             compact on its next turn"
        );
        assert_eq!(compaction_quiet(false, false, 0), Duration::from_secs(120));
    }

    #[test]
    fn a_stopped_pass_waits_longer_each_time_up_to_a_cap() {
        assert_eq!(compaction_quiet(false, true, 1), Duration::from_secs(240));
        assert_eq!(compaction_quiet(false, true, 2), Duration::from_secs(480));
        assert_eq!(compaction_quiet(false, true, 4), Duration::from_secs(1920));
        assert_eq!(
            compaction_quiet(false, true, 40),
            Duration::from_secs(1920),
            "the doubling stops, and never overflows"
        );
        assert_eq!(compaction_quiet(true, true, 1), Duration::from_secs(10));
    }

    #[test]
    fn an_asleep_job_does_not_take_a_tick_from_the_job_that_is_asking() {
        let jobs = [
            // Both have never run, so both wait `Duration::MAX`.
            job(LaneJob::IndexMaintenance, None, 60),
            job(LaneJob::MemoryExtraction, None, 60),
        ];

        // Extraction asks; declared after the sleeper, it wins only through `asking`.
        assert_eq!(
            select_next(LaneInputs {
                saw_activity_since_start: true,
                idle_for: LONG_IDLE,
                jobs: &jobs,
                asking: Some(LaneJob::MemoryExtraction),
            }),
            LaneDecision::Run(LaneJob::MemoryExtraction),
        );

        // Control: the rule favours the asker, not extraction.
        assert_eq!(
            select_next(LaneInputs {
                saw_activity_since_start: true,
                idle_for: LONG_IDLE,
                jobs: &jobs,
                asking: Some(LaneJob::IndexMaintenance),
            }),
            LaneDecision::Run(LaneJob::IndexMaintenance),
        );
    }

    #[test]
    fn asking_breaks_a_tie_and_never_beats_a_longer_wait() {
        let jobs = [
            // Never run: infinitely starved.
            job(LaneJob::Consolidation, None, 60),
            // Ran a second ago, and asking.
            job(LaneJob::MemoryExtraction, Some(1), 0),
        ];

        assert_eq!(
            select_next(LaneInputs {
                saw_activity_since_start: true,
                idle_for: LONG_IDLE,
                jobs: &jobs,
                asking: Some(LaneJob::MemoryExtraction),
            }),
            LaneDecision::Run(LaneJob::Consolidation),
            "the job that has waited longer still goes first",
        );
    }

    #[test]
    fn a_read_with_nobody_asking_still_breaks_ties_by_declaration_order() {
        let jobs = [
            job(LaneJob::IndexMaintenance, None, 60),
            job(LaneJob::MemoryExtraction, None, 60),
        ];

        assert_eq!(
            select_next(LaneInputs {
                saw_activity_since_start: true,
                idle_for: LONG_IDLE,
                jobs: &jobs,
                asking: None,
            }),
            LaneDecision::Run(LaneJob::IndexMaintenance),
        );
    }

    /// The compiler checks `as_str`'s match for new variants, but nothing else checks `ALL`.
    #[test]
    fn every_job_is_in_all() {
        // Named one by one: a loop over `ALL` against `ALL` passes on an empty slice.
        for job in [
            LaneJob::Compaction,
            LaneJob::Consolidation,
            LaneJob::Titling,
            LaneJob::LongTaskStep,
            LaneJob::ProactiveReview,
            LaneJob::SummaryRefresh,
            LaneJob::IndexMaintenance,
            LaneJob::MemoryExtraction,
            LaneJob::SuggestionGeneration,
        ] {
            assert!(
                LaneJob::ALL.contains(&job),
                "{} missing from ALL",
                job.as_str()
            );
        }
        assert_eq!(LaneJob::ALL.len(), 9, "a job was added or removed");
    }

    #[test]
    fn all_is_in_the_order_ties_break() {
        let mut sorted = LaneJob::ALL.to_vec();
        sorted.sort();
        assert_eq!(sorted.as_slice(), LaneJob::ALL, "ALL is not in Ord order");
    }

    #[test]
    fn wire_names_round_trip_and_nothing_else_parses() {
        for &job in LaneJob::ALL {
            assert_eq!(LaneJob::from_wire(job.as_str()), Some(job));
        }
        for bogus in ["", "Consolidation", "memory-extraction", "titling ", "nope"] {
            assert_eq!(
                LaneJob::from_wire(bogus),
                None,
                "{bogus:?} should not parse"
            );
        }
    }

    #[test]
    fn names_and_titles_are_unique() {
        let mut wire: Vec<&str> = LaneJob::ALL.iter().map(|j| j.as_str()).collect();
        wire.sort_unstable();
        let before = wire.len();
        wire.dedup();
        assert_eq!(wire.len(), before, "two jobs share a wire name");

        let mut titles: Vec<&str> = LaneJob::ALL.iter().map(|j| j.title()).collect();
        titles.sort_unstable();
        let before = titles.len();
        titles.dedup();
        assert_eq!(titles.len(), before, "two jobs share a title");
    }

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

    #[test]
    fn a_members_long_task_goes_before_the_ponds_chores_but_none_starves() {
        let pick = |jobs: &[JobState], asking: LaneJob| {
            select_next(LaneInputs {
                saw_activity_since_start: true,
                idle_for: LONG_IDLE,
                jobs,
                asking: Some(asking),
            })
        };
        // The task just ran a step; the chores have waited minutes, and once would have won.
        let step = job(LaneJob::LongTaskStep, Some(2), 0);
        let chores = [
            job(LaneJob::MemoryExtraction, Some(600), 0),
            job(LaneJob::SummaryRefresh, Some(300), 0),
            job(LaneJob::SuggestionGeneration, None, 0),
        ];
        let mut jobs = chores.to_vec();
        jobs.push(step.clone());
        assert_eq!(
            pick(&jobs, LaneJob::MemoryExtraction),
            LaneDecision::Run(LaneJob::LongTaskStep)
        );

        let mut with_compaction = jobs.clone();
        with_compaction.push(job(LaneJob::Compaction, Some(1), 0));
        assert_eq!(
            pick(&with_compaction, LaneJob::LongTaskStep),
            LaneDecision::Run(LaneJob::Compaction),
            "a returning member waits on a compaction"
        );

        let mut starved = vec![job(
            LaneJob::MemoryExtraction,
            Some(CHORE_PATIENCE.as_secs()),
            0,
        )];
        starved.push(step);
        assert_eq!(
            pick(&starved, LaneJob::LongTaskStep),
            LaneDecision::Run(LaneJob::MemoryExtraction),
            "a chore that has waited its patience gets one turn"
        );

        assert_eq!(
            pick(&chores, LaneJob::SummaryRefresh),
            LaneDecision::Run(LaneJob::SuggestionGeneration),
            "with no task, the chores keep their own order"
        );
    }

    #[test]
    fn an_exemption_belongs_to_one_job_and_does_not_qualify_the_rest() {
        let mut sweep = job(LaneJob::IndexMaintenance, None, 0);
        sweep.exempt_from_activity_gate = true;
        let jobs = [job(LaneJob::Consolidation, None, 0), sweep];

        let decision = select_next(LaneInputs {
            saw_activity_since_start: false,
            idle_for: LONG_IDLE,
            jobs: &jobs,
            asking: None,
        });
        assert_eq!(
            decision,
            LaneDecision::Run(LaneJob::IndexMaintenance),
            "the exempt job must win, and must not have qualified consolidation"
        );
    }

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
            asking: None,
        });
        assert!(
            matches!(decision, LaneDecision::Idle(_)),
            "the startup guard must still hold when nothing is exempt"
        );
    }

    /// `asking: None`, so ties break by declaration order.
    fn tick(jobs: &[JobState]) -> LaneDecision {
        select_next(LaneInputs {
            saw_activity_since_start: true,
            idle_for: LONG_IDLE,
            jobs,
            asking: None,
        })
    }

    // ── Mutual exclusion ───────────────────────────────────────────────────

    #[test]
    fn a_tick_can_never_start_two_jobs() {
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
        let jobs = [
            job(LaneJob::Consolidation, None, 0),
            job(LaneJob::Titling, None, 0),
        ];
        let decision = select_next(LaneInputs {
            saw_activity_since_start: true,
            idle_for: Duration::from_secs(30),
            jobs: &jobs,
            asking: None,
        });
        assert_eq!(decision, LaneDecision::Idle(SkipReason::StillActive));
    }

    #[test]
    fn an_untouched_process_runs_nothing() {
        let jobs = [job(LaneJob::Consolidation, None, 0)];
        let decision = select_next(LaneInputs {
            saw_activity_since_start: false,
            idle_for: LONG_IDLE,
            jobs: &jobs,
            asking: None,
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

    // ── Fairness ───────────────────────────────────────────────────────────

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
        let jobs = [
            job(LaneJob::Consolidation, Some(86_400), 0),
            job(LaneJob::Titling, None, 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::Titling));
    }

    #[test]
    fn a_frequent_job_cannot_starve_a_rare_one() {
        // Titling is eligible every 5 min, consolidation every 6 h.
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

        // Losing ties is not starving: a longer wait wins outright.
        let starved = [
            job(LaneJob::IndexMaintenance, Some(500), 0),
            job(LaneJob::MemoryExtraction, Some(9_000), 0),
        ];
        assert_eq!(tick(&starved), LaneDecision::Run(LaneJob::MemoryExtraction));
    }

    #[test]
    fn the_index_sweep_cannot_run_beside_another_job() {
        // Starved far longer than the other, so it wins the tick outright...
        let jobs = [
            job(LaneJob::Consolidation, Some(60), 0),
            job(LaneJob::IndexMaintenance, Some(9_000), 0),
        ];
        assert_eq!(tick(&jobs), LaneDecision::Run(LaneJob::IndexMaintenance));

        // ...and winning is the whole grant: `LaneDecision` cannot name two jobs.
        assert_eq!(tick(&jobs).job(), Some(LaneJob::IndexMaintenance));
    }

    // ── Reporting ──────────────────────────────────────────────────────────

    #[test]
    fn an_idle_lane_reports_the_reason_that_explains_the_most() {
        // One job waits out its floor, but the household is active: the lane-wide reason wins.
        let jobs = [
            job(LaneJob::Consolidation, Some(60), 6 * 3600),
            job(LaneJob::Titling, Some(10), 300),
        ];
        let decision = select_next(LaneInputs {
            saw_activity_since_start: true,
            idle_for: Duration::from_secs(5),
            jobs: &jobs,
            asking: None,
        });
        assert_eq!(decision, LaneDecision::Idle(SkipReason::StillActive));
    }

    #[test]
    fn a_short_threshold_job_runs_in_a_gap_that_blocks_the_chores() {
        // A one-minute pause: too short for consolidation, enough for the summary refresh.
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
            asking: None,
        });
        assert_eq!(decision, LaneDecision::Run(LaneJob::SummaryRefresh));
    }

    #[test]
    fn an_empty_lane_is_idle_rather_than_a_panic() {
        assert_eq!(tick(&[]), LaneDecision::Idle(SkipReason::Disabled));
    }
}
