//! Scheduling policy for background memory consolidation, which monopolises the inference slot.

use chrono::{DateTime, Utc};
use std::time::{Duration, Instant};

/// Quiet time before a consolidation may start; outlasts a natural pause in conversation.
pub const INACTIVITY_THRESHOLD_SECS: u64 = 15 * 60;

/// Below this many scoreable memories, there are too few duplicates to be worth consolidating.
pub const MIN_MEMORIES_TO_CONSOLIDATE: usize = 6;

/// Everything the gate needs to decide whether a run may start.
#[derive(Debug, Clone, Copy)]
pub struct GateInputs {
    /// Current value of `memory_consolidation_enabled`, re-read per tick.
    pub enabled: bool,
    /// Real user activity seen since process start: the "never on startup" guard.
    pub saw_activity_since_start: bool,
    /// How long since the most recent observed user activity.
    pub idle_for: Duration,
    /// How long the user must be quiet before a run may start.
    pub idle_threshold: Duration,
    /// Time since this process's previous run; `None` if it has not run yet.
    pub since_last_run: Option<Duration>,
    /// Minimum spacing between runs, from `memory_consolidation_interval_hours`.
    pub interval_floor: Duration,
}

/// Why a tick did not start a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `memory_consolidation_enabled` is currently false.
    Disabled,
    /// No user activity since this process started (the startup guard).
    NoActivitySinceStart,
    /// The user was active too recently.
    StillActive,
    /// A run happened less than `interval_floor` ago.
    IntervalFloor,
}

impl SkipReason {
    /// Short, stable label for structured logs.
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::Disabled => "disabled",
            SkipReason::NoActivitySinceStart => "no_activity_since_start",
            SkipReason::StillActive => "still_active",
            SkipReason::IntervalFloor => "interval_floor",
        }
    }
}

/// The gate's verdict for one scheduler tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    Run,
    Skip(SkipReason),
}

impl GateDecision {
    pub fn is_run(self) -> bool {
        matches!(self, GateDecision::Run)
    }
}

/// Decide whether a background consolidation run may start now.
pub fn should_run(inputs: GateInputs) -> GateDecision {
    if !inputs.enabled {
        return GateDecision::Skip(SkipReason::Disabled);
    }
    // Never on startup: an untouched process never consolidates, however long it has been up.
    if !inputs.saw_activity_since_start {
        return GateDecision::Skip(SkipReason::NoActivitySinceStart);
    }
    if inputs.idle_for < inputs.idle_threshold {
        return GateDecision::Skip(SkipReason::StillActive);
    }
    if let Some(since) = inputs.since_last_run {
        if since < inputs.interval_floor {
            return GateDecision::Skip(SkipReason::IntervalFloor);
        }
    }
    GateDecision::Run
}

/// Has user activity been seen since start? `in_process_at` must be strictly after `started_at`
/// so the boot stamp never counts; `db_activity` catches the out-of-process voice loop.
pub fn saw_activity_since_start(
    started_at: Instant,
    in_process_at: Instant,
    started_at_utc: DateTime<Utc>,
    db_activity: Option<DateTime<Utc>>,
) -> bool {
    in_process_at > started_at || db_activity.is_some_and(|at| at > started_at_utc)
}

/// Idle time since the more recent of the two sources; a future `db_activity` (skew) is ignored.
pub fn combined_idle_for(
    in_process_at: Instant,
    db_activity: Option<DateTime<Utc>>,
    now_utc: DateTime<Utc>,
) -> Duration {
    let in_process_idle = in_process_at.elapsed();
    match db_activity.and_then(|at| (now_utc - at).to_std().ok()) {
        Some(db_idle) => in_process_idle.min(db_idle),
        None => in_process_idle,
    }
}

