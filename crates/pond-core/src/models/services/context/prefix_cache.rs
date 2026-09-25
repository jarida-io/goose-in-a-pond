//! KV prefix-cache state: recompact freely when the cache is already lost, else keep it intact.
//! Only ever RELAXES the trimmer's guard (when provably cold); no `turns_served` threshold.

use std::time::{Duration, Instant};

/// Why the KV prefix is no longer usable; each names a place in the live adapter that kills it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationReason {
    /// A new provider object was built and swapped into the engine.
    ProviderRebuilt,
    /// The chat model changed; a `ProviderRebuilt` can keep the same model (thinking re-stamp).
    ModelSwapped,
    /// A fresh engine session was hydrated with an existing conversation's history.
    SessionResumed,
    /// The turn carries an image; multimodal turns forfeit KV retention outright.
    MultimodalTurn,
    /// The tool set changed, which moves the tools block inside the prefix.
    ToolSetChanged,
    /// The static system prefix itself changed.
    PromptChanged,
}

impl InvalidationReason {
    /// Stable, lowercase name for structured logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderRebuilt => "provider_rebuilt",
            Self::ModelSwapped => "model_swapped",
            Self::SessionResumed => "session_resumed",
            Self::MultimodalTurn => "multimodal_turn",
            Self::ToolSetChanged => "tool_set_changed",
            Self::PromptChanged => "prompt_changed",
        }
    }
}

/// What compaction may assume about the KV prefix. An unseen cache resolves to `Warm` in
/// [`PrefixCacheState::posture_of`]: assuming cold would spend re-prefills mid-conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePosture {
    /// Served a turn, not invalidated: front edits cost a real re-prefill, so only when forced.
    Warm,
    /// Gone or never served a turn: this turn re-prefills anyway, so recompaction is free.
    Cold,
}

/// The engine's static-prefix cache: one per adapter (one `last_prefix_hash`), not per session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefixCacheState {
    /// Telemetry only: no decision reads it, so this file never calls a clock.
    pub built_at: Instant,
    /// Hash of the current static prefix — the adapter's `prefix_hash`.
    pub hash: u64,
    /// Turns served off this prefix without a rebuild; only its zero case is acted on.
    pub turns_served: u32,
    /// The invalidation that made the prefix cold; `serve_turn` clears it one turn late.
    pub invalidated_by: Option<InvalidationReason>,
}

impl PrefixCacheState {
    /// Built but unserved, hence `Cold`: at construction the engine has no cache either.
    pub fn new(hash: u64, built_at: Instant) -> Self {
        Self {
            built_at,
            hash,
            turns_served: 0,
            invalidated_by: None,
        }
    }

    /// Record that something destroyed the prefix; its successor must not inherit `turns_served`.
    pub fn invalidate(&mut self, reason: InvalidationReason) {
        self.invalidated_by = Some(reason);
        self.turns_served = 0;
    }

    /// Record a new prefix. Clearing the reason doesn't make it warm: `turns_served` is zero.
    pub fn rebuilt(&mut self, hash: u64, now: Instant) {
        self.hash = hash;
        self.built_at = now;
        self.turns_served = 0;
        self.invalidated_by = None;
    }

    /// Record a turn served off this prefix. The reason clears one turn late ON PURPOSE: a
    /// resume is recorded earlier in the same turn, and an eager clear would read it as `Warm`.
    pub fn serve_turn(&mut self) {
        if self.turns_served > 0 {
            self.invalidated_by = None;
        }
        self.turns_served = self.turns_served.saturating_add(1);
    }

