//! Per-SESSION tool-relevance selection (Phase D2): which `giap-*` groups have their tool
//! SCHEMAS in the prompt, for prompt cost only; the model still decides tool use natively.
//! Runs once per session, never per turn — per-turn churn rewrites the tools JSON and
//! destroys the engine's KV prefix reuse. Every failure path widens, never narrows.

use crate::mcp::domain::tool_group::{
    core_group_names, find_group, group_of_tool, is_catalog_extension, TOOLKIT_EXTENSION,
    TOOL_GROUPS,
};

/// Cosine-similarity floor for including a non-core group, on all-MiniLM-L6-v2. Calibrated
/// for short text: unrelated pairs land ~0.00-0.15, clearly on-topic above ~0.40. 0.28 sits
/// below that band because a surplus group costs ~100-1200 prompt tokens and a missing one
/// costs a wasted round trip, ~10s on-device.
pub const DEFAULT_RELEVANCE_THRESHOLD: f32 = 0.28;

/// Cosine similarity of the session's opening context against one group.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupScore {
    pub extension: String,
    pub score: f32,
}

/// How the selection was reached — surfaced in the trace line so the saving (or
/// the absence of one) is explainable after the fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionBasis {
    /// `tool_selection_mode = "all"`, or nothing to select from.
    ModeAll,
    /// No embedder available, or embedding/scoring failed — widened to all.
    NoEmbedder,
    /// Scored against group descriptions.
    Scored,
    /// `tool_selection_mode = "minimal"`: the toolkit hatch only, no scoring.
    ModeMinimal,
}

/// The outcome of selection for one session.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSelection {
    /// Extension names whose tools go in the prompt. Sorted, deduped.
    pub groups: Vec<String>,
    pub basis: SelectionBasis,
}

impl ToolSelection {
    /// All registered groups — the "no narrowing" answer.
    pub fn all(available: &[String], basis: SelectionBasis) -> Self {
        let mut groups = available.to_vec();
        groups.sort();
        groups.dedup();
        Self { groups, basis }
    }
}

/// Upper bound on any single embedded signal. Embedding models truncate anyway.
const MAX_SIGNAL_CHARS: usize = 2000;

/// The texts representing a session's topic — the question, the standing context, then
/// each active skill's "name: description" — scored INDEPENDENTLY and combined with `max`,
/// never concatenated. A short question embedded with a kilobyte of memories yields a
/// vector the memories dominate: live, "Check the weather" selected `giap-schedule`.
pub fn selection_signals(first_message: &str, memories: &str, skills: &str) -> Vec<String> {
    [first_message, memories, skills]
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.len() <= MAX_SIGNAL_CHARS {
                return s.to_string();
            }
            // Truncate on a char boundary — the signal is user text.
            let cut = s
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|i| *i <= MAX_SIGNAL_CHARS)
                .last()
                .unwrap_or(0);
            s[..cut].to_string()
        })
        .collect()
}

/// Combine per-signal scores into one score per group by taking the best.
///
/// `max` and not a mean: the signals are alternative descriptions of the session's topic,
/// not parts of one, so averaging would reintroduce the dilution this split exists to fix.
pub fn merge_scores(per_signal: &[Vec<GroupScore>]) -> Vec<GroupScore> {
    let mut merged: Vec<GroupScore> = Vec::new();
    for scores in per_signal {
        for s in scores {
            match merged.iter_mut().find(|m| m.extension == s.extension) {
                Some(m) => m.score = m.score.max(s.score),
                None => merged.push(s.clone()),
            }
        }
    }
    merged
}

/// The group descriptions to embed, intersected with what is registered.
///
/// Core groups are excluded: they are never scored, so embedding them would be
/// wasted work.
pub fn scorable_groups(available: &[String]) -> Vec<(&'static str, &'static str)> {
    TOOL_GROUPS
        .iter()
        .filter(|g| !g.core)
        .filter(|g| available.iter().any(|a| a == g.extension))
        .map(|g| (g.extension, g.description))
        .collect()
}

