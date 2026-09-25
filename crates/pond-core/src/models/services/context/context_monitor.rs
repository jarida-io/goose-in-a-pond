//! Per-session context fill rate, to warn before the "context cliff" on small (3K-8K) windows.

use std::collections::HashMap;
use std::sync::Mutex;

/// Growth samples kept per session for the rolling tokens-per-turn average.
const MAX_GROWTH_SAMPLES: usize = 10;

/// Utilization percentage above which a warning message is emitted.
const WARNING_THRESHOLD_PCT: f32 = 60.0;

/// Utilization percentage above which compaction should be triggered.
const COMPACT_THRESHOLD_PCT: f32 = 75.0;

/// Compact when fewer turns than this remain, whatever the utilization.
const MIN_TURNS_REMAINING: u32 = 3;

/// Recorded turns between compaction passes; deliberately equal to [`MIN_TURNS_REMAINING`].
/// `should_compact` stays true past 75%, so ungated it would summarise every turn.
const COMPACTION_COOLDOWN_TURNS: u32 = 3;

#[derive(Debug, Clone)]
pub struct ContextState {
    pub estimated_tokens: u32,
    pub turns: u32,
    pub context_limit: u32,
    /// Per-turn token growth, newest last; at most [`MAX_GROWTH_SAMPLES`] entries.
    pub growth_rates: Vec<u32>,
    /// `turns` at the last claimed compaction pass; drives the cooldown.
    pub turns_at_last_compaction: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ContextHealth {
    /// Percentage of the context window currently consumed (0.0 - 100.0).
    pub utilization_pct: f32,
    /// Average tokens added per turn (rolling window).
    pub avg_growth_rate: u32,
    /// At the rolling-average growth rate; `u32::MAX` when growth is zero.
    pub estimated_turns_remaining: u32,
    /// True when utilization > 75% or fewer than 3 turns remain.
    pub should_compact: bool,
    /// Human-readable warning message, present when `utilization_pct > 60%`.
    pub warning: Option<String>,
}

/// The caller gates every call on `context_monitor_enabled`; the monitor never checks it.
pub struct ContextMonitor {
    session_contexts: Mutex<HashMap<String, ContextState>>,
}

impl ContextMonitor {
    pub fn new() -> Self {
        Self {
            session_contexts: Mutex::new(HashMap::new()),
        }
    }

    /// Record a completed turn; `estimated_tokens` is the session's running total, not a delta.
    pub fn record_turn(&self, session_id: &str, estimated_tokens: u32, context_limit: u32) {
        let mut sessions = self
            .session_contexts
            .lock()
            .expect("context monitor lock poisoned");

        let state = sessions
            .entry(session_id.to_string())
            .or_insert_with(|| ContextState {
                estimated_tokens: 0,
                turns: 0,
                context_limit,
                growth_rates: Vec::with_capacity(MAX_GROWTH_SAMPLES),
                turns_at_last_compaction: None,
            });

        let growth = estimated_tokens.saturating_sub(state.estimated_tokens);

        state.estimated_tokens = estimated_tokens;
        state.context_limit = context_limit;
        state.turns += 1;

        if state.growth_rates.len() >= MAX_GROWTH_SAMPLES {
            state.growth_rates.remove(0);
        }
        state.growth_rates.push(growth);
    }

    /// A zero-state snapshot when the session has no recorded turns.
    pub fn check_context_health(&self, session_id: &str) -> ContextHealth {
        let sessions = self
            .session_contexts
            .lock()
            .expect("context monitor lock poisoned");

        match sessions.get(session_id) {
            Some(s) => health_of(s),
            None => ContextHealth {
                utilization_pct: 0.0,
                avg_growth_rate: 0,
                estimated_turns_remaining: u32::MAX,
                should_compact: false,
                warning: None,
            },
        }
    }

    /// Claim one compaction pass, or decline. Pressure, cooldown and exclusivity are checked
    /// under one lock; the cooldown is stamped on the claim, not on completion.
    pub fn claim_compaction(&self, session_id: &str) -> bool {
        let mut sessions = self
            .session_contexts
            .lock()
            .expect("context monitor lock poisoned");

        let Some(state) = sessions.get_mut(session_id) else {
            return false;
        };

        if !health_of(state).should_compact {
            return false;
        }

        if let Some(last) = state.turns_at_last_compaction {
            if state.turns.saturating_sub(last) < COMPACTION_COOLDOWN_TURNS {
                return false;
            }
        }

        state.turns_at_last_compaction = Some(state.turns);
        true
    }

