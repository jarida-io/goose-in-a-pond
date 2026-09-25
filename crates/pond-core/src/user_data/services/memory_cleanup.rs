//! Memory decay and cleanup, using boop-agent's adaptive half-life formula.

use crate::user_data::domain::memory::{
    MemoryEventKind, MemoryFragment, MemoryLifecycle, MemoryTier,
};
use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::ports::memory_repository::MemoryRepository;
use anyhow::Result;

// Test copies of the `Settings` defaults (`default_memory_decay_*`); production reads settings.
#[cfg(test)]
const DEFAULT_BASE_HALF_LIFE_DAYS: f32 = 11.25;
#[cfg(test)]
const DEFAULT_DECAY_BETA: f32 = 0.8;

/// Compute the effective score of a memory after adaptive time-based decay.
///
/// **Adaptive half-life**: important memories literally have longer half-lives,
/// not just higher starting scores.
///
/// ```text
/// adaptive_half_life = base_half_life_days * (1.0 + importance)
/// lambda = (ln(2) / adaptive_half_life) * decay_beta * (1.0 + decay_rate)
/// decayed = importance * exp(-lambda * days_since_access)
/// reinforcement = 1.0 + ln(access_count + 1) * 0.1
/// score = (decayed * reinforcement).clamp(0.0, 1.0)
/// ```
///
/// - `base_half_life_days`: base half-life before importance scaling (default 11.25)
/// - `decay_beta`: steepness factor — lower = gentler decay (default 0.8)
/// - Permanent-tier memories always return their raw importance (no decay).
pub fn effective_score(
    fragment: &MemoryFragment,
    base_half_life_days: f32,
    decay_beta: f32,
) -> f32 {
    let importance = fragment.importance.unwrap_or(0.5);
    let decay_rate = fragment.decay_rate.unwrap_or(0.01);

    if fragment.tier.as_ref() == Some(&MemoryTier::Permanent) {
        return importance;
    }

    let days_since_access = fragment
        .last_accessed_at
        .map(|at| {
            let diff = chrono::Utc::now() - at;
            (diff.num_seconds() as f64 / 86400.0).max(0.0) as f32
        })
        .unwrap_or_else(|| {
            let diff = chrono::Utc::now() - fragment.created_at;
            (diff.num_seconds() as f64 / 86400.0).max(0.0) as f32
        });

    let adaptive_half_life = base_half_life_days * (1.0 + importance);
    let lambda = (2.0_f32.ln() / adaptive_half_life) * decay_beta * (1.0 + decay_rate);
    let decayed = importance * (-lambda * days_since_access).exp();
    let reinforcement = 1.0 + (fragment.access_count as f32 + 1.0).ln() * 0.1;
    (decayed * reinforcement).clamp(0.0, 1.0)
}