/// Choose the groups for a session. `available` is what is registered this run; `scores`
/// is `None` when no embedder was available or scoring failed. Guarantees, in precedence
/// order: no scores means every available group; core groups always when registered; every
/// group at or above `threshold`; and the best-scoring non-core group even if it missed.
pub fn select_groups(
    available: &[String],
    scores: Option<&[GroupScore]>,
    threshold: f32,
) -> ToolSelection {
    if available.is_empty() {
        return ToolSelection::all(available, SelectionBasis::ModeAll);
    }
    let Some(scores) = scores else {
        return ToolSelection::all(available, SelectionBasis::NoEmbedder);
    };

    let mut chosen: Vec<String> = core_group_names()
        .into_iter()
        .filter(|c| available.iter().any(|a| a == c))
        .map(|c| c.to_string())
        .collect();

    // Anything registered that the catalog does not describe is a user-added
    // external MCP extension: never narrowed, because the user added it on
    // purpose and there is no description to score it against.
    for ext in available {
        if !is_catalog_extension(ext) {
            chosen.push(ext.clone());
        }
    }

    for s in scores {
        if s.score >= threshold && available.iter().any(|a| a == &s.extension) {
            chosen.push(s.extension.clone());
        }
    }

    // Rule 4: rescue the top scorer regardless of the threshold.
    if let Some(best) = scores
        .iter()
        .filter(|s| available.iter().any(|a| a == &s.extension))
        .filter(|s| find_group(&s.extension).is_some_and(|g| !g.core))
        .max_by(|a, b| a.score.total_cmp(&b.score))
    {
        chosen.push(best.extension.clone());
    }

    chosen.sort();
    chosen.dedup();
    ToolSelection {
        groups: chosen,
        basis: SelectionBasis::Scored,
    }
}

/// The `"minimal"` answer: the toolkit escape hatch, and nothing else.
///
/// No scoring, no embedder, no core set. Two tools — `list_tool_groups` and
/// `enable_tool_group` — are 222 tokens of the 8,192-token local prompt budget,
/// which is the only shape that fits a 4% ceiling. Everything else is one
/// `enable_tool_group` call away and, once enabled, is persisted for the
/// session like any other loaded group.
///
/// `available` is the PERMITTED set, so a scope that denies the toolkit gets an
/// empty selection rather than a hatch it is not entitled to. That is a pond
/// with no tools at all, which is the honest outcome: the alternative is
/// handing back a hatch whose every destination is denied.
pub fn minimal_groups(available: &[String]) -> ToolSelection {
    let groups = available
        .iter()
        .filter(|a| a.as_str() == TOOLKIT_EXTENSION)
        .cloned()
        .collect();
    ToolSelection {
        groups,
        basis: SelectionBasis::ModeMinimal,
    }
}

/// Keep only the groups a scope is permitted to hold.
///
/// One function because there are three places that must apply this rule — the
/// in-process cache, the persisted row, and anything that restores a group set
/// from elsewhere — and writing it three times is how one of them came to be
/// missing it. The cache path returned its entry unclamped, so an Owner turn
/// could cache the wide list and a later Guest turn on the same session got it
/// back.
///
/// Scope is re-derived every turn and narrows on failure (a transient device
/// read falls through to Guest), so "the cache was filled by a wider speaker"
/// is an ordinary case, not an exotic one.
pub fn clamp_to_permitted(groups: Vec<String>, permitted: &[String]) -> Vec<String> {
    groups
        .into_iter()
        .filter(|g| permitted.iter().any(|p| p == g))
        .collect()
}

/// Retain only the tools belonging to `groups`.
///
/// A tool with no `__` prefix, or whose prefix is not a catalog extension, is KEPT: the
/// first is goose plumbing the shim's allow-set governs, the second a user-added server.
pub fn filter_tools_by_groups<'a, I>(tools: I, groups: &[String]) -> Vec<String>
where
    I: IntoIterator<Item = &'a String>,
{
    tools
        .into_iter()
        .filter(|name| match group_of_tool(name) {
            Some(ext) if is_catalog_extension(ext) => groups.iter().any(|g| g == ext),
            _ => true,
        })
        .cloned()
        .collect()
}

/// Remove every tool a `Guest` must never reach; the list is in
/// [`crate::mcp::domain::tool_group::groups_denied_to_guests`]. Filters TOOLS, because the
/// group-level subtraction runs only under `tool_selection_narrows()` — "relevant" or
/// "minimal" (default
/// "all"). A prompt-surface control: a real gate needs goose's private `add_inspector`.
pub fn subtract_guest_denied_tools<'a, I>(tools: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a String>,
{
    let denied = crate::mcp::domain::tool_group::groups_denied_to_guests();
    tools
        .into_iter()
        .filter(|name| match group_of_tool(name) {
            // An unprefixed name belongs to no group and cannot be matched
            // against the denylist. Keeping it is the widening choice, but a
            // tool with no group is a platform tool, not personal data.
            None => true,
            // A CATALOG extension is judged by the denylist, which is a list of
            // `giap-*` literals.
            Some(ext) if is_catalog_extension(ext) => !denied.contains(&ext),
            // Engine plumbing. Not personal data, and the shim's allow-set
            // governs it anyway — same reasoning as the unprefixed case.
            Some(ext) if ENGINE_TOOL_PREFIXES.contains(&ext) => true,
            // Anything else with a prefix is a user-added MCP server the `giap-*` denylist
            // can never name, so default-deny: the honest answer about a server whose data
            // GIAP knows nothing about is no. Consent is a per-server flag. The subagent
            // side needs no equivalent — `TaskSpec::grants_tool` already denies unknowns.
            Some(_) => false,
        })
        .cloned()
        .collect()
}