    /// Claim a pass the user asked for. Skips the cooldown check but still stamps it, so the
    /// manual and automatic axes can't double-spend; pressure is still required.
    pub fn claim_manual_compaction(&self, session_id: &str) -> bool {
        let mut sessions = self
            .session_contexts
            .lock()
            .expect("context monitor lock poisoned");

        let Some(state) = sessions.get_mut(session_id) else {
            return false;
        };

        if !health_of(state).should_compact {
            return false;
        }

        state.turns_at_last_compaction = Some(state.turns);
        true
    }

    /// Record a finished pass: drops only the stale growth window. Not `reset_session`, which
    /// would clear the cooldown stamp and let the pass fire again at once.
    pub fn note_compacted(&self, session_id: &str) {
        let mut sessions = self
            .session_contexts
            .lock()
            .expect("context monitor lock poisoned");
        if let Some(state) = sessions.get_mut(session_id) {
            state.growth_rates.clear();
        }
    }

    /// Call when the session is deleted, or a reused id inherits its predecessor's history.
    pub fn reset_session(&self, session_id: &str) {
        let mut sessions = self
            .session_contexts
            .lock()
            .expect("context monitor lock poisoned");
        sessions.remove(session_id);
    }
}

/// Shared by reporting and claiming, so the pressure they see cannot drift apart.
fn health_of(state: &ContextState) -> ContextHealth {
    let utilization_pct = if state.context_limit == 0 {
        0.0
    } else {
        (state.estimated_tokens as f32 / state.context_limit as f32) * 100.0
    };

    let avg_growth_rate = if state.growth_rates.is_empty() {
        0
    } else {
        let sum: u32 = state.growth_rates.iter().sum();
        sum / state.growth_rates.len() as u32
    };

    let estimated_turns_remaining = if avg_growth_rate == 0 {
        u32::MAX
    } else {
        let remaining_tokens = state.context_limit.saturating_sub(state.estimated_tokens);
        remaining_tokens / avg_growth_rate
    };

    let should_compact = utilization_pct > COMPACT_THRESHOLD_PCT
        || (avg_growth_rate > 0 && estimated_turns_remaining < MIN_TURNS_REMAINING);

    let warning = if utilization_pct > WARNING_THRESHOLD_PCT {
        Some(format!(
            "Context window {:.0}% full ({}/{} tokens). ~{} turns remaining.",
            utilization_pct,
            state.estimated_tokens,
            state.context_limit,
            if estimated_turns_remaining == u32::MAX {
                "unlimited".to_string()
            } else {
                estimated_turns_remaining.to_string()
            },
        ))
    } else {
        None
    };

    ContextHealth {
        utilization_pct,
        avg_growth_rate,
        estimated_turns_remaining,
        should_compact,
        warning,
    }
}

impl Default for ContextMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_session_has_zero_utilization() {
        let monitor = ContextMonitor::new();
        let health = monitor.check_context_health("session-1");
        assert_eq!(health.utilization_pct, 0.0);
        assert_eq!(health.avg_growth_rate, 0);
        assert_eq!(health.estimated_turns_remaining, u32::MAX);
        assert!(!health.should_compact);
        assert!(health.warning.is_none());
    }

    #[test]
    fn utilization_increases_after_turns() {
        let monitor = ContextMonitor::new();

        monitor.record_turn("s1", 500, 4096);
        let h1 = monitor.check_context_health("s1");
        assert!(h1.utilization_pct > 12.0 && h1.utilization_pct < 13.0);
        assert_eq!(h1.avg_growth_rate, 500);

        monitor.record_turn("s1", 1000, 4096);
        let h2 = monitor.check_context_health("s1");
        assert!(h2.utilization_pct > 24.0 && h2.utilization_pct < 25.0);
        assert_eq!(h2.avg_growth_rate, 500);

        monitor.record_turn("s1", 1800, 4096);
        let h3 = monitor.check_context_health("s1");
        assert!(h3.utilization_pct > 43.0 && h3.utilization_pct < 44.0);
    }

    #[test]
    fn warning_fires_above_sixty_percent() {
        let monitor = ContextMonitor::new();

        monitor.record_turn("s1", 2400, 4096);
        let h1 = monitor.check_context_health("s1");
        assert!(h1.utilization_pct < 60.0);
        assert!(h1.warning.is_none());

        monitor.record_turn("s1", 2600, 4096);
        let h2 = monitor.check_context_health("s1");
        assert!(h2.utilization_pct > 60.0);
        assert!(h2.warning.is_some());
        assert!(h2.warning.as_ref().unwrap().contains("Context window"));
    }

