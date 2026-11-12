//! Memory consolidation. [`select_batch`] keeps a run inside a 3B model's context, and
//! [`apply_actions`] is the only write path, so both modes share its correction, kept and owner
//! guards.

use crate::user_data::domain::member_profile::{file_under, About};
use crate::user_data::domain::memory::{
    MemoryEventKind, MemoryFragment, MemoryLifecycle, MemorySegment, MAX_FACT_CONTENT_LEN,
};
use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::ports::memory_consolidator::{ConsolidationAction, MemoryConsolidator};
use crate::user_data::ports::memory_repository::MemoryRepository;
use crate::user_data::services::consolidation_schedule::MIN_MEMORIES_TO_CONSOLIDATE;
use anyhow::Result;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Single-pass label in `consolidation_runs`, matching the `memory_consolidation_mode` value.
pub const MODE_SINGLE: &str = "single";
/// Mode label for a three-stage Proposer/Adversary/Judge run.
pub const MODE_ADVERSARIAL: &str = "adversarial";

/// Which consolidation pipeline a run should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolidationMode {
    /// One LLM call. The default.
    Single,
    /// Three sequential LLM calls: Proposer -> Adversary -> Judge.
    Adversarial,
}

impl ConsolidationMode {
    /// Stable label for logs and the `consolidation_runs.mode` column.
    pub fn as_str(self) -> &'static str {
        match self {
            ConsolidationMode::Single => MODE_SINGLE,
            ConsolidationMode::Adversarial => MODE_ADVERSARIAL,
        }
    }

    /// True when this mode costs three LLM calls instead of one.
    pub fn is_adversarial(self) -> bool {
        matches!(self, ConsolidationMode::Adversarial)
    }
}

/// Parse `memory_consolidation_mode`; anything unrecognised falls back to the cheap `Single`.
pub fn mode_from_setting(setting: &str) -> ConsolidationMode {
    if setting.trim().eq_ignore_ascii_case(MODE_ADVERSARIAL) {
        ConsolidationMode::Adversarial
    } else {
        ConsolidationMode::Single
    }
}

/// The bounded slice of memories one consolidation run may see, and how much was left out.
#[derive(Debug, Clone)]
pub struct BatchSelection {
    /// Memories this run may reason about, oldest first.
    pub batch: Vec<MemoryFragment>,
    /// How many scoreable, segmented memories were available in total.
    pub considered: usize,
    /// How many the batch cap left for a future run; callers should log it.
    pub deferred: usize,
}

impl BatchSelection {
    /// True when there are too few memories for consolidation to be worthwhile.
    pub fn below_minimum(&self) -> bool {
        self.batch.len() < MIN_MEMORIES_TO_CONSOLIDATE
    }
}

/// Choose which segmented memories a run may see: oldest first, as duplicates cluster in time.
/// Limitation: the window never rotates, so a run proposing nothing re-reads the same slice.
pub fn select_batch(memories: Vec<MemoryFragment>, batch_size: usize) -> BatchSelection {
    // A zero/absurd setting must not disable consolidation outright.
    let batch_size = batch_size.max(MIN_MEMORIES_TO_CONSOLIDATE);

    let mut eligible: Vec<MemoryFragment> = memories
        .into_iter()
        .filter(|m| m.segment.is_some())
        .collect();
    eligible.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    let considered = eligible.len();
    let deferred = considered.saturating_sub(batch_size);
    eligible.truncate(batch_size);

    BatchSelection {
        batch: eligible,
        considered,
        deferred,
    }
}

/// One batch per owner, the household's shared memories among them: a merge or split that
/// crossed two members would hand one member's facts to the other.
pub fn select_owner_batches(
    memories: Vec<MemoryFragment>,
    batch_size: usize,
) -> Vec<BatchSelection> {
    let mut by_owner: BTreeMap<Option<String>, Vec<MemoryFragment>> = BTreeMap::new();
    for memory in memories {
        by_owner
            .entry(memory.profile_id.clone())
            .or_default()
            .push(memory);
    }
    by_owner
        .into_values()
        .map(|group| select_batch(group, batch_size))
        .collect()
}

/// How many of each action a run actually applied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub merged: usize,
    pub pruned: usize,
    pub split: usize,
    pub recategorized: usize,
    pub superseded: usize,
    /// Actions refused by a correction, kept-memory or owner guard.
    pub blocked: usize,
}

impl ApplyOutcome {
    /// Total number of actions that changed the store.
    pub fn changed(&self) -> usize {
        self.merged + self.pruned + self.split + self.recategorized + self.superseded
    }
}

/// The one owner every id shares, all of them in the batch; `None` when any is outside it or two
/// owners differ. The inner `None` is the household's shared memories.
fn sole_owner<'a>(
    ids: impl IntoIterator<Item = &'a String>,
    memory_map: &HashMap<&str, &MemoryFragment>,
) -> Option<Option<String>> {
    let mut owner: Option<Option<String>> = None;
    for id in ids {
        let profile = &memory_map.get(id.as_str())?.profile_id;
        match &owner {
            None => owner = Some(profile.clone()),
            Some(seen) if seen != profile => return None,
            Some(_) => {}
        }
    }
    owner
}

/// One memory a run changed or wrote, with its text, so the member can see what changed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ChangedRow {
    pub id: String,
    pub text: String,
}