/// Tool-name prefixes belonging to the AGENT ENGINE rather than any extension. goose
/// injects a few of its own tools; they are not personal data and the provider shim's
/// allow-set already vetoes them. Named explicitly because [`subtract_guest_denied_tools`]
/// otherwise default-denies every non-catalog prefix, which would look like a bug.
const ENGINE_TOOL_PREFIXES: &[&str] = &["platform", "recipe", "dynamic_task"];

/// The groups a speaker with this scope may EVER hold: the PAI-1 boundary as a value, so
/// no caller derives it independently. Applied to the candidates going INTO
/// [`select_groups`] rather than subtracted after, which works because `select_groups`
/// filters the core set by `available`. A denylist, like `groups_denied_to_guests`.
pub fn permitted_groups(
    available: &[String],
    scope: &crate::user_data::domain::profile::ProfileScope,
) -> Vec<String> {
    if !scope.excludes_everything() {
        return available.to_vec();
    }
    let denied = crate::mcp::domain::tool_group::groups_denied_to_guests();
    available
        .iter()
        .filter(|e| !denied.contains(&e.as_str()))
        .cloned()
        .collect()
}

/// The `<tool-groups>` block for the session's system appendix, listing the groups
/// that are NOT loaded so the model can call `enable_tool_group` without a round trip on
/// `list_tool_groups`. It rides the user message because the system prefix must stay
/// byte-identical across sessions for KV reuse. Empty string when nothing is dormant.
pub fn dormant_groups_note(available: &[String], loaded: &[String]) -> String {
    let dormant: Vec<&'static crate::mcp::domain::tool_group::ToolGroup> = TOOL_GROUPS
        .iter()
        .filter(|g| available.iter().any(|a| a == g.extension))
        .filter(|g| !loaded.iter().any(|l| l == g.extension))
        .collect();
    if dormant.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(64 + dormant.len() * 80);
    out.push_str("<tool-groups>\n");
    out.push_str(
        "These extra tool groups exist but are not loaded right now. If you need one, call \
         enable_tool_group with its name and its tools become available immediately.\n",
    );
    for g in dormant {
        // First sentence only — enough to choose by, a fraction of the tokens.
        let gist = g
            .description
            .split_once(':')
            .map(|(head, _)| head)
            .unwrap_or(g.description);
        out.push_str("- ");
        out.push_str(g.extension);
        out.push_str(": ");
        out.push_str(gist.trim());
        out.push('\n');
    }
    out.push_str("</tool-groups>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available() -> Vec<String> {
        TOOL_GROUPS
            .iter()
            .map(|g| g.extension.to_string())
            .collect()
    }

    fn score(ext: &str, score: f32) -> GroupScore {
        GroupScore {
            extension: ext.to_string(),
            score,
        }
    }

    /// The fallback that makes narrowing safe: no embedder means no narrowing.
    #[test]
    fn no_scores_falls_back_to_every_group() {
        let avail = available();
        let sel = select_groups(&avail, None, DEFAULT_RELEVANCE_THRESHOLD);
        assert_eq!(sel.basis, SelectionBasis::NoEmbedder);
        assert_eq!(sel.groups.len(), avail.len());
        for ext in &avail {
            assert!(sel.groups.contains(ext), "{ext} missing from fallback");
        }
    }

    #[test]
    fn empty_availability_is_not_a_crash() {
        let sel = select_groups(&[], None, DEFAULT_RELEVANCE_THRESHOLD);
        assert!(sel.groups.is_empty());
        assert_eq!(sel.basis, SelectionBasis::ModeAll);
    }

    /// Core groups survive even when every score is zero.
    /// "minimal" offers the hatch and nothing else -- not the core set, which
    /// is 778 tokens and 9.5% of the local prompt budget on its own.
    /// The asymmetry that made this a function: a wider speaker's cached set
    /// must not survive into a narrower speaker's turn.
    #[test]
    fn a_wider_speakers_groups_do_not_survive_a_narrower_turn() {
        let owner_cached = vec![
            "giap-memory".to_string(),
            "giap-sensors".to_string(),
            "giap-weather".to_string(),
        ];
        // What a Guest is permitted: memory and sensors are personal-data groups.
        let guest_permitted = vec!["giap-weather".to_string(), "giap-toolkit".to_string()];

        let clamped = clamp_to_permitted(owner_cached, &guest_permitted);

        assert_eq!(clamped, vec!["giap-weather".to_string()]);
        assert!(
            !clamped.iter().any(|g| g == "giap-memory"),
            "a personal-data group reached a guest through the cache"
        );
    }

    /// The other direction, so the clamp cannot pass by refusing everything.
    #[test]
    fn clamping_against_a_wider_ceiling_keeps_everything() {
        let held = vec!["giap-weather".to_string(), "giap-memory".to_string()];
        let permitted = vec![
            "giap-weather".to_string(),
            "giap-memory".to_string(),
            "giap-sensors".to_string(),
        ];
        assert_eq!(clamp_to_permitted(held.clone(), &permitted), held);
    }

    #[test]
    fn minimal_offers_the_hatch_and_nothing_else() {
        let available = vec![
            "giap-memory".to_string(),
            "giap-system".to_string(),
            TOOLKIT_EXTENSION.to_string(),
            "giap-weather".to_string(),
        ];

        let sel = minimal_groups(&available);

        assert_eq!(sel.groups, vec![TOOLKIT_EXTENSION.to_string()]);
        assert_eq!(sel.basis, SelectionBasis::ModeMinimal);
        // The core groups specifically: they are the ones every other narrowing
        // path puts back unconditionally, so they are what would leak in here.
        for core in core_group_names() {
            if core != TOOLKIT_EXTENSION {
                assert!(
                    !sel.groups.iter().any(|g| g == core),
                    "{core} is core, but minimal offers the hatch alone"
                );
            }
        }
    }

    /// A scope that is not entitled to the toolkit gets NO hatch, not a hatch
    /// whose every destination is denied.
    ///
    /// `available` is the permitted set, so this is the guest/denied case. The
    /// result is a pond with no tools at all, which is the honest outcome --
    /// and it is a one-line `filter`, which is exactly the kind of line that
    /// gets rewritten into `unwrap_or(TOOLKIT_EXTENSION)` by someone who reads
    /// the empty vector as a bug.
    #[test]
    fn minimal_offers_nothing_when_the_hatch_is_not_permitted() {
        let available = vec!["giap-weather".to_string(), "giap-memory".to_string()];

        let sel = minimal_groups(&available);

        assert!(
            sel.groups.is_empty(),
            "the toolkit is not permitted here, so there is no hatch to offer: {:?}",
            sel.groups
        );
        assert_eq!(sel.basis, SelectionBasis::ModeMinimal);
    }

    /// Nothing registered is not a crash, and not a widening either.
    #[test]
    fn minimal_on_an_empty_availability_is_empty() {
        assert!(minimal_groups(&[]).groups.is_empty());
    }

    #[test]
    fn core_groups_are_always_present() {
        let avail = available();
        let scores: Vec<GroupScore> = scorable_groups(&avail)
            .iter()
            .map(|(e, _)| score(e, 0.0))
            .collect();
        let sel = select_groups(&avail, Some(&scores), DEFAULT_RELEVANCE_THRESHOLD);
        for c in core_group_names() {
            assert!(sel.groups.contains(&c.to_string()), "core {c} was dropped");
        }
    }

    /// The point of the feature: a weather question does not load all 27 tools.
    #[test]
    fn a_single_relevant_group_narrows_hard() {
        let avail = available();
        let mut scores: Vec<GroupScore> = scorable_groups(&avail)
            .iter()
            .map(|(e, _)| score(e, 0.05))
            .collect();
        for s in scores.iter_mut() {
            if s.extension == "giap-weather" {
                s.score = 0.61;
            }
        }
        let sel = select_groups(&avail, Some(&scores), DEFAULT_RELEVANCE_THRESHOLD);
        assert_eq!(sel.basis, SelectionBasis::Scored);
        // core + weather, and nothing else (the count comes from
        // `core_group_names()` below rather than a literal, which is why this
        // survived the core set going from four groups to three).
        assert_eq!(sel.groups.len(), core_group_names().len() + 1);
        assert!(sel.groups.contains(&"giap-weather".to_string()));
        assert!(!sel.groups.contains(&"giap-schedule".to_string()));
    }

    #[test]
    fn every_group_above_threshold_is_included() {
        let avail = available();
        let scores = vec![
            score("giap-device", 0.44),
            score("giap-device-control", 0.41),
            score("giap-schedule", 0.30),
            score("giap-knowledge", 0.02),
        ];
        let sel = select_groups(&avail, Some(&scores), DEFAULT_RELEVANCE_THRESHOLD);
        for want in ["giap-device", "giap-device-control", "giap-schedule"] {
            assert!(sel.groups.contains(&want.to_string()), "{want} missing");
        }
    }

    /// Rule 4: the best non-core scorer rides along even below the threshold,
    /// so a merely under-rated capability does not cost a round trip.
    #[test]
    fn top_scorer_is_rescued_below_threshold() {
        let avail = available();
        let scores = vec![
            score("giap-weather", 0.19),
            score("giap-knowledge", 0.11),
            score("giap-sensors", 0.03),
        ];
        let sel = select_groups(&avail, Some(&scores), DEFAULT_RELEVANCE_THRESHOLD);
        assert!(
            sel.groups.contains(&"giap-weather".to_string()),
            "top scorer should be rescued"
        );
        assert!(!sel.groups.contains(&"giap-knowledge".to_string()));
    }

    /// Selection only ever picks from what is registered — a disabled extension
    /// cannot be resurrected by a high score.
    #[test]
    fn unavailable_groups_are_never_selected() {
        // A registered, non-core group that no score names. It must be
        // `giap-memory`-free: the last assertion below is precisely that a core
        // group which is NOT registered stays out, so putting one in `avail`
        // would delete the property being tested.
        let avail: Vec<String> = vec![
            "giap-knowledge".into(),
            "giap-toolkit".into(),
            "giap-weather".into(),
        ];
        let scores = vec![score("giap-schedule", 0.99), score("giap-weather", 0.50)];
        let sel = select_groups(&avail, Some(&scores), DEFAULT_RELEVANCE_THRESHOLD);
        assert!(!sel.groups.contains(&"giap-schedule".to_string()));
        assert!(sel.groups.contains(&"giap-weather".to_string()));
        // giap-memory / giap-system are core but not registered here.
        assert!(!sel.groups.contains(&"giap-memory".to_string()));
    }

    // ── The PAI-1 boundary ────────────────────────────────────────────────

    /// A guest may not hold a personal-data group, and may not be shown one: both halves
    /// in one test because they were one bug. Groups withheld from the selection were then
    /// advertised anyway by `dormant_groups_note`, under a sentence telling the model that
    /// enabling one makes its tools available immediately.
    #[test]
    fn a_guest_is_neither_given_nor_offered_a_personal_group() {
        use crate::user_data::domain::profile::ProfileScope;

        let all = available();
        let denied = crate::mcp::domain::tool_group::groups_denied_to_guests();
        assert!(
            !denied.is_empty(),
            "the guest denylist is empty, so this test would pass against anything"
        );

        let permitted = permitted_groups(&all, &ProfileScope::Guest);

        // Held: nothing denied survives into the ceiling.
        for d in denied {
            assert!(
                !permitted.iter().any(|p| p == d),
                "'{d}' is denied to guests but is in the permitted set"
            );
        }
        assert!(
            permitted.len() < all.len(),
            "the guest ceiling is the whole catalog — the subtraction did nothing"
        );

        // Offered: the note may only name groups from the ceiling. Loaded is
        // empty, which is the worst case — everything permitted is dormant.
        let note = dormant_groups_note(&permitted, &[]);
        assert!(
            !note.is_empty(),
            "no note rendered, so the assertions below prove nothing"
        );
        for d in denied {
            assert!(
                !note.contains(d),
                "the dormant-groups note offers '{d}' to a guest:\n{note}"
            );
        }
    }

    /// An identified speaker loses nothing. The boundary is for guests only.
    #[test]
    fn an_identified_speaker_keeps_the_whole_catalog() {
        use crate::user_data::domain::profile::ProfileScope;

        let all = available();
        for scope in [
            ProfileScope::Household,
            ProfileScope::Owner("member-1".to_string()),
        ] {
            let permitted = permitted_groups(&all, &scope);
            assert_eq!(
                permitted.len(),
                all.len(),
                "{scope:?} lost groups it is entitled to"
            );
        }
    }

    /// The ceiling bounds selection, including the core groups: `select_groups` filters
    /// `core_group_names()` by `available`, so passing the guest ceiling as `available`
    /// keeps `giap-memory` out even though it is core.
    #[test]
    fn selecting_from_the_guest_ceiling_drops_even_core_groups() {
        use crate::mcp::domain::tool_group::core_group_names;
        use crate::user_data::domain::profile::ProfileScope;

        let permitted = permitted_groups(&available(), &ProfileScope::Guest);
        let denied = crate::mcp::domain::tool_group::groups_denied_to_guests();

        // Precondition the whole approach rests on: at least one core group is
        // denied to guests. If that stopped being true this test would be
        // measuring nothing.
        let denied_core: Vec<&str> = core_group_names()
            .into_iter()
            .filter(|c| denied.contains(c))
            .collect();
        assert!(
            !denied_core.is_empty(),
            "no core group is denied to guests, so 'the pre-filter also bounds \
             core groups' is untested"
        );

        // No embedder — the widening path, which is what a fresh Jetson takes.
        let selection = select_groups(&permitted, None, DEFAULT_RELEVANCE_THRESHOLD);
        for c in &denied_core {
            assert!(
                !selection.groups.iter().any(|g| g == c),
                "core group '{c}' came back for a guest despite the ceiling"
            );
        }
    }

    /// A user-added MCP server is not in the catalog, so selection leaves it be.
    #[test]
    fn external_extensions_are_never_narrowed() {
        let mut avail = available();
        avail.push("my-home-assistant-mcp".to_string());
        let scores: Vec<GroupScore> = scorable_groups(&avail)
            .iter()
            .map(|(e, _)| score(e, 0.0))
            .collect();
        let sel = select_groups(&avail, Some(&scores), DEFAULT_RELEVANCE_THRESHOLD);
        assert!(sel.groups.contains(&"my-home-assistant-mcp".to_string()));
    }

    #[test]
    fn scorable_groups_excludes_core_and_unregistered() {
        // A user-added MCP server stands in for "registered but not in the
        // catalog". This said `giap-draft`, which was a catalog group until it
        // was deleted -- after which the fixture was exercising the external
        // path while still reading as a test about a GIAP group.
        let avail: Vec<String> = vec![
            "some-user-mcp-server".into(),
            "giap-memory".into(),
            "giap-weather".into(),
        ];
        let scorable = scorable_groups(&avail);
        assert_eq!(scorable.len(), 1);
        assert_eq!(scorable[0].0, "giap-weather");
    }

    #[test]
    fn tool_filter_keeps_only_selected_groups() {
        let tools: Vec<String> = vec![
            "giap-weather__get_forecast".into(),
            "giap-weather__get_current_weather".into(),
            "giap-schedule__create_schedule".into(),
            "giap-memory__recall_memories".into(),
        ];
        let groups: Vec<String> = vec!["giap-weather".into(), "giap-memory".into()];
        let kept = filter_tools_by_groups(&tools, &groups);
        assert_eq!(kept.len(), 3);
        assert!(!kept.iter().any(|t| t.starts_with("giap-schedule")));
    }

    #[test]
    fn tool_filter_keeps_unknown_and_unprefixed_tools() {
        let tools: Vec<String> = vec![
            "my-mcp__do_thing".into(),
            "unprefixed_tool".into(),
            "giap-knowledge__compute_answer".into(),
        ];
        let groups: Vec<String> = vec!["giap-weather".into()];
        let kept = filter_tools_by_groups(&tools, &groups);
        assert!(kept.contains(&"my-mcp__do_thing".to_string()));
        assert!(kept.contains(&"unprefixed_tool".to_string()));
        assert!(!kept.contains(&"giap-knowledge__compute_answer".to_string()));
    }

    /// The regression this split exists for: the question and the standing context must
    /// reach the embedder as SEPARATE texts. Concatenated, a short question is drowned by
    /// a long memory block — live, "Check the weather" selected `giap-schedule` and left
    /// `giap-weather` dormant.
    #[test]
    fn question_and_memories_are_scored_separately() {
        let sigs = selection_signals("Check the weather", "- [identity] lives in Nairobi", "");
        assert_eq!(sigs.len(), 2);
        assert_eq!(sigs[0], "Check the weather", "the question stands alone");
        assert!(sigs[1].contains("Nairobi"));
        assert!(
            !sigs[0].contains("Nairobi"),
            "the memories must never be folded into the question"
        );
    }

    #[test]
    fn an_empty_half_is_dropped_not_embedded() {
        assert_eq!(selection_signals("hi", "   ", ""), vec!["hi".to_string()]);
        assert_eq!(
            selection_signals("  ", "mems", ""),
            vec!["mems".to_string()]
        );
        assert!(selection_signals(" ", "", "").is_empty());
    }

    #[test]
    fn skills_signal_is_scored_independently() {
        let sigs = selection_signals(
            "hi",
            "",
            "task-reminder: Creates reminders using the scheduler",
        );
        assert_eq!(sigs.len(), 2);
        assert!(sigs[1].contains("scheduler"));
    }

    #[test]
    fn signals_are_bounded_and_utf8_safe() {
        let memories = "e\u{301}".repeat(4000); // multi-byte, well over the cap
        let sigs = selection_signals("hi", &memories, "");
        assert_eq!(sigs.len(), 2);
        assert!(sigs[1].len() <= 2001);
        // Truncation must not have split a char — the String is valid by
        // construction, so re-validating its chars is the assertion.
        assert!(sigs[1].chars().count() > 0);
    }

    /// Merging takes the best per group, not the mean — averaging would put the
    /// dilution back.
    #[test]
    fn merge_takes_the_best_score_per_group() {
        let a = vec![
            GroupScore {
                extension: "giap-weather".into(),
                score: 0.61,
            },
            GroupScore {
                extension: "giap-schedule".into(),
                score: 0.10,
            },
        ];
        let b = vec![
            GroupScore {
                extension: "giap-weather".into(),
                score: 0.05,
            },
            GroupScore {
                extension: "giap-schedule".into(),
                score: 0.31,
            },
        ];
        let merged = merge_scores(&[a, b]);
        let get = |e: &str| merged.iter().find(|m| m.extension == e).unwrap().score;
        assert!((get("giap-weather") - 0.61).abs() < 1e-6);
        assert!((get("giap-schedule") - 0.31).abs() < 1e-6);
    }

    /// End to end on the live failure: with the question scored on its own, the
    /// weather group clears the bar and is selected.
    #[test]
    fn a_specific_question_selects_its_group_despite_unrelated_memories() {
        let avail = available();
        // Question signal: strongly on-topic for weather. Memory signal: noise
        // about other things, mildly resembling schedule.
        let question = vec![
            GroupScore {
                extension: "giap-weather".into(),
                score: 0.62,
            },
            GroupScore {
                extension: "giap-schedule".into(),
                score: 0.08,
            },
        ];
        let mems = vec![
            GroupScore {
                extension: "giap-weather".into(),
                score: 0.04,
            },
            GroupScore {
                extension: "giap-schedule".into(),
                score: 0.17,
            },
        ];
        let merged = merge_scores(&[question, mems]);
        let sel = select_groups(&avail, Some(&merged), DEFAULT_RELEVANCE_THRESHOLD);
        assert!(
            sel.groups.contains(&"giap-weather".to_string()),
            "got {:?}",
            sel.groups
        );
    }

    /// An active skill's own description is a selection signal in its own right: an
    /// unrelated opening message plus a skill that needs `giap-schedule` still selects
    /// `giap-schedule`, because a skill telling the model to call a tool is worthless if
    /// that tool's schema was never in the prompt to begin with.
    #[test]
    fn a_skill_signal_selects_its_group_despite_an_unrelated_question() {
        let avail = available();
        let question = vec![
            GroupScore {
                extension: "giap-schedule".into(),
                score: 0.03,
            },
            GroupScore {
                extension: "giap-weather".into(),
                score: 0.02,
            },
        ];
        let skill = vec![
            GroupScore {
                extension: "giap-schedule".into(),
                score: 0.58,
            },
            GroupScore {
                extension: "giap-weather".into(),
                score: 0.01,
            },
        ];
        let merged = merge_scores(&[question, skill]);
        let sel = select_groups(&avail, Some(&merged), DEFAULT_RELEVANCE_THRESHOLD);
        assert!(
            sel.groups.contains(&"giap-schedule".to_string()),
            "got {:?}",
            sel.groups
        );
        assert!(!sel.groups.contains(&"giap-weather".to_string()));
    }

    #[test]
    fn dormant_note_lists_only_unloaded_groups() {
        let avail = available();
        let loaded: Vec<String> = core_group_names().iter().map(|c| c.to_string()).collect();
        let note = dormant_groups_note(&avail, &loaded);
        assert!(note.contains("<tool-groups>"));
        assert!(note.contains("giap-weather"));
        assert!(
            !note.contains("- giap-memory"),
            "loaded groups must not be advertised as dormant"
        );
        assert!(note.contains("enable_tool_group"));
    }

    /// With everything loaded there is nothing to advertise, so the note costs
    /// zero tokens — important for the default "all" mode.
    #[test]
    fn dormant_note_is_empty_when_nothing_is_dormant() {
        let avail = available();
        assert!(dormant_groups_note(&avail, &avail).is_empty());
    }

    // ── PAI-1 P5: the guest denial, at the tool level ───────────────────────

    fn tools(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// The defect this function exists for. `tool_selection_mode` defaults to
    /// "all", which skips the whole selection path where the group-level
    /// subtraction lives -- so the unfiltered set reached the model and a Guest
    /// kept every memory tool. Nothing below depends on selection running.
    #[test]
    fn guest_denied_tools_are_removed_from_an_unfiltered_set() {
        // Only tools whose GROUP still exists. `giap-draft__approve_draft`,
        // `giap-audit__list_egress` and `giap-vision__describe_scene` were in
        // this fixture after their groups were deleted, and they were still
        // withheld -- but by the default-deny-unknown-prefix arm below, not by
        // the denylist this test names. The assertion passed while guarding
        // nothing.
        let all = tools(&[
            "giap-memory__recall_memories",
            "giap-memory__forget_memory",
            "giap-memory__keyword_search",
            "giap-sensors__read_sensor",
            "giap-weather__get_forecast",
            "giap-toolkit__enable_tool_group",
        ]);
        for tool in &all {
            let group = group_of_tool(tool).expect("fixture tools are prefixed");
            assert!(
                is_catalog_extension(group),
                "{tool}'s group is gone, so this fixture would exercise the \
                 unknown-prefix arm rather than the denylist"
            );
        }

        let kept = subtract_guest_denied_tools(all.iter());

        for personal in [
            "giap-memory__recall_memories",
            "giap-memory__forget_memory",
            "giap-memory__keyword_search",
            "giap-sensors__read_sensor",
        ] {
            assert!(
                !kept.iter().any(|t| t == personal),
                "{personal} must not reach a guest"
            );
        }

        // A visitor keeps a useful assistant: the denial is targeted, not a
        // blanket refusal to work.
        assert!(kept.iter().any(|t| t == "giap-weather__get_forecast"));
        assert!(kept.iter().any(|t| t == "giap-toolkit__enable_tool_group"));
    }

    /// Assert the positive case too, not only the boundary. Three vacuous-test
    /// incidents in this programme were all an assertion that held trivially
    /// because the fixture was empty or wrong.
    #[test]
    fn a_non_guest_set_is_returned_intact() {
        let all = tools(&["giap-memory__recall_memories", "giap-weather__get_forecast"]);
        // The function is the guest branch; the caller decides when to apply it.
        // What it must never do is drop something that is not on the denylist.
        let kept = subtract_guest_denied_tools(all.iter());
        assert!(
            kept.iter().any(|t| t == "giap-weather__get_forecast"),
            "a non-personal tool was dropped: {kept:?}"
        );
        assert_eq!(kept.len(), 1, "exactly the one denied tool should go");
    }

    #[test]
    fn subtracting_twice_equals_subtracting_once() {
        let all = tools(&["giap-memory__recall_memories", "giap-weather__get_forecast"]);
        let once = subtract_guest_denied_tools(all.iter());
        let twice = subtract_guest_denied_tools(once.iter());
        assert_eq!(
            once, twice,
            "must be idempotent -- it runs after a path that may already have subtracted at the group level"
        );
    }

    /// An unprefixed tool has no group to match against the denylist. Keeping
    /// it is the widening choice, so it is stated explicitly rather than left
    /// to be discovered.
    #[test]
    fn an_ungrouped_tool_is_kept() {
        let all = tools(&["final_output", "platform__final_output"]);
        assert_eq!(
            subtract_guest_denied_tools(all.iter()).len(),
            2,
            "engine plumbing was dropped for a guest — it is not personal data, \
             and the shim's allow-set already governs it"
        );
    }

    /// A user-added MCP server is default-DENIED to a guest, the asymmetry with the line
    /// above: `groups_denied_to_guests()` holds `giap-*` literals only, so a non-catalog
    /// prefix can never appear in it and a membership check would be structurally `true`.
    /// Withholding can be walked back with a per-server flag; the other direction cannot.
    #[test]
    fn a_third_party_server_is_withheld_from_a_guest() {
        let all = tools(&[
            "acme-mail__read_inbox",
            "giap-weather__get_forecast",
            "platform__manage_schedule",
        ]);
        let kept = subtract_guest_denied_tools(all.iter());
        assert!(
            !kept.iter().any(|t| t.starts_with("acme-mail__")),
            "a user-added MCP server survived the guest boundary: {kept:?}"
        );
        assert!(
            kept.iter().any(|t| t.starts_with("giap-weather__")),
            "a permitted builtin was dropped: {kept:?}"
        );
        assert!(
            kept.iter().any(|t| t.starts_with("platform__")),
            "engine plumbing was dropped: {kept:?}"
        );
    }

    /// Every name in the denylist must actually be a group the catalog knows,
    /// or the subtraction silently protects nothing. A typo here is invisible:
    /// the filter would just never match.
    #[test]
    fn every_guest_denied_group_exists_in_the_catalog() {
        for g in crate::mcp::domain::tool_group::groups_denied_to_guests() {
            assert!(
                crate::mcp::domain::tool_group::is_catalog_extension(g),
                "{g} is on the guest denylist but is not a catalog extension -- \
                 the denial would match nothing"
            );
        }
    }
}
