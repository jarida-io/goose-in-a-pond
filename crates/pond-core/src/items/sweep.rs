//! One sweep enforces every kind's lifecycle ([`super::lifecycle`]), in place of a timer per
//! store: memory decay, context retention, and home events past their days.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use super::lifecycle::Kept;
use super::Kind;
use crate::context::ports::ContextRepository;
use crate::context::retention::ContextRetention;
use crate::user_data::ports::memory_repository::MemoryRepository;
use crate::user_data::services::memory_cleanup::run_cleanup;

/// The household's decay dials, from `Settings`; `None` when memory cleanup is switched off.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecayDials {
    pub prune_threshold: f32,
    pub archive_threshold: f32,
    pub base_half_life_days: f32,
    pub decay_beta: f32,
}

pub struct SweepPolicy {
    pub decay: Option<DecayDials>,
    /// `None` leaves ingested items as they are: decision 12 in context-space.md.
    pub retention: Option<ContextRetention>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub memories_scanned: usize,
    pub memories_archived: usize,
    pub memories_pruned: usize,
    pub context_purged: u64,
    pub uncited_purged: u64,
}

impl SweepReport {
    pub fn changed(&self) -> bool {
        self.memories_archived + self.memories_pruned > 0
            || self.context_purged + self.uncited_purged > 0
    }
}

/// What the sweep does for each way of keeping an item; each runs once however many kinds
/// share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Decay,
    SourceRetention,
    Uncited(Kind, u32),
}

fn steps() -> Vec<Step> {
    let mut steps = Vec::new();
    for kind in Kind::ALL {
        let step = match kind.lifecycle().kept {
            Kept::UntilSuperseded => Some(Step::Decay),
            // No ingested item carries a `valid_until` yet, so an event keeps to its source's
            // setting until one does.
            Kept::PerSourceSetting | Kept::PastValidUntil(_) => {
                (kind.detail_table() == "context_items").then_some(Step::SourceRetention)
            }
            Kept::DaysUnlessCited(days) => Some(Step::Uncited(kind, days)),
            Kept::WithConversation | Kept::UntilDeleted | Kept::UntilDecided => None,
        };
        if let Some(step) = step {
            if !steps.contains(&step) {
                steps.push(step);
            }
        }
    }
    steps
}

pub async fn sweep(
    memories: &dyn MemoryRepository,
    context: &dyn ContextRepository,
    policy: &SweepPolicy,
    now: DateTime<Utc>,
) -> Result<SweepReport> {
    let mut report = SweepReport::default();
    for step in steps() {
        match step {
            Step::Decay => {
                let Some(d) = policy.decay else { continue };
                let (scanned, archived, pruned) = run_cleanup(
                    memories,
                    d.prune_threshold,
                    d.archive_threshold,
                    d.base_half_life_days,
                    d.decay_beta,
                )
                .await?;
                report.memories_scanned = scanned;
                report.memories_archived = archived;
                report.memories_pruned = pruned;
            }
            Step::SourceRetention => {
                let Some(retention) = &policy.retention else {
                    continue;
                };
                report.context_purged += context.purge_expired(retention, now).await?;
            }
            Step::Uncited(kind, days) => {
                if policy.retention.is_none() {
                    continue;
                }
                let cutoff = now - Duration::days(i64::from(days));
                report.uncited_purged += context.purge_uncited(kind, cutoff).await?;
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::mocks::mock_context_repository::MockContextRepository;
    use crate::user_data::domain::memory::MemoryFragment;
    use crate::user_data::mocks::mock_memory::MockMemoryRepository;
    use std::collections::HashMap;

    #[test]
    fn every_way_of_keeping_an_item_is_one_step_run_once() {
        assert_eq!(
            steps(),
            vec![
                Step::Decay,
                Step::SourceRetention,
                Step::Uncited(Kind::HomeEvent, super::super::lifecycle::HOME_EVENT_DAYS),
            ]
        );
    }

    fn decayed() -> MemoryFragment {
        let mut old = MemoryFragment::from_chat("old".into(), None, None, "x".into());
        old.importance = Some(0.1);
        old.created_at = Utc::now() - Duration::days(400);
        old
    }

    fn dials() -> DecayDials {
        DecayDials {
            prune_threshold: 0.05,
            archive_threshold: 0.2,
            base_half_life_days: 11.25,
            decay_beta: 0.8,
        }
    }

    #[tokio::test]
    async fn the_sweep_decays_memory_and_leaves_ingested_items_while_retention_is_off() {
        let memories = MockMemoryRepository::new();
        memories.add(decayed()).await.unwrap();
        let context = MockContextRepository::new();
        let report = sweep(
            &memories,
            &context,
            &SweepPolicy {
                decay: Some(dials()),
                retention: None,
            },
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(report.memories_scanned, 1);
        assert_eq!(report.memories_archived + report.memories_pruned, 1);
        assert_eq!(report.context_purged + report.uncited_purged, 0);
    }

    #[tokio::test]
    async fn with_decay_off_no_memory_is_touched() {
        let memories = MockMemoryRepository::new();
        memories.add(decayed()).await.unwrap();
        let report = sweep(
            &memories,
            &MockContextRepository::new(),
            &SweepPolicy {
                decay: None,
                retention: Some(ContextRetention::new(HashMap::new(), 30, 7)),
            },
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(report.memories_scanned, 0);
        assert!(!report.changed());
    }
}