    pub fn age_since(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.built_at)
    }

    /// The rule, on a state we can see.
    pub fn posture(&self) -> CachePosture {
        if self.invalidated_by.is_some() || self.turns_served == 0 {
            CachePosture::Cold
        } else {
            CachePosture::Warm
        }
    }

    /// The single place an unseen state becomes a policy (`Warm`; see [`CachePosture`]).
    pub fn posture_of(state: Option<&Self>) -> CachePosture {
        match state {
            Some(s) => s.posture(),
            None => CachePosture::Warm,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> PrefixCacheState {
        PrefixCacheState::new(0xabc, Instant::now())
    }

    #[test]
    fn a_prefix_that_has_served_nothing_is_cold() {
        let state = fresh();
        assert_eq!(state.turns_served, 0);
        assert_eq!(state.invalidated_by, None);
        assert_eq!(state.posture(), CachePosture::Cold);
    }

    #[test]
    fn one_served_turn_is_enough_to_be_warm() {
        let mut state = fresh();
        state.serve_turn();
        assert_eq!(state.turns_served, 1);
        assert_eq!(state.posture(), CachePosture::Warm);
    }

    #[test]
    fn every_invalidation_reason_takes_a_warm_prefix_cold_and_names_itself() {
        let all = [
            InvalidationReason::ProviderRebuilt,
            InvalidationReason::ModelSwapped,
            InvalidationReason::SessionResumed,
            InvalidationReason::MultimodalTurn,
            InvalidationReason::ToolSetChanged,
            InvalidationReason::PromptChanged,
        ];
        for reason in all {
            let mut state = fresh();
            state.serve_turn();
            state.serve_turn();
            assert_eq!(
                state.posture(),
                CachePosture::Warm,
                "{reason:?}: setup failed to warm the prefix"
            );

            state.invalidate(reason);

            assert_eq!(
                state.posture(),
                CachePosture::Cold,
                "{reason:?} left the prefix warm"
            );
            assert_eq!(state.invalidated_by, Some(reason));
            assert_eq!(
                state.turns_served, 0,
                "{reason:?} let the new prefix inherit the old one's standing"
            );
        }

        let mut names: Vec<&str> = all.iter().map(|r| r.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 6);
    }

    /// The adapter rebuilds and trims in one turn; a warm rebuild would make the rule unreachable.
    #[test]
    fn rebuilding_does_not_make_a_prefix_warm() {
        let mut state = fresh();
        state.serve_turn();
        state.serve_turn();
        assert_eq!(state.posture(), CachePosture::Warm);

        state.invalidate(InvalidationReason::PromptChanged);
        state.rebuilt(0xdef, Instant::now());

        assert_eq!(state.hash, 0xdef);
        assert_eq!(state.turns_served, 0);
        assert_eq!(state.posture(), CachePosture::Cold);
    }

    #[test]
    fn the_turn_after_a_rebuild_is_warm() {
        let mut state = fresh();
        state.invalidate(InvalidationReason::PromptChanged);
        state.rebuilt(7, Instant::now());
        assert_eq!(state.posture(), CachePosture::Cold);

        state.serve_turn();
        assert_eq!(state.posture(), CachePosture::Warm);
    }

    #[test]
    fn a_resume_served_in_the_same_turn_stays_cold() {
        let mut state = fresh();
        state.serve_turn();
        state.serve_turn();
        assert_eq!(state.posture(), CachePosture::Warm);

        state.invalidate(InvalidationReason::SessionResumed);
        // Same turn: the prefix hash is found unchanged, so the turn is served off it.
        state.serve_turn();

        assert_eq!(
            state.posture(),
            CachePosture::Cold,
            "the resume cleared itself within its own turn"
        );
        assert_eq!(
            state.invalidated_by,
            Some(InvalidationReason::SessionResumed)
        );

        state.serve_turn();
        assert_eq!(state.posture(), CachePosture::Warm);
        assert_eq!(state.invalidated_by, None);
    }

    #[test]
    fn an_unseen_cache_is_treated_as_warm() {
        assert_eq!(PrefixCacheState::posture_of(None), CachePosture::Warm);

        let mut warm = fresh();
        warm.serve_turn();
        assert_eq!(
            PrefixCacheState::posture_of(Some(&warm)),
            CachePosture::Warm
        );
        assert_eq!(
            PrefixCacheState::posture_of(Some(&fresh())),
            CachePosture::Cold
        );
    }

    #[test]
    fn age_is_measured_against_a_supplied_now_and_never_goes_backwards() {
        let built = Instant::now();
        let state = PrefixCacheState::new(1, built);
        assert_eq!(state.age_since(built), Duration::ZERO);
        assert_eq!(
            state.age_since(built + Duration::from_secs(90)),
            Duration::from_secs(90)
        );
        // `checked_sub`: `Instant - Duration` panics if unrepresentable, possible just after boot.
        if let Some(earlier) = built.checked_sub(Duration::from_secs(5)) {
            assert_eq!(state.age_since(earlier), Duration::ZERO);
        }
    }

    #[test]
    fn many_served_turns_accumulate_and_saturate_rather_than_wrapping() {
        let mut state = fresh();
        for _ in 0..30 {
            state.serve_turn();
        }
        assert_eq!(state.turns_served, 30);
        assert_eq!(state.posture(), CachePosture::Warm);

        state.turns_served = u32::MAX;
        state.serve_turn();
        assert_eq!(state.turns_served, u32::MAX);
        assert_eq!(state.posture(), CachePosture::Warm);
    }
}