/// One change a run made: `archive`, `merge`, `split`, `supersede` or `recategorize`, the rows
/// it took out of use (`rows`), what replaced them (`into`), and the model's reason.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppliedChange {
    pub action: &'static str,
    pub reason: String,
    pub rows: Vec<ChangedRow>,
    pub into: Vec<ChangedRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment: Option<String>,
}

fn changed(memory: &MemoryFragment) -> ChangedRow {
    ChangedRow {
        id: memory.id.clone(),
        text: memory.content.clone(),
    }
}

/// What `consolidation_runs.details` records for a run: every change with its rows' text and
/// reason, beside the trial the model held (`null` for a single-pass run).
pub fn run_details(changes: &[AppliedChange], trial: Option<serde_json::Value>) -> String {
    serde_json::json!({ "changes": changes, "trial": trial }).to_string()
}

/// Apply accepted consolidation actions. Pass the batch the model saw: an action naming a memory
/// outside it, or a merge across owners, is refused, and what is written keeps its sources' owner.
pub async fn apply_actions(
    repo: &dyn MemoryRepository,
    batch: &[MemoryFragment],
    actions: impl IntoIterator<Item = ConsolidationAction>,
) -> Result<ApplyOutcome> {
    Ok(apply_proposals(
        repo,
        batch,
        actions.into_iter().map(|action| (action, String::new())),
    )
    .await?
    .0)
}