/// One cleanup pass; returns `(scanned, archived, pruned)`. Both outcomes archive the row:
/// below `prune_threshold` it is logged `Pruned`, below `archive_threshold` `Archived`.
pub async fn run_cleanup(
    repo: &dyn MemoryRepository,
    prune_threshold: f32,
    archive_threshold: f32,
    base_half_life_days: f32,
    decay_beta: f32,
) -> Result<(usize, usize, usize)> {
    let memories = repo.search_scoreable(&ProfileScope::Household).await?;
    let mut updates: Vec<(String, MemoryLifecycle)> = Vec::new();
    let mut event_kinds: Vec<(String, MemoryEventKind)> = Vec::new();
    let mut pruned = 0;
    let mut archived = 0;

    for mem in &memories {
        // Always prune empty/whitespace-only fragments regardless of tier
        if mem.content.trim().is_empty() {
            updates.push((mem.id.clone(), MemoryLifecycle::Archived));
            event_kinds.push((mem.id.clone(), MemoryEventKind::Pruned));
            pruned += 1;
            continue;
        }

        if mem.importance.is_none() {
            continue;
        }

        if mem.tier.as_ref() == Some(&MemoryTier::Permanent) {
            continue;
        }

        let score = effective_score(mem, base_half_life_days, decay_beta);

        if score < prune_threshold {
            updates.push((mem.id.clone(), MemoryLifecycle::Archived));
            event_kinds.push((mem.id.clone(), MemoryEventKind::Pruned));
            pruned += 1;
        } else if score < archive_threshold {
            updates.push((mem.id.clone(), MemoryLifecycle::Archived));
            event_kinds.push((mem.id.clone(), MemoryEventKind::Archived));
            archived += 1;
        }
    }

    if !updates.is_empty() {
        repo.batch_update_lifecycle(&updates).await?;

        for (id, kind) in &event_kinds {
            let _ = repo.log_event(kind.clone(), id, None, None).await;
        }
    }

    let scanned = memories.len();
    if pruned > 0 || archived > 0 {
        tracing::info!("[memory-cleanup] scanned={scanned}, archived={archived}, pruned={pruned}");
    }

    Ok((scanned, archived, pruned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::memory::{MemorySegment, MemoryTier};

    fn make_memory(
        importance: f32,
        decay_rate: f32,
        days_old: f64,
        access_count: u32,
        tier: MemoryTier,
    ) -> MemoryFragment {
        let created = chrono::Utc::now() - chrono::Duration::seconds((days_old * 86400.0) as i64);
        MemoryFragment {
            id: "test".to_string(),
            profile_id: None,
            session_id: None,
            content: "test memory".to_string(),
            embedding: None,
            source: "test".to_string(),
            tags: vec![],
            created_at: created,
            segment: Some(MemorySegment::Knowledge),
            importance: Some(importance),
            tier: Some(tier),
            decay_rate: Some(decay_rate),
            access_count,
            last_accessed_at: None, // uses created_at fallback
            lifecycle: Some(MemoryLifecycle::Active),
            superseded_by: None,
            corrects: None,
        }
    }

    const BASE: f32 = DEFAULT_BASE_HALF_LIFE_DAYS;
    const BETA: f32 = DEFAULT_DECAY_BETA;

    #[test]
    fn permanent_never_decays() {
        let mem = make_memory(0.8, 0.0, 365.0, 0, MemoryTier::Permanent);
        let score = effective_score(&mem, BASE, BETA);
        assert!(
            (score - 0.8).abs() < 0.01,
            "permanent score should be raw importance, got {score}"
        );
    }

    #[test]
    fn short_tier_decays_fast() {
        let mem = make_memory(0.5, 0.1, 30.0, 0, MemoryTier::Short);
        let score = effective_score(&mem, BASE, BETA);
        // Adaptive half-life = 11.25 * 1.5 = 16.9 days; at 30 days ~0.17
        assert!(
            score < 0.25,
            "short-tier 30-day-old memory should decay significantly, got {score}"
        );
    }

    #[test]
    fn long_tier_retains_well() {
        let mem = make_memory(0.7, 0.01, 30.0, 0, MemoryTier::Long);
        let score = effective_score(&mem, BASE, BETA);
        // Adaptive half-life = 11.25 * 1.7 = 19.1 days; at 30 days ~0.29
        assert!(
            score > 0.2,
            "long-tier 30-day memory should still be meaningful, got {score}"
        );
    }

    #[test]
    fn access_count_reinforces() {
        let mem_no_access = make_memory(0.5, 0.05, 20.0, 0, MemoryTier::Long);
        let mem_accessed = make_memory(0.5, 0.05, 20.0, 10, MemoryTier::Long);
        let score_no = effective_score(&mem_no_access, BASE, BETA);
        let score_yes = effective_score(&mem_accessed, BASE, BETA);
        assert!(
            score_yes > score_no,
            "accessed memory should score higher: {score_yes} vs {score_no}"
        );
    }

    #[test]
    fn fresh_memory_high_score() {
        let mem = make_memory(0.8, 0.01, 0.0, 0, MemoryTier::Long);
        let score = effective_score(&mem, BASE, BETA);
        assert!(
            score > 0.7,
            "fresh memory should have high score, got {score}"
        );
    }

    #[test]
    fn high_importance_decays_slower_than_low() {
        let high = make_memory(0.85, 0.01, 20.0, 0, MemoryTier::Long);
        let low = make_memory(0.3, 0.01, 20.0, 0, MemoryTier::Long);
        let score_high = effective_score(&high, BASE, BETA);
        let score_low = effective_score(&low, BASE, BETA);
        // Half-lives: 11.25 * 1.85 = 20.8 d (high) vs 11.25 * 1.3 = 14.6 d (low).
        let retention_high = score_high / 0.85;
        let retention_low = score_low / 0.3;
        assert!(
            retention_high > retention_low,
            "high-importance should retain more of its score: {retention_high:.3} vs {retention_low:.3}"
        );
    }

    #[test]
    fn adaptive_half_life_scales_with_importance() {
        // At its half-life (11.25 * 1.8 = 20.25 d) the score should be about importance / 2.
        let mem = make_memory(0.8, 0.0, 20.25, 0, MemoryTier::Long);
        let score = effective_score(&mem, BASE, BETA);
        // Loose tolerance: `decay_beta` < 1 leaves the score somewhat above half.
        let expected_half = 0.8 * 0.5;
        assert!(
            (score - expected_half).abs() < 0.15,
            "at adaptive half-life, score should be near {expected_half:.2}, got {score:.3}"
        );
    }
}