    #[test]
    fn should_compact_fires_above_seventy_five_percent() {
        let monitor = ContextMonitor::new();

        // Moderate growth, so the <3-turns-remaining rule doesn't fire first.
        monitor.record_turn("s1", 400, 8192);
        monitor.record_turn("s1", 800, 8192);
        monitor.record_turn("s1", 1200, 8192);
        monitor.record_turn("s1", 1600, 8192);
        monitor.record_turn("s1", 2000, 8192);

        let h1 = monitor.check_context_health("s1");
        assert!(h1.utilization_pct < 75.0, "util={}", h1.utilization_pct);
        assert!(
            !h1.should_compact,
            "should not compact at {}%",
            h1.utilization_pct
        );

        monitor.record_turn("s1", 6200, 8192);
        let h2 = monitor.check_context_health("s1");
        assert!(h2.utilization_pct > 75.0, "util={}", h2.utilization_pct);
        assert!(
            h2.should_compact,
            "should compact at {}%",
            h2.utilization_pct
        );
    }

    #[test]
    fn should_compact_fires_when_few_turns_remaining() {
        let monitor = ContextMonitor::new();

        // ~32% used, but only 2 turns remain at 1000/turn.
        monitor.record_turn("s1", 1000, 3072);
        let h1 = monitor.check_context_health("s1");
        assert!(h1.utilization_pct < 75.0, "Utilization should be below 75%");
        assert!(
            h1.estimated_turns_remaining < MIN_TURNS_REMAINING,
            "Should have fewer than {} turns remaining, got {}",
            MIN_TURNS_REMAINING,
            h1.estimated_turns_remaining
        );
        assert!(
            h1.should_compact,
            "Should compact when estimated_turns_remaining < {}",
            MIN_TURNS_REMAINING
        );
    }

    #[test]
    fn estimated_turns_remaining_calculates_correctly() {
        let monitor = ContextMonitor::new();

        monitor.record_turn("s1", 400, 4096);
        monitor.record_turn("s1", 800, 4096);
        monitor.record_turn("s1", 1200, 4096);

        let h = monitor.check_context_health("s1");
        assert_eq!(h.avg_growth_rate, 400);
        assert_eq!(h.estimated_turns_remaining, 7);
    }

    #[test]
    fn reset_clears_session_state() {
        let monitor = ContextMonitor::new();

        monitor.record_turn("s1", 2000, 4096);
        let h1 = monitor.check_context_health("s1");
        assert!(h1.utilization_pct > 0.0);

        monitor.reset_session("s1");

        let h2 = monitor.check_context_health("s1");
        assert_eq!(h2.utilization_pct, 0.0);
        assert_eq!(h2.avg_growth_rate, 0);
        assert_eq!(h2.estimated_turns_remaining, u32::MAX);
        assert!(!h2.should_compact);
        assert!(h2.warning.is_none());
    }

    #[test]
    fn growth_rates_capped_at_max_samples() {
        let monitor = ContextMonitor::new();

        for i in 1..=15u32 {
            monitor.record_turn("s1", i * 100, 8192);
        }

        let sessions = monitor.session_contexts.lock().unwrap();
        let state = sessions.get("s1").unwrap();
        assert_eq!(state.growth_rates.len(), MAX_GROWTH_SAMPLES);
        assert_eq!(state.turns, 15);
    }

    #[test]
    fn multiple_sessions_tracked_independently() {
        let monitor = ContextMonitor::new();

        monitor.record_turn("s1", 1000, 4096);
        monitor.record_turn("s2", 500, 8192);

        let h1 = monitor.check_context_health("s1");
        let h2 = monitor.check_context_health("s2");

        assert!(h1.utilization_pct > h2.utilization_pct);
        assert_eq!(h1.avg_growth_rate, 1000);
        assert_eq!(h2.avg_growth_rate, 500);
    }

    #[test]
    fn zero_context_limit_returns_zero_utilization() {
        let monitor = ContextMonitor::new();
        monitor.record_turn("s1", 100, 0);
        let h = monitor.check_context_health("s1");
        assert_eq!(h.utilization_pct, 0.0);
    }

    // ── Acting on should_compact ────────────────────────────────────────

    /// Drive one session above the 75% utilisation threshold.
    fn saturate(monitor: &ContextMonitor, session: &str) {
        monitor.record_turn(session, 7000, 8192);
    }

    #[test]
    fn a_session_under_no_pressure_cannot_claim_a_compaction() {
        let monitor = ContextMonitor::new();
        monitor.record_turn("s1", 500, 8192);
        assert!(
            !monitor.claim_compaction("s1"),
            "claimed a compaction at {}% utilisation",
            monitor.check_context_health("s1").utilization_pct,
        );
    }

    #[test]
    fn an_unknown_session_cannot_claim_a_compaction() {
        let monitor = ContextMonitor::new();
        assert!(!monitor.claim_compaction("never-seen"));
    }