/// [`apply_actions`], each action with the model's reason for it, returning every change made.
pub async fn apply_proposals(
    repo: &dyn MemoryRepository,
    batch: &[MemoryFragment],
    proposals: impl IntoIterator<Item = (ConsolidationAction, String)>,
) -> Result<(ApplyOutcome, Vec<AppliedChange>)> {
    let memory_map: HashMap<&str, &MemoryFragment> =
        batch.iter().map(|m| (m.id.as_str(), m)).collect();

    let mut outcome = ApplyOutcome::default();
    let mut changes: Vec<AppliedChange> = Vec::new();

    for (action, reason) in proposals {
        match action {
            ConsolidationAction::Merge {
                source_ids,
                merged_content,
                segment,
                importance,
            } => {
                // A blank merge would store an empty memory and supersede every source onto it.
                if merged_content.trim().is_empty() {
                    tracing::warn!("[consolidation] blocked a merge with no merged text");
                    outcome.blocked += 1;
                    continue;
                }
                let Some(owner) = sole_owner(&source_ids, &memory_map) else {
                    tracing::warn!(
                        "[consolidation] blocked a merge across owners or outside the batch"
                    );
                    outcome.blocked += 1;
                    continue;
                };
                // A merge folds one fact said twice. Longer than a fact, or built from rows
                // filed under different things, it is a paragraph that undoes the profile.
                let filed: HashSet<About> = source_ids
                    .iter()
                    .filter_map(|id| memory_map.get(id.as_str()))
                    .filter_map(|m| m.about)
                    .collect();
                if merged_content.trim().chars().count() > MAX_FACT_CONTENT_LEN || filed.len() > 1 {
                    tracing::warn!(
                        chars = merged_content.trim().chars().count(),
                        filed_under = filed.len(),
                        "[consolidation] blocked a merge that joins different facts"
                    );
                    outcome.blocked += 1;
                    continue;
                }
                // A correction source makes the merge a correction too, so it stays unprunable.
                let any_correction = source_ids
                    .iter()
                    .filter_map(|id| memory_map.get(id.as_str()))
                    .any(|m| m.is_correction());

                let effective_segment = if any_correction {
                    MemorySegment::Correction
                } else {
                    segment
                };

                let corrects = source_ids
                    .iter()
                    .filter_map(|id| memory_map.get(id.as_str()))
                    .filter_map(|m| m.corrects.clone())
                    .next();

                // A kept source makes the merge kept too, or merging would undo the keep.
                let any_kept = source_ids
                    .iter()
                    .filter_map(|id| memory_map.get(id.as_str()))
                    .any(|m| m.is_kept());

                if any_correction {
                    tracing::info!(
                        "[consolidation] merge includes correction source — forcing segment=Correction"
                    );
                }

                let merged_text = merged_content.clone();
                let mut new_frag = MemoryFragment::from_extraction(
                    uuid::Uuid::new_v4().to_string(),
                    None,
                    merged_content,
                    effective_segment,
                    importance,
                    corrects,
                );
                new_frag.profile_id = owner;
                // What its sources were filed under still says what the merge is about.
                if let (Some(about), false) = (filed.iter().next().copied(), any_correction) {
                    file_under(&mut new_frag, about);
                }
                if any_kept {
                    new_frag.keep();
                }
                let new_id = new_frag.id.clone();
                // Bail if the replacement fails to write; its sources must not be superseded.
                repo.add(new_frag).await?;

                for src_id in &source_ids {
                    let _ = repo.mark_superseded(src_id, &new_id).await;
                    let _ = repo
                        .log_event(MemoryEventKind::Superseded, src_id, None, Some(&new_id))
                        .await;
                }
                let _ = repo
                    .log_event(MemoryEventKind::Consolidated, &new_id, None, None)
                    .await;
                changes.push(AppliedChange {
                    action: "merge",
                    reason: reason.clone(),
                    rows: source_ids
                        .iter()
                        .filter_map(|id| memory_map.get(id.as_str()))
                        .map(|m| changed(m))
                        .collect(),
                    into: vec![ChangedRow {
                        id: new_id.clone(),
                        text: merged_text,
                    }],
                    segment: None,
                });
                outcome.merged += 1;
            }
            ConsolidationAction::Supersede { newer_id, older_id } => {
                let pair = [newer_id.clone(), older_id.clone()];
                let (Some(_), Some(older), Some(newer)) = (
                    sole_owner(&pair, &memory_map),
                    memory_map.get(older_id.as_str()),
                    memory_map.get(newer_id.as_str()),
                ) else {
                    tracing::warn!(
                        "[consolidation] blocked a supersede across owners or outside the batch"
                    );
                    outcome.blocked += 1;
                    continue;
                };
                if newer_id == older_id
                    || older.is_kept()
                    || (older.is_correction() && !newer.is_correction())
                {
                    tracing::warn!(
                        "[consolidation] blocked superseding a kept or correction memory {older_id}"
                    );
                    outcome.blocked += 1;
                    continue;
                }
                let _ = repo.mark_superseded(&older_id, &newer_id).await;
                let _ = repo
                    .update_lifecycle(&older_id, MemoryLifecycle::Archived)
                    .await;
                let _ = repo
                    .log_event(
                        MemoryEventKind::Superseded,
                        &older_id,
                        None,
                        Some(&newer_id),
                    )
                    .await;
                changes.push(AppliedChange {
                    action: "supersede",
                    reason: reason.clone(),
                    rows: vec![changed(older)],
                    into: vec![changed(newer)],
                    segment: None,
                });
                outcome.superseded += 1;
            }
            ConsolidationAction::Prune { id } => {
                let Some(mem) = memory_map.get(id.as_str()) else {
                    tracing::warn!("[consolidation] blocked prune of {id}, outside the batch");
                    outcome.blocked += 1;
                    continue;
                };
                if mem.is_correction() || mem.is_kept() {
                    tracing::warn!(
                        "[consolidation] blocked prune of correction or kept memory {id}"
                    );
                    outcome.blocked += 1;
                    continue;
                }

                // Archived, never deleted: the member can see it in the run's details and restore it.
                let _ = repo.update_lifecycle(&id, MemoryLifecycle::Archived).await;
                let _ = repo
                    .log_event(MemoryEventKind::Pruned, &id, None, None)
                    .await;
                changes.push(AppliedChange {
                    action: "archive",
                    reason: reason.clone(),
                    rows: vec![changed(mem)],
                    into: Vec::new(),
                    segment: None,
                });
                outcome.pruned += 1;
            }
            ConsolidationAction::Recategorize {
                id,
                new_segment,
                new_importance,
            } => {
                let Some(memory) = memory_map.get(id.as_str()) else {
                    tracing::warn!(
                        "[consolidation] blocked recategorising {id}, outside the batch"
                    );
                    outcome.blocked += 1;
                    continue;
                };
                changes.push(AppliedChange {
                    action: "recategorize",
                    reason: reason.clone(),
                    rows: vec![changed(memory)],
                    into: Vec::new(),
                    segment: Some(format!("{new_segment:?}").to_lowercase()),
                });
                let _ = repo.update_segment(&id, new_segment, new_importance).await;
                let _ = repo
                    .log_event(
                        MemoryEventKind::Consolidated,
                        &id,
                        None,
                        Some("recategorized"),
                    )
                    .await;
                outcome.recategorized += 1;
            }
            ConsolidationAction::Split {
                source_id,
                new_memories,
            } => {
                let Some(source) = memory_map.get(source_id.as_str()) else {
                    tracing::warn!(
                        "[consolidation] blocked split of {source_id}, outside the batch"
                    );
                    outcome.blocked += 1;
                    continue;
                };
                // Carry `corrects` to the first Correction entry so the fix survives the split.
                let source_corrects = source.corrects.clone();
                // Every piece of a kept memory stays kept.
                let source_kept = source.is_kept();

                let mut first_new_id = String::new();
                let mut corrects_assigned = false;
                let mut pieces: Vec<ChangedRow> = Vec::new();

                for entry in &new_memories {
                    let entry_corrects = if !corrects_assigned
                        && source_corrects.is_some()
                        && entry.segment == MemorySegment::Correction
                    {
                        corrects_assigned = true;
                        source_corrects.clone()
                    } else {
                        None
                    };

                    let mut new_frag = MemoryFragment::from_extraction(
                        uuid::Uuid::new_v4().to_string(),
                        None,
                        entry.content.clone(),
                        entry.segment.clone(),
                        entry.importance,
                        entry_corrects,
                    );
                    new_frag.profile_id = source.profile_id.clone();
                    if source_kept {
                        new_frag.keep();
                    }
                    let nid = new_frag.id.clone();
                    if first_new_id.is_empty() {
                        first_new_id = nid.clone();
                    }
                    pieces.push(ChangedRow {
                        id: nid.clone(),
                        text: entry.content.clone(),
                    });
                    repo.add(new_frag).await?;
                    let _ = repo
                        .log_event(MemoryEventKind::Consolidated, &nid, None, None)
                        .await;
                }
                if !first_new_id.is_empty() {
                    let _ = repo.mark_superseded(&source_id, &first_new_id).await;
                    let _ = repo
                        .log_event(
                            MemoryEventKind::Superseded,
                            &source_id,
                            None,
                            Some(&first_new_id),
                        )
                        .await;
                    changes.push(AppliedChange {
                        action: "split",
                        reason: reason.clone(),
                        rows: vec![changed(source)],
                        into: pieces,
                        segment: None,
                    });
                    outcome.split += 1;
                }
            }
        }
    }

    Ok((outcome, changes))
}