/// Interval floor from hours, clamped to at least 1 h: a stored `0` would re-fire every tick.
pub fn interval_floor_from_hours(hours: u32) -> Duration {
    Duration::from_secs(u64::from(hours.max(1)) * 3600)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> GateInputs {
        GateInputs {
            enabled: true,
            saw_activity_since_start: true,
            idle_for: Duration::from_secs(INACTIVITY_THRESHOLD_SECS + 1),
            idle_threshold: Duration::from_secs(INACTIVITY_THRESHOLD_SECS),
            since_last_run: None,
            interval_floor: interval_floor_from_hours(24),
        }
    }

    #[test]
    fn runs_when_idle_after_real_activity() {
        assert_eq!(should_run(base()), GateDecision::Run);
    }

    #[test]
    fn disabled_never_runs() {
        let inputs = GateInputs {
            enabled: false,
            ..base()
        };
        assert_eq!(should_run(inputs), GateDecision::Skip(SkipReason::Disabled));
    }

    #[test]
    fn never_runs_on_startup_without_user_activity() {
        let inputs = GateInputs {
            saw_activity_since_start: false,
            idle_for: Duration::from_secs(86_400),
            ..base()
        };
        assert_eq!(
            should_run(inputs),
            GateDecision::Skip(SkipReason::NoActivitySinceStart)
        );
    }

    #[test]
    fn startup_guard_outranks_a_long_uptime() {
        let inputs = GateInputs {
            saw_activity_since_start: false,
            idle_for: Duration::from_secs(7 * 86_400),
            since_last_run: None,
            ..base()
        };
        assert!(!should_run(inputs).is_run());
    }

    #[test]
    fn does_not_run_while_user_is_active() {
        let inputs = GateInputs {
            idle_for: Duration::from_secs(60),
            ..base()
        };
        assert_eq!(
            should_run(inputs),
            GateDecision::Skip(SkipReason::StillActive)
        );
    }

    #[test]
    fn interval_floor_blocks_a_repeat_run() {
        let inputs = GateInputs {
            since_last_run: Some(Duration::from_secs(3600)),
            interval_floor: interval_floor_from_hours(24),
            ..base()
        };
        assert_eq!(
            should_run(inputs),
            GateDecision::Skip(SkipReason::IntervalFloor)
        );
    }

    #[test]
    fn interval_floor_allows_a_run_once_elapsed() {
        let inputs = GateInputs {
            since_last_run: Some(Duration::from_secs(24 * 3600 + 1)),
            interval_floor: interval_floor_from_hours(24),
            ..base()
        };
        assert_eq!(should_run(inputs), GateDecision::Run);
    }

    #[test]
    fn interval_floor_treats_zero_hours_as_one_hour() {
        assert_eq!(
            interval_floor_from_hours(0),
            Duration::from_secs(3600),
            "a stored 0 must not disable rate limiting"
        );
        let inputs = GateInputs {
            since_last_run: Some(Duration::from_secs(59 * 60)),
            interval_floor: interval_floor_from_hours(0),
            ..base()
        };
        assert_eq!(
            should_run(inputs),
            GateDecision::Skip(SkipReason::IntervalFloor)
        );
    }

    #[test]
    fn first_run_is_not_blocked_by_the_floor() {
        let inputs = GateInputs {
            since_last_run: None,
            ..base()
        };
        assert_eq!(should_run(inputs), GateDecision::Run);
    }

    // ── saw_activity_since_start ─────────────────────────────────────────────

    #[test]
    fn boot_stamped_clock_is_not_activity() {
        let in_process_at = Instant::now(); // stamped during wiring
        let started_at = Instant::now(); // scheduler baseline, captured after
        let started_at_utc = Utc::now();

        assert!(!saw_activity_since_start(
            started_at,
            in_process_at,
            started_at_utc,
            None
        ));
    }

    #[test]
    fn an_in_process_request_counts_as_activity() {
        let started_at = Instant::now();
        let started_at_utc = Utc::now();
        let in_process_at = started_at + Duration::from_millis(1);

        assert!(saw_activity_since_start(
            started_at,
            in_process_at,
            started_at_utc,
            None
        ));
    }

    #[test]
    fn an_out_of_process_voice_turn_counts_as_activity() {
        let in_process_at = Instant::now();
        let started_at = in_process_at + Duration::from_millis(1);
        let started_at_utc = Utc::now();
        let voice_turn_at = started_at_utc + chrono::Duration::seconds(5);

        assert!(saw_activity_since_start(
            started_at,
            in_process_at,
            started_at_utc,
            Some(voice_turn_at)
        ));
    }

    #[test]
    fn pre_existing_sessions_do_not_count_as_activity() {
        let in_process_at = Instant::now();
        let started_at = in_process_at + Duration::from_millis(1);
        let started_at_utc = Utc::now();
        // History from a previous run of the server.
        let old_session = started_at_utc - chrono::Duration::days(3);

        assert!(
            !saw_activity_since_start(started_at, in_process_at, started_at_utc, Some(old_session)),
            "restarting with existing history must not licence a run"
        );
    }

    // ── combined_idle_for ───────────────────────────────────────────────────

    #[test]
    fn idle_takes_the_more_recent_source() {
        let in_process_at = Instant::now() - Duration::from_secs(3600);
        let now = Utc::now();
        // Out-of-process voice turn 10s ago while HTTP has been quiet an hour.
        let db = Some(now - chrono::Duration::seconds(10));

        let idle = combined_idle_for(in_process_at, db, now);
        assert!(
            idle < Duration::from_secs(60),
            "a recent voice turn must dominate a stale in-process clock, got {idle:?}"
        );
    }

    #[test]
    fn idle_falls_back_to_the_in_process_clock_without_db_activity() {
        let in_process_at = Instant::now() - Duration::from_secs(120);
        let idle = combined_idle_for(in_process_at, None, Utc::now());
        assert!(idle >= Duration::from_secs(120));
    }

    #[test]
    fn a_future_db_timestamp_is_discarded_rather_than_trusted() {
        let in_process_at = Instant::now() - Duration::from_secs(120);
        let now = Utc::now();
        let skewed = Some(now + chrono::Duration::hours(1));

        let idle = combined_idle_for(in_process_at, skewed, now);
        assert!(
            idle >= Duration::from_secs(120),
            "clock skew must not be read as activity"
        );
    }
}