    #[test]
    fn a_saturated_session_claims_once_and_then_waits_out_the_cooldown() {
        let monitor = ContextMonitor::new();
        saturate(&monitor, "s1");

        assert!(monitor.claim_compaction("s1"), "first claim must succeed");

        for turn in 1..COMPACTION_COOLDOWN_TURNS {
            saturate(&monitor, "s1");
            assert!(
                !monitor.claim_compaction("s1"),
                "claimed again only {turn} turn(s) into a {COMPACTION_COOLDOWN_TURNS}-turn cooldown \
                 - a still-saturated session would summarise between every pair of turns",
            );
        }

        saturate(&monitor, "s1");
        assert!(
            monitor.claim_compaction("s1"),
            "cooldown never expired after {COMPACTION_COOLDOWN_TURNS} turns",
        );
    }

    #[test]
    fn repeated_claims_within_one_turn_yield_exactly_one_pass() {
        let monitor = ContextMonitor::new();
        saturate(&monitor, "s1");

        let granted = (0..5).filter(|_| monitor.claim_compaction("s1")).count();
        assert_eq!(
            granted, 1,
            "{granted} concurrent claims were granted for one turn",
        );
    }

    #[test]
    fn the_cooldown_is_per_session() {
        let monitor = ContextMonitor::new();
        saturate(&monitor, "s1");
        saturate(&monitor, "s2");

        assert!(monitor.claim_compaction("s1"));
        assert!(
            monitor.claim_compaction("s2"),
            "one session's cooldown blocked another's",
        );
    }

    #[test]
    fn note_compacted_clears_growth_history_but_not_the_cooldown() {
        let monitor = ContextMonitor::new();
        saturate(&monitor, "s1");
        assert!(monitor.claim_compaction("s1"));

        monitor.note_compacted("s1");

        let h = monitor.check_context_health("s1");
        assert_eq!(h.avg_growth_rate, 0, "growth samples survived a compaction");
        assert_eq!(h.estimated_turns_remaining, u32::MAX);
        assert!(
            h.utilization_pct > COMPACT_THRESHOLD_PCT,
            "note_compacted pretended the context window emptied ({}%)",
            h.utilization_pct,
        );

        saturate(&monitor, "s1");
        assert!(
            !monitor.claim_compaction("s1"),
            "note_compacted cleared the cooldown, so the next turn re-fired",
        );
    }

    #[test]
    fn note_compacted_on_an_unknown_session_is_a_no_op() {
        let monitor = ContextMonitor::new();
        monitor.note_compacted("never-seen");
        assert!(!monitor.claim_compaction("never-seen"));
    }

    #[test]
    fn reset_session_clears_the_cooldown_too() {
        let monitor = ContextMonitor::new();
        saturate(&monitor, "s1");
        assert!(monitor.claim_compaction("s1"));

        monitor.reset_session("s1");

        saturate(&monitor, "s1");
        assert!(
            monitor.claim_compaction("s1"),
            "a cleared session inherited the cooldown of the conversation it replaced",
        );
    }

    // ── The manual claim ───────────────────────────────────────────────────

    /// In production the automatic claim always beats the press (it follows the button's frame).
    #[test]
    fn a_manual_claim_succeeds_after_the_pressure_axis_took_the_quota() {
        let monitor = ContextMonitor::new();
        saturate(&monitor, "s1");

        assert!(
            monitor.claim_compaction("s1"),
            "the pressure axis could not claim - this test reproduces nothing",
        );
        assert!(
            monitor.claim_manual_compaction("s1"),
            "the manual claim was refused by a quota the pressure axis had \
             already spent on this same session's behalf, which is what made \
             the Compact now button dead on every default install",
        );
    }

    #[test]
    fn a_manual_claim_rations_the_automatic_axis_afterwards() {
        let monitor = ContextMonitor::new();
        saturate(&monitor, "s1");

        assert!(monitor.claim_manual_compaction("s1"));
        for turn in 1..COMPACTION_COOLDOWN_TURNS {
            saturate(&monitor, "s1");
            assert!(
                !monitor.claim_compaction("s1"),
                "the pressure axis claimed only {turn} turn(s) after a manual \
                 press - a person pressing the button no longer costs the \
                 automatic axis anything, so the two together summarise more \
                 often than either alone ever could",
            );
        }
    }

    #[test]
    fn a_manual_claim_is_still_refused_on_a_session_under_no_pressure() {
        let monitor = ContextMonitor::new();
        monitor.record_turn("s1", 500, 8192);
        assert!(
            !monitor.claim_manual_compaction("s1"),
            "a person compacted a session at {}% utilisation",
            monitor.check_context_health("s1").utilization_pct,
        );
    }

    #[test]
    fn a_manual_claim_on_an_unknown_session_is_refused() {
        let monitor = ContextMonitor::new();
        assert!(!monitor.claim_manual_compaction("never-seen"));
    }
}