/// Run single-pass consolidation once per owner's batch, recording an audit row for each; returns
/// `(merged, pruned)` summed. The adversarial mode is in `pond-server`, which holds the cancel
/// token and SSE sink.
pub async fn run_consolidation(
    consolidator: &dyn MemoryConsolidator,
    repo: &dyn MemoryRepository,
    batch_size: usize,
) -> Result<(usize, usize)> {
    let (mut merged, mut pruned) = (0, 0);
    let memories = repo.search_scoreable(&ProfileScope::Household).await?;
    for selection in select_owner_batches(memories, batch_size) {
        if selection.below_minimum() {
            continue;
        }
        let started = std::time::Instant::now();
        if selection.deferred > 0 {
            tracing::info!(
                mode = MODE_SINGLE,
                considered = selection.considered,
                batch = selection.batch.len(),
                deferred = selection.deferred,
                "[memory-consolidation] batch cap applied — remaining memories deferred to the next run"
            );
        }

        let actions = consolidator.consolidate(&selection.batch).await?;
        let proposed = actions.len();
        let (outcome, changes) = apply_proposals(
            repo,
            &selection.batch,
            actions.into_iter().map(|a| (a, String::new())),
        )
        .await?;

        if outcome.changed() > 0 {
            tracing::info!(
                merged = outcome.merged,
                pruned = outcome.pruned,
                split = outcome.split,
                recategorized = outcome.recategorized,
                blocked = outcome.blocked,
                "[memory-consolidation] single-pass complete"
            );
        }

        let _ = repo
            .log_consolidation_run(
                MODE_SINGLE,
                selection.batch.len(),
                outcome.changed(),
                proposed.saturating_sub(outcome.changed()),
                started.elapsed().as_millis() as u64,
                Some(&run_details(&changes, None)),
            )
            .await;

        merged += outcome.merged + outcome.recategorized;
        pruned += outcome.pruned;
    }
    Ok((merged, pruned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::mocks::mock_memory::MockMemoryRepository;
    use crate::user_data::ports::memory_consolidator::SplitEntry;
    use async_trait::async_trait;
    use chrono::{Duration as ChronoDuration, Utc};

    fn frag(id: &str, segment: MemorySegment, age_secs: i64) -> MemoryFragment {
        let mut f = MemoryFragment::from_extraction(
            id.to_string(),
            None,
            format!("content of {id}"),
            segment,
            0.5,
            None,
        );
        f.created_at = Utc::now() - ChronoDuration::seconds(age_secs);
        f
    }

    fn correction(id: &str, corrects: Option<&str>) -> MemoryFragment {
        MemoryFragment::from_extraction(
            id.to_string(),
            None,
            format!("correction {id}"),
            MemorySegment::Correction,
            0.9,
            corrects.map(str::to_string),
        )
    }

    // ── mode dispatch ───────────────────────────────────────────────────────

    #[test]
    fn mode_setting_selects_the_pipeline() {
        assert_eq!(mode_from_setting("single"), ConsolidationMode::Single);
        assert_eq!(
            mode_from_setting("adversarial"),
            ConsolidationMode::Adversarial
        );
    }

    #[test]
    fn mode_setting_is_case_and_whitespace_tolerant() {
        assert_eq!(
            mode_from_setting("  Adversarial "),
            ConsolidationMode::Adversarial
        );
        assert_eq!(mode_from_setting("SINGLE"), ConsolidationMode::Single);
    }

    #[test]
    fn unknown_mode_falls_back_to_single_not_adversarial() {
        for bogus in ["", "three-stage", "adversarial-v2", "yes", "true"] {
            assert_eq!(
                mode_from_setting(bogus),
                ConsolidationMode::Single,
                "{bogus:?} must resolve to the cheap path"
            );
        }
    }

    #[test]
    fn mode_labels_match_the_setting_values() {
        assert_eq!(ConsolidationMode::Single.as_str(), MODE_SINGLE);
        assert_eq!(ConsolidationMode::Adversarial.as_str(), MODE_ADVERSARIAL);
        for mode in [ConsolidationMode::Single, ConsolidationMode::Adversarial] {
            assert_eq!(mode_from_setting(mode.as_str()), mode);
        }
    }

    #[test]
    fn the_default_setting_resolves_to_single() {
        let defaults = crate::user_data::domain::settings::Settings::default();
        assert_eq!(
            mode_from_setting(&defaults.memory_consolidation_mode),
            ConsolidationMode::Single
        );
    }

    // ── select_batch ────────────────────────────────────────────────────────

    #[test]
    fn select_batch_caps_and_reports_the_remainder() {
        let memories: Vec<MemoryFragment> = (0..25)
            .map(|i| frag(&format!("m{i}"), MemorySegment::Knowledge, i as i64))
            .collect();

        let selection = select_batch(memories, 10);

        assert_eq!(selection.batch.len(), 10, "batch must respect the cap");
        assert_eq!(selection.considered, 25);
        assert_eq!(
            selection.deferred, 15,
            "the remainder must be reported, not silently dropped"
        );
    }

    #[test]
    fn select_batch_takes_the_oldest_first() {
        // m0 is newest (age 0), m24 is oldest (age 24).
        let memories: Vec<MemoryFragment> = (0..25)
            .map(|i| frag(&format!("m{i}"), MemorySegment::Knowledge, i as i64))
            .collect();

        let selection = select_batch(memories, 6);

        let ids: Vec<&str> = selection.batch.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["m24", "m23", "m22", "m21", "m20", "m19"]);
    }

    #[test]
    fn select_batch_skips_unsegmented_memories() {
        let mut memories = vec![frag("a", MemorySegment::Knowledge, 1)];
        let mut bare = frag("b", MemorySegment::Knowledge, 2);
        bare.segment = None;
        memories.push(bare);

        let selection = select_batch(memories, 50);
        assert_eq!(selection.considered, 1);
        assert_eq!(selection.batch[0].id, "a");
    }

    #[test]
    fn select_batch_reports_no_remainder_when_under_the_cap() {
        let memories: Vec<MemoryFragment> = (0..4)
            .map(|i| frag(&format!("m{i}"), MemorySegment::Knowledge, i as i64))
            .collect();
        let selection = select_batch(memories, 50);
        assert_eq!(selection.deferred, 0);
        assert!(
            selection.below_minimum(),
            "4 memories is under the useful minimum"
        );
    }

    #[test]
    fn select_batch_floors_an_absurd_batch_size() {
        let memories: Vec<MemoryFragment> = (0..20)
            .map(|i| frag(&format!("m{i}"), MemorySegment::Knowledge, i as i64))
            .collect();
        // A stored 0 must not mean "consolidate nothing forever".
        let selection = select_batch(memories, 0);
        assert_eq!(selection.batch.len(), MIN_MEMORIES_TO_CONSOLIDATE);
    }

    // ── apply_actions kept-memory safety ───────────────────────────────────

    fn kept(id: &str) -> MemoryFragment {
        let mut f = frag(id, MemorySegment::Preference, 5);
        f.keep();
        f
    }

    #[tokio::test]
    async fn prune_of_a_kept_memory_is_blocked() {
        let repo = MockMemoryRepository::new();
        let batch = vec![kept("p1")];

        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Prune {
                id: "p1".to_string(),
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.pruned, 0);
        assert_eq!(outcome.blocked, 1);
        assert!(repo.lifecycle_updates().await.is_empty());
    }

    #[tokio::test]
    async fn a_merge_with_no_text_stores_nothing_and_supersedes_nothing() {
        let repo = MockMemoryRepository::new();
        let batch = vec![
            frag("k1", MemorySegment::Knowledge, 5),
            frag("k2", MemorySegment::Knowledge, 6),
        ];
        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Merge {
                source_ids: vec!["k1".to_string(), "k2".to_string()],
                merged_content: "  ".to_string(),
                segment: MemorySegment::Knowledge,
                importance: 0.5,
            }],
        )
        .await
        .unwrap();
        assert_eq!(outcome.merged, 0);
        assert_eq!(outcome.blocked, 1);
        assert!(repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn a_supersede_archives_the_older_and_keeps_the_newer() {
        let repo = MockMemoryRepository::new();
        let batch = vec![
            frag("old", MemorySegment::Preference, 1),
            frag("new", MemorySegment::Preference, 9),
        ];
        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Supersede {
                newer_id: "new".to_string(),
                older_id: "old".to_string(),
            }],
        )
        .await
        .unwrap();
        assert_eq!(outcome.superseded, 1);
        assert_eq!(outcome.merged, 0, "no merged row is written");
        assert_eq!(
            repo.lifecycle_updates().await,
            vec![("old".to_string(), MemoryLifecycle::Archived)]
        );
    }

    /// The member reads what a run changed from `consolidation_runs.details`.
    #[tokio::test]
    async fn every_change_is_recorded_with_its_rows_text_and_reason() {
        let repo = MockMemoryRepository::new();
        let batch = vec![
            frag("q1", MemorySegment::Knowledge, 1),
            frag("d1", MemorySegment::Preference, 2),
            frag("d2", MemorySegment::Preference, 3),
        ];
        let (outcome, changes) = apply_proposals(
            &repo,
            &batch,
            vec![
                (
                    ConsolidationAction::Prune {
                        id: "q1".to_string(),
                    },
                    "one question, not a trait".to_string(),
                ),
                (
                    ConsolidationAction::Merge {
                        source_ids: vec!["d1".to_string(), "d2".to_string()],
                        merged_content: "likes teal".to_string(),
                        segment: MemorySegment::Preference,
                        importance: 0.6,
                    },
                    "the same colour twice".to_string(),
                ),
            ],
        )
        .await
        .unwrap();
        assert_eq!(outcome.changed(), 2);
        assert_eq!(changes[0].action, "archive");
        assert_eq!(changes[0].reason, "one question, not a trait");
        assert_eq!(changes[0].rows[0].text, batch[0].content);
        assert_eq!(
            repo.lifecycle_updates().await,
            vec![("q1".to_string(), MemoryLifecycle::Archived)],
            "archived, never deleted"
        );
        assert_eq!(changes[1].action, "merge");
        assert_eq!(changes[1].rows.len(), 2);
        assert_eq!(changes[1].into[0].text, "likes teal");

        let details: serde_json::Value =
            serde_json::from_str(&run_details(&changes, None)).unwrap();
        assert_eq!(details["changes"][0]["rows"][0]["id"], "q1");
        assert!(details["trial"].is_null());
    }

    #[tokio::test]
    async fn a_correction_is_never_superseded_by_a_plain_memory() {
        let repo = MockMemoryRepository::new();
        let mut correction = frag("fix", MemorySegment::Correction, 1);
        correction.corrects = Some("was wrong".to_string());
        let batch = vec![correction, frag("plain", MemorySegment::Preference, 9)];
        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Supersede {
                newer_id: "plain".to_string(),
                older_id: "fix".to_string(),
            }],
        )
        .await
        .unwrap();
        assert_eq!(outcome.superseded, 0);
        assert_eq!(outcome.blocked, 1);
    }

    #[tokio::test]
    async fn merge_with_a_kept_source_stays_kept() {
        let repo = MockMemoryRepository::new();
        let batch = vec![kept("p1"), frag("k1", MemorySegment::Knowledge, 6)];

        apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Merge {
                source_ids: vec!["p1".to_string(), "k1".to_string()],
                merged_content: "likes warm light in the evening".to_string(),
                segment: MemorySegment::Preference,
                importance: 0.7,
            }],
        )
        .await
        .unwrap();

        let stored = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        let created = stored
            .iter()
            .find(|m| m.content == "likes warm light in the evening")
            .unwrap();
        assert!(created.is_kept(), "merging must not undo a keep");
        assert_eq!(created.decay_rate, Some(0.0));
    }

    #[tokio::test]
    async fn a_merge_never_joins_different_facts_into_a_paragraph() {
        let repo = MockMemoryRepository::new();
        let filed = |id: &str, about: About| {
            let mut row = frag(id, MemorySegment::Knowledge, 5);
            file_under(&mut row, about);
            row
        };
        let batch = vec![
            filed("name", About::Name),
            filed("work", About::Work),
            filed("tea1", About::Likes),
            filed("tea2", About::Likes),
            frag("loose", MemorySegment::Preference, 9),
        ];
        let merge = |ids: &[&str], text: &str| ConsolidationAction::Merge {
            source_ids: ids.iter().map(|id| id.to_string()).collect(),
            merged_content: text.to_string(),
            segment: MemorySegment::Knowledge,
            importance: 0.5,
        };
        let outcome = apply_actions(
            &repo,
            &batch,
            vec![
                merge(&["name", "work"], "Jerry is an engineer called Jerr."),
                merge(&["tea1", "tea2"], &"Jerry likes tea. ".repeat(30)),
                merge(&["tea1", "tea2", "loose"], "Jerry likes strong tea."),
            ],
        )
        .await
        .unwrap();
        assert_eq!(outcome.blocked, 2, "two filings, then a paragraph");
        assert_eq!(outcome.merged, 1);

        let stored = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        let created = stored
            .iter()
            .find(|m| m.content == "Jerry likes strong tea.")
            .unwrap();
        assert_eq!(
            created.about,
            Some(About::Likes),
            "a merge stays filed where its sources were"
        );
        assert_eq!(created.segment, Some(MemorySegment::Preference));
    }

    #[tokio::test]
    async fn merge_without_a_kept_source_takes_the_segment_tier() {
        let repo = MockMemoryRepository::new();
        let batch = vec![
            frag("k1", MemorySegment::Knowledge, 5),
            frag("k2", MemorySegment::Knowledge, 6),
        ];

        apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Merge {
                source_ids: vec!["k1".to_string(), "k2".to_string()],
                merged_content: "works from home on Fridays".to_string(),
                segment: MemorySegment::Routine,
                importance: 0.65,
            }],
        )
        .await
        .unwrap();

        let stored = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        let created = stored
            .iter()
            .find(|m| m.content == "works from home on Fridays")
            .unwrap();
        assert!(!created.is_kept());
    }

    #[tokio::test]
    async fn split_of_a_kept_memory_keeps_every_piece() {
        let repo = MockMemoryRepository::new();
        let batch = vec![kept("p1")];

        apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Split {
                source_id: "p1".to_string(),
                new_memories: vec![
                    SplitEntry {
                        content: "likes jazz".to_string(),
                        segment: MemorySegment::Preference,
                        importance: 0.7,
                    },
                    SplitEntry {
                        content: "plays piano".to_string(),
                        segment: MemorySegment::Knowledge,
                        importance: 0.5,
                    },
                ],
            }],
        )
        .await
        .unwrap();

        let stored = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        for content in ["likes jazz", "plays piano"] {
            let piece = stored.iter().find(|m| m.content == content).unwrap();
            assert!(piece.is_kept(), "{content} must stay kept");
        }
    }

    // ── apply_actions correction safety ─────────────────────────────────────

    #[tokio::test]
    async fn prune_of_a_correction_is_blocked() {
        let repo = MockMemoryRepository::new();
        let batch = vec![correction("c1", Some("name is not Jeremy"))];

        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Prune {
                id: "c1".to_string(),
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.pruned, 0);
        assert_eq!(outcome.blocked, 1);
        assert!(
            repo.lifecycle_updates().await.is_empty(),
            "a blocked prune must not touch lifecycle"
        );
    }

    #[tokio::test]
    async fn prune_of_a_correction_by_corrects_field_is_blocked() {
        let repo = MockMemoryRepository::new();
        // Correction status can come from `corrects` alone, not just the segment.
        let mut mem = frag("p1", MemorySegment::Preference, 10);
        mem.corrects = Some("used to say coffee".to_string());
        let batch = vec![mem];

        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Prune {
                id: "p1".to_string(),
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.blocked, 1);
        assert!(repo.lifecycle_updates().await.is_empty());
    }

    #[tokio::test]
    async fn ordinary_prune_archives_and_logs() {
        let repo = MockMemoryRepository::new();
        let batch = vec![frag("k1", MemorySegment::Knowledge, 10)];

        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Prune {
                id: "k1".to_string(),
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.pruned, 1);
        assert_eq!(
            repo.lifecycle_updates().await,
            vec![("k1".to_string(), MemoryLifecycle::Archived)]
        );
        assert!(repo
            .events()
            .await
            .iter()
            .any(|(kind, id, _)| matches!(kind, MemoryEventKind::Pruned) && id == "k1"));
    }

    #[tokio::test]
    async fn merge_with_a_correction_source_forces_correction_and_keeps_corrects() {
        let repo = MockMemoryRepository::new();
        let batch = vec![
            correction("c1", Some("name is not Jeremy")),
            frag("k1", MemorySegment::Knowledge, 5),
        ];

        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Merge {
                source_ids: vec!["c1".to_string(), "k1".to_string()],
                merged_content: "name is Jerry".to_string(),
                // The model asked for Knowledge; the guard must override.
                segment: MemorySegment::Knowledge,
                importance: 0.6,
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.merged, 1);
        let stored = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        let created = stored
            .iter()
            .find(|m| m.content == "name is Jerry")
            .expect("merged memory was written");
        assert_eq!(created.segment, Some(MemorySegment::Correction));
        assert_eq!(created.corrects.as_deref(), Some("name is not Jeremy"));

        let superseded = repo.superseded().await;
        assert_eq!(superseded.len(), 2);
        assert!(superseded.iter().all(|(_, by)| by == &created.id));
    }

    #[tokio::test]
    async fn merge_without_a_correction_source_keeps_the_proposed_segment() {
        let repo = MockMemoryRepository::new();
        let batch = vec![
            frag("k1", MemorySegment::Knowledge, 5),
            frag("k2", MemorySegment::Knowledge, 6),
        ];

        apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Merge {
                source_ids: vec!["k1".to_string(), "k2".to_string()],
                merged_content: "prefers dark roast".to_string(),
                segment: MemorySegment::Preference,
                importance: 0.7,
            }],
        )
        .await
        .unwrap();

        let stored = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        let created = stored
            .iter()
            .find(|m| m.content == "prefers dark roast")
            .unwrap();
        assert_eq!(created.segment, Some(MemorySegment::Preference));
        assert!(created.corrects.is_none());
    }

    #[tokio::test]
    async fn split_propagates_corrects_to_the_first_correction_entry() {
        let repo = MockMemoryRepository::new();
        let batch = vec![correction("c1", Some("not a vegetarian"))];

        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Split {
                source_id: "c1".to_string(),
                new_memories: vec![
                    SplitEntry {
                        content: "eats fish".to_string(),
                        segment: MemorySegment::Preference,
                        importance: 0.5,
                    },
                    SplitEntry {
                        content: "is not a vegetarian".to_string(),
                        segment: MemorySegment::Correction,
                        importance: 0.9,
                    },
                    SplitEntry {
                        content: "also not vegan".to_string(),
                        segment: MemorySegment::Correction,
                        importance: 0.8,
                    },
                ],
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.split, 1);
        let stored = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();

        let carrier = stored
            .iter()
            .find(|m| m.content == "is not a vegetarian")
            .unwrap();
        assert_eq!(
            carrier.corrects.as_deref(),
            Some("not a vegetarian"),
            "the first Correction-segment entry inherits corrects"
        );

        // Exactly one entry carries it — not the preference, not the second correction.
        let carriers = stored.iter().filter(|m| m.corrects.is_some()).count();
        assert_eq!(carriers, 1);

        // The source is superseded by the first new memory.
        let superseded = repo.superseded().await;
        assert_eq!(superseded.len(), 1);
        assert_eq!(superseded[0].0, "c1");
    }

    #[tokio::test]
    async fn recategorize_updates_segment_and_logs() {
        let repo = MockMemoryRepository::new();
        let batch = vec![frag("k1", MemorySegment::Knowledge, 5)];

        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Recategorize {
                id: "k1".to_string(),
                new_segment: MemorySegment::Identity,
                new_importance: 0.95,
            }],
        )
        .await
        .unwrap();

        assert_eq!(outcome.recategorized, 1);
        assert_eq!(
            repo.segment_updates().await,
            vec![("k1".to_string(), MemorySegment::Identity, 0.95)]
        );
        assert!(repo
            .events()
            .await
            .iter()
            .any(|(_, id, data)| id == "k1" && data.as_deref() == Some("recategorized")));
    }

    // ── run_consolidation orchestration ─────────────────────────────────────

    struct FixedConsolidator {
        actions: Vec<ConsolidationAction>,
        /// How many memories the consolidator was handed.
        seen: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl MemoryConsolidator for FixedConsolidator {
        async fn consolidate(
            &self,
            memories: &[MemoryFragment],
        ) -> Result<Vec<ConsolidationAction>> {
            self.seen
                .store(memories.len(), std::sync::atomic::Ordering::SeqCst);
            Ok(self.actions.clone())
        }
    }

    #[tokio::test]
    async fn run_consolidation_respects_the_batch_cap_and_audits() {
        let repo = MockMemoryRepository::new();
        for i in 0..30 {
            repo.add(frag(&format!("m{i}"), MemorySegment::Knowledge, i))
                .await
                .unwrap();
        }

        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let consolidator = FixedConsolidator {
            actions: vec![ConsolidationAction::Prune {
                id: "m29".to_string(),
            }],
            seen: seen.clone(),
        };

        let (merged, pruned) = run_consolidation(&consolidator, &repo, 8).await.unwrap();

        assert_eq!(merged, 0);
        assert_eq!(pruned, 1);
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            8,
            "the consolidator must never see more than batch_size memories"
        );

        let runs = repo.consolidation_runs().await;
        assert_eq!(runs.len(), 1, "one audit row per run");
        assert_eq!(runs[0].0, MODE_SINGLE);
        assert_eq!(runs[0].1, 8, "audit records the batch size actually used");
    }

    #[tokio::test]
    async fn run_consolidation_skips_a_tiny_store() {
        let repo = MockMemoryRepository::new();
        for i in 0..3 {
            repo.add(frag(&format!("m{i}"), MemorySegment::Knowledge, i))
                .await
                .unwrap();
        }

        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let consolidator = FixedConsolidator {
            actions: vec![],
            seen: seen.clone(),
        };

        let (merged, pruned) = run_consolidation(&consolidator, &repo, 50).await.unwrap();
        assert_eq!((merged, pruned), (0, 0));
        assert_eq!(
            seen.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no LLM call for a store below the minimum"
        );
        assert!(repo.consolidation_runs().await.is_empty());
    }

    // ── owners ─────────────────────────────────────────────────────────────

    fn owned(id: &str, owner: Option<&str>) -> MemoryFragment {
        let mut f = frag(id, MemorySegment::Knowledge, 5);
        f.profile_id = owner.map(str::to_string);
        f
    }

    async fn written(repo: &MockMemoryRepository, content: &str) -> MemoryFragment {
        repo.search_recent(&ProfileScope::Household, 20)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.content == content)
            .expect("written")
    }

    #[test]
    fn no_batch_holds_two_owners_memories() {
        let memories = vec![
            owned("a1", Some("amina")),
            owned("b1", Some("baraka")),
            owned("h1", None),
            owned("a2", Some("amina")),
        ];
        let batches = select_owner_batches(memories, 50);
        assert_eq!(batches.len(), 3);
        for selection in &batches {
            let owners: std::collections::HashSet<_> =
                selection.batch.iter().map(|m| &m.profile_id).collect();
            assert_eq!(owners.len(), 1, "one owner per batch");
        }
    }

    #[tokio::test]
    async fn a_merge_keeps_its_sources_owner() {
        let repo = MockMemoryRepository::new();
        let batch = vec![owned("a1", Some("amina")), owned("a2", Some("amina"))];
        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Merge {
                source_ids: vec!["a1".into(), "a2".into()],
                merged_content: "amina takes her tea without sugar".into(),
                segment: MemorySegment::Preference,
                importance: 0.6,
            }],
        )
        .await
        .unwrap();
        assert_eq!(outcome.merged, 1);
        let merged = written(&repo, "amina takes her tea without sugar").await;
        assert_eq!(
            merged.profile_id.as_deref(),
            Some("amina"),
            "a member's merged fact stays hers, not the household's"
        );
    }

    #[tokio::test]
    async fn a_merge_across_two_members_is_refused() {
        let repo = MockMemoryRepository::new();
        let batch = vec![owned("a1", Some("amina")), owned("b1", Some("baraka"))];
        let outcome = apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Merge {
                source_ids: vec!["a1".into(), "b1".into()],
                merged_content: "both like tea".into(),
                segment: MemorySegment::Preference,
                importance: 0.6,
            }],
        )
        .await
        .unwrap();
        assert_eq!((outcome.merged, outcome.blocked), (0, 1));
        assert!(repo.superseded().await.is_empty(), "no source is retired");
    }

    #[tokio::test]
    async fn a_split_keeps_its_sources_owner() {
        let repo = MockMemoryRepository::new();
        let batch = vec![owned("b1", Some("baraka"))];
        apply_actions(
            &repo,
            &batch,
            vec![ConsolidationAction::Split {
                source_id: "b1".into(),
                new_memories: vec![SplitEntry {
                    content: "baraka cycles to work".into(),
                    segment: MemorySegment::Knowledge,
                    importance: 0.5,
                }],
            }],
        )
        .await
        .unwrap();
        let piece = written(&repo, "baraka cycles to work").await;
        assert_eq!(piece.profile_id.as_deref(), Some("baraka"));
    }

    #[tokio::test]
    async fn an_action_outside_the_batch_is_refused() {
        let repo = MockMemoryRepository::new();
        let batch = vec![owned("a1", Some("amina"))];
        let outcome = apply_actions(
            &repo,
            &batch,
            vec![
                ConsolidationAction::Prune { id: "b1".into() },
                ConsolidationAction::Recategorize {
                    id: "b1".into(),
                    new_segment: MemorySegment::Identity,
                    new_importance: 0.9,
                },
                ConsolidationAction::Merge {
                    source_ids: vec!["a1".into(), "b1".into()],
                    merged_content: "x".into(),
                    segment: MemorySegment::Knowledge,
                    importance: 0.5,
                },
            ],
        )
        .await
        .unwrap();
        assert_eq!(outcome.changed(), 0);
        assert_eq!(
            outcome.blocked, 3,
            "another member's memory is out of reach"
        );
        assert!(repo.lifecycle_updates().await.is_empty());
        assert!(repo.segment_updates().await.is_empty());
    }
}
