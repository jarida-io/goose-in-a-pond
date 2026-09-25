//! Deterministic in-turn history trimmer, the hard-real-time half of hybrid compaction:
//! never calls a model or blocks, and tool results always travel with their turn.
//!
//! The real-prompt overshoot correction is load-bearing: no counter here is exact, and an
//! image counts only its surrounding text (~250 tokens short; `MAX_HISTORY_REPLAY_IMAGES`
//! bounds that). Images are capped by the adapter after [`trim_history`], via
//! `super::image_history`; do not add a second image rule here.

use std::borrow::Cow;
use std::time::Duration;

use super::context_budget::{truncate_head_tail, CompactionProfile, TOOL_RESULT_MAX_BYTES};
use super::prefix_cache::CachePosture;
use super::token_counting::PER_MESSAGE_TOKEN_OVERHEAD;
use crate::models::ports::token_counter::TokenCounter;

/// Floor for the history budget after the overshoot correction, so the turn's own message
/// survives. With a subagent reservation live, declared budgets can exceed the window by this.
pub const MIN_HISTORY_TOKENS: usize = 64;

/// Default for `Settings::compaction_verbatim_days`: days kept verbatim before age weighting.
/// Generous on purpose: a short horizon truncates tool results the model is still using.
pub const DEFAULT_VERBATIM_DAYS: u32 = 3;

/// Tool-result cap for material older than the verbatim horizon.
pub const AGED_TOOL_RESULT_MAX_BYTES: usize = TOOL_RESULT_MAX_BYTES / 4;

/// Results at or below this count as already aged: `truncate_head_tail` appends an elision
/// marker (the 64), and cold turns re-run the rung, so without it results erode every turn.
const AGED_FIXED_POINT_BYTES: usize = AGED_TOOL_RESULT_MAX_BYTES + 64;

/// Horizon for [`trim_history`]; zero days is the only way to disable age weighting.
pub fn verbatim_horizon_from_days(days: u32) -> Option<Duration> {
    if days == 0 {
        return None;
    }
    Some(Duration::from_secs(u64::from(days) * 24 * 60 * 60))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimRole {
    User,
    Assistant,
    ToolResult,
}

/// Engine-neutral conversation message; `index` keys back into the source conversation.
#[derive(Debug, Clone)]
pub struct TrimMessage {
    pub index: usize,
    pub role: TrimRole,
    pub text: String,
    /// True for the spliced `<conversation-summary>` message.
    pub is_summary: bool,
    /// Caller-measured age at trim time; `None` is treated as recent everywhere.
    pub age_secs: Option<u64>,
}

#[derive(Debug)]
pub struct TrimOutcome {
    pub messages: Vec<TrimMessage>,
    pub dropped_turns: usize,
    pub estimated_tokens: usize,
    /// Tool results re-cut to [`AGED_TOOL_RESULT_MAX_BYTES`] for being past the verbatim horizon.
    pub aged_truncations: usize,
    /// False when nothing was modified, so the adapter can skip rewriting the conversation.
    pub changed: bool,
    /// Age weighting degraded an in-budget conversation only because the prefix cache was cold.
    pub cold_recompaction: bool,
}

/// Whether this turn's user message is already in the slice, which decides whose
/// `<system-context>` is stale. The Goose adapter trims before `Agent::reply` appends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrentTurn {
    /// Trimmed before appending, so every user message present is stale. The production shape.
    NotYetAppended,
    /// Trimmed after appending: the last user message is this turn's and must survive.
    AlreadyAppended,
}

/// Remove the `<system-context>` block (per-turn, stale in history) from a prior user message.
pub fn strip_system_context(text: &str) -> Cow<'_, str> {
    const OPEN: &str = "<system-context>";
    const CLOSE: &str = "</system-context>";
    let Some(start) = text.find(OPEN) else {
        return Cow::Borrowed(text);
    };
    let Some(close) = text[start..].find(CLOSE) else {
        return Cow::Borrowed(text);
    };
    let end = start + close + CLOSE.len();
    let mut out = String::with_capacity(text.len() - (end - start));
    out.push_str(&text[..start]);
    out.push_str(text[end..].trim_start_matches('\n'));
    Cow::Owned(out)
}

fn estimate_tokens(messages: &[TrimMessage], counter: &dyn TokenCounter) -> usize {
    messages
        .iter()
        .map(|m| counter.count(&m.text) + PER_MESSAGE_TOKEN_OVERHEAD)
        .sum()
}

/// Start of the last turn (a user message and everything after it).
fn last_turn_start(messages: &[TrimMessage]) -> usize {
    messages
        .iter()
        .rposition(|m| m.role == TrimRole::User && !m.is_summary)
        .unwrap_or(0)
}

/// The history budget [`trim_history`] actually trims to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryBudget {
    /// Tokens history may occupy this turn, floored at [`MIN_HISTORY_TOKENS`].
    pub tokens: usize,
    /// The previous turn's real prompt overshot the usable ceiling and shrank this budget.
    pub overshoot_corrected: bool,
}

/// History's budget after the preamble clamp and overshoot correction; computed ONLY here.
pub fn effective_history_budget(
    profile: &CompactionProfile,
    last_real_prompt_tokens: Option<u32>,
) -> HistoryBudget {
    let usable = profile.usable_prompt_tokens();
    let mut tokens = if usable > 0 {
        profile
            .history_token_budget
            .min(profile.usable_history_tokens())
            .max(MIN_HISTORY_TOKENS)
    } else {
        profile.history_token_budget
    };
    let mut overshoot_corrected = false;
    if let Some(real) = last_real_prompt_tokens {
        let real = real as usize;
        if usable > 0 && real > usable {
            tokens = tokens.saturating_sub(real - usable).max(MIN_HISTORY_TOKENS);
            overshoot_corrected = true;
        }
    }
    HistoryBudget {
        tokens,
        overshoot_corrected,
    }
}

/// Trim `messages` to the history budget, splicing `rolling_summary` at the front.
/// `last_real_prompt_tokens` is last turn's real prompt size; `None`/`Warm` degrade least.
#[allow(clippy::too_many_arguments)]
pub fn trim_history(
    messages: Vec<TrimMessage>,
    profile: &CompactionProfile,
    rolling_summary: Option<&str>,
    last_real_prompt_tokens: Option<u32>,
    counter: &dyn TokenCounter,
    current_turn: CurrentTurn,
    verbatim_horizon: Option<Duration>,
    cache: CachePosture,
) -> TrimOutcome {
    let mut changed = false;

    // A prompt that fills the window makes goose compact mid-generation by a path that ignores
    // GOOSE_AUTO_COMPACT_THRESHOLD; the clamp and overshoot correction keep it below that.
    let HistoryBudget {
        tokens: budget,
        overshoot_corrected,
    } = effective_history_budget(profile, last_real_prompt_tokens);
    if overshoot_corrected {
        changed = true;
    }

    // 1. Strip stale <system-context> from prior user messages (see `CurrentTurn`).
    let mut msgs: Vec<TrimMessage> = messages;
    let spare_last_user = match current_turn {
        CurrentTurn::AlreadyAppended => msgs
            .iter()
            .rposition(|m| m.role == TrimRole::User && !m.is_summary),
        CurrentTurn::NotYetAppended => None,
    };
    for (i, m) in msgs.iter_mut().enumerate() {
        if m.role == TrimRole::User && !m.is_summary && Some(i) != spare_last_user {
            if let Cow::Owned(stripped) = strip_system_context(&m.text) {
                m.text = stripped;
                changed = true;
            }
        }
    }

    // 2. Head+tail truncate tool results; the adapter uses the same helper, so estimates match.
    for m in msgs.iter_mut() {
        if m.role == TrimRole::ToolResult {
            if let Some(truncated) = truncate_head_tail(&m.text, TOOL_RESULT_MAX_BYTES) {
                m.text = truncated;
                changed = true;
            }
        }
    }

    // 3. Splice or refresh the rolling summary at the front.
    if let Some(summary) = rolling_summary {
        let body = format!("<conversation-summary>\n{summary}\n</conversation-summary>");
        match msgs.iter_mut().find(|m| m.is_summary) {
            Some(existing) => {
                if existing.text != body {
                    existing.text = body;
                    changed = true;
                }
            }
            None => {
                msgs.insert(
                    0,
                    TrimMessage {
                        index: usize::MAX,
                        role: TrimRole::User,
                        text: body,
                        is_summary: true,
                        // The summary is generated now, whatever it summarises.
                        age_secs: Some(0),
                    },
                );
                changed = true;
            }
        }
    }

    // 4. Re-cut tool results past the horizon, only if over budget or cold (a cold prefix is
    //    re-prefilled anyway), never in the last turn, never with an unknown age.
    let mut aged_truncations = 0usize;
    let mut cold_recompaction = false;
    if let Some(horizon) = verbatim_horizon {
        let over_budget = estimate_tokens(&msgs, counter) > budget;
        if over_budget || cache == CachePosture::Cold {
            cold_recompaction = !over_budget;
            let horizon_secs = horizon.as_secs();
            let keep_verbatim_from = last_turn_start(&msgs);
            for (i, m) in msgs.iter_mut().enumerate() {
                if i >= keep_verbatim_from
                    || m.role != TrimRole::ToolResult
                    || m.text.len() <= AGED_FIXED_POINT_BYTES
                    || !m.age_secs.is_some_and(|age| age > horizon_secs)
                {
                    continue;
                }
                if let Some(truncated) = truncate_head_tail(&m.text, AGED_TOOL_RESULT_MAX_BYTES) {
                    m.text = truncated;
                    aged_truncations += 1;
                    changed = true;
                }
            }
        }
    }

    // 5. Drop oldest complete turns (never the summary or last turn) until within budget.
    //    By position, not timestamps: age is monotonic with it and cannot clock-skew.
    let mut dropped_turns = 0usize;
    loop {
        let estimated = estimate_tokens(&msgs, counter);
        if estimated <= budget {
            break;
        }
        let keep_from = last_turn_start(&msgs);
        let first_real = msgs.iter().position(|m| !m.is_summary).unwrap_or(0);
        if first_real >= keep_from {
            break; // only the last turn (+ summary) remains — nothing left to drop
        }
        let turn_end = msgs
            .iter()
            .enumerate()
            .skip(first_real + 1)
            .find(|(_, m)| m.role == TrimRole::User && !m.is_summary)
            .map(|(i, _)| i)
            .unwrap_or(keep_from);
        msgs.drain(first_real..turn_end);
        dropped_turns += 1;
        changed = true;
    }

    let estimated_tokens = estimate_tokens(&msgs, counter);
    TrimOutcome {
        messages: msgs,
        dropped_turns,
        estimated_tokens,
        aged_truncations,
        changed,
        // Only if something was degraded: turn one is always cold, so the posture alone would
        // put a false recompaction in every fresh session's trace.
        cold_recompaction: cold_recompaction && aged_truncations > 0,
    }
}

/// Shape stored history for a fresh engine session; callers drop tool results first. Drops
/// empty messages (providers reject them) and the trailing, already-persisted user turn.
pub fn plan_replay(
    messages: Vec<(TrimRole, String)>,
    profile: &CompactionProfile,
    rolling_summary: Option<&str>,
    counter: &dyn TokenCounter,
) -> Vec<TrimMessage> {
    let mut rows: Vec<(TrimRole, String)> = messages
        .into_iter()
        .filter(|(_, text)| !text.trim().is_empty())
        .collect();
    while matches!(rows.last(), Some((TrimRole::User, _))) {
        rows.pop();
    }
    if rows.is_empty() {
        return Vec::new();
    }

    let trim_input: Vec<TrimMessage> = rows
        .into_iter()
        .enumerate()
        .map(|(index, (role, text))| TrimMessage {
            index,
            role,
            text,
            is_summary: false,
            // Unaged: only an over-budget replay would use it, and the drop loop covers that.
            age_secs: None,
        })
        .collect();
    // Trailing user messages were popped, so every `<system-context>` here is stale.
    trim_history(
        trim_input,
        profile,
        rolling_summary,
        None,
        counter,
        CurrentTurn::NotYetAppended,
        None,
        // A replay is a brand-new engine session: no prefix to protect.
        CachePosture::Cold,
    )
    .messages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::services::context::token_counting::HeuristicTokenCounter;

    /// Warm-cache shim of [`super::trim_history`]; only the cold-rule tests pass a posture.
    #[allow(clippy::too_many_arguments)]
    fn trim_history(
        messages: Vec<TrimMessage>,
        profile: &CompactionProfile,
        rolling_summary: Option<&str>,
        last_real_prompt_tokens: Option<u32>,
        counter: &dyn TokenCounter,
        current_turn: CurrentTurn,
        verbatim_horizon: Option<Duration>,
    ) -> TrimOutcome {
        super::trim_history(
            messages,
            profile,
            rolling_summary,
            last_real_prompt_tokens,
            counter,
            current_turn,
            verbatim_horizon,
            CachePosture::Warm,
        )
    }

    fn profile(history_budget: usize) -> CompactionProfile {
        CompactionProfile {
            compaction_threshold: 0.6,
            memory_token_budget: 200,
            max_memory_fragments: 3,
            system_prompt_budget: 1500,
            history_token_budget: history_budget,
            // Zero so these cases exercise exactly the budget they pass in.
            output_reserve_tokens: 0,
            context_window_tokens: 3072,
            prompt_window_tokens: 3072,
        }
    }

    /// Same shape, but with a reserve — for the ceiling/overshoot cases.
    fn profile_reserved(history_budget: usize, ctx: usize, reserve: usize) -> CompactionProfile {
        CompactionProfile {
            compaction_threshold: 0.6,
            memory_token_budget: 200,
            max_memory_fragments: 3,
            system_prompt_budget: 1500,
            history_token_budget: history_budget,
            output_reserve_tokens: reserve,
            context_window_tokens: ctx,
            prompt_window_tokens: ctx,
        }
    }

    #[test]
    fn history_budget_never_exceeds_the_window_minus_the_output_reserve() {
        let p = profile_reserved(1200, 4096, 768);
        assert_eq!(p.usable_prompt_tokens(), 3328);

        let msgs: Vec<TrimMessage> = (0..40).map(|i| user(i, &"x".repeat(400))).collect();
        let out = trim_history(
            msgs,
            &p,
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        // 1200 <= 3328, so the declared budget still applies here.
        assert!(out.estimated_tokens <= 1200, "got {}", out.estimated_tokens);
    }

    #[test]
    fn the_reserve_wins_when_it_is_tighter_than_the_declared_budget() {
        let p = profile_reserved(4000, 2048, 768); // usable = 1280
        let msgs: Vec<TrimMessage> = (0..40).map(|i| user(i, &"x".repeat(400))).collect();
        let out = trim_history(
            msgs,
            &p,
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(
            out.estimated_tokens <= 1280,
            "history must fit the usable window, got {}",
            out.estimated_tokens
        );
    }

    #[test]
    fn the_history_clamp_subtracts_the_preamble_not_just_the_output_reserve() {
        // The shipped profile, not a fixture: its real numbers are the over-committed ones.
        let p = CompactionProfile::from_context_window(8_192);
        assert_eq!(p.history_token_budget, 4_000);
        assert_eq!(p.usable_prompt_tokens(), 7_168);
        assert_eq!(p.usable_history_tokens(), 3_668);

        // ~10,000 tokens offered: the clamp must cut them to 3,668, never let them reach 7,168.
        let msgs: Vec<TrimMessage> = (0..40).map(|i| user(i, &"x".repeat(1_000))).collect();
        let out = trim_history(
            msgs,
            &p,
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(
            out.estimated_tokens <= 3_668,
            "history claimed {} tokens, past the {} the preamble leaves it",
            out.estimated_tokens,
            p.usable_history_tokens()
        );
        assert!(
            out.dropped_turns > 0,
            "nothing was dropped, so the clamp never bound"
        );
        assert!(
            out.estimated_tokens
                + p.system_prompt_budget
                + p.memory_token_budget
                + p.output_reserve_tokens
                <= p.context_window_tokens,
            "budgeted prompt still overruns the window"
        );
    }

    #[test]
    fn a_preamble_wider_than_the_window_squeezes_history_to_the_floor() {
        // usable 1,280 minus the 1,700 preamble saturates to 0, so the floor takes over.
        let p = profile_reserved(4_000, 2_048, 768);
        assert_eq!(p.usable_history_tokens(), 0);
        let msgs: Vec<TrimMessage> = (0..40).map(|i| user(i, &"x".repeat(400))).collect();
        let out = trim_history(
            msgs,
            &p,
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(out.estimated_tokens > 0, "must keep the current turn");
        assert_eq!(out.messages.len(), 1, "only the current turn survives");
    }

    #[test]
    fn a_real_prompt_over_the_ceiling_shrinks_the_next_budget_by_the_overshoot() {
        let p = profile_reserved(1200, 4096, 768); // usable = 3328
        let msgs: Vec<TrimMessage> = (0..40).map(|i| user(i, &"x".repeat(400))).collect();

        let baseline = trim_history(
            msgs.clone(),
            &p,
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        )
        .estimated_tokens;
        // Engine said the last prompt was 3,786 tokens — 458 over the ceiling.
        let corrected = trim_history(
            msgs,
            &p,
            None,
            Some(3786),
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        )
        .estimated_tokens;
        assert!(
            corrected < baseline,
            "overshoot must tighten the budget: {corrected} vs {baseline}"
        );
        assert!(corrected <= 1200 - 458 + 40, "got {corrected}");
    }

    #[test]
    fn the_budget_never_collapses_below_the_floor() {
        let p = profile_reserved(1200, 4096, 768);
        let msgs: Vec<TrimMessage> = (0..40).map(|i| user(i, &"x".repeat(400))).collect();
        let out = trim_history(
            msgs,
            &p,
            None,
            Some(100_000),
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(out.estimated_tokens > 0, "must keep the current turn");
    }

    // ── A subagent is a second claim on one window ──────────────────────────

    /// Swept: reserving from the DECLARED budget passes at small fractions, then breaks.
    #[test]
    fn a_parents_budget_and_its_childs_reservation_fit_the_window_or_hit_the_floor() {
        let mut floored = 0usize;
        let mut checked = 0usize;
        for window in [2_048, 4_096, 8_192, 12_288, 16_384, 32_768, 65_536, 128_000] {
            for prompt_window in [window.min(8_192), window] {
                let parent = CompactionProfile::for_windows(window, prompt_window);
                for fraction in [0.1_f32, 0.25, 0.3, 0.5, 0.75, 0.9, 1.0] {
                    checked += 1;
                    let claimable = parent
                        .history_token_budget
                        .min(parent.usable_history_tokens());
                    let reserved = (claimable as f64 * fraction as f64).ceil() as usize;
                    let child_live = parent.with_history_reserved(fraction);
                    let effective = effective_history_budget(&child_live, None).tokens;

                    let sum = effective
                        + reserved
                        + child_live.system_prompt_budget
                        + child_live.memory_token_budget;
                    if effective == MIN_HISTORY_TOKENS {
                        floored += 1;
                        continue;
                    }
                    assert!(
                        sum <= child_live.usable_prompt_tokens(),
                        "window {window}/{prompt_window} at fraction {fraction}: the parent \
                         ({effective}) and its child ({reserved}) together with the preamble \
                         claim {sum} tokens of a {} usable prompt - two agents each believing \
                         they own the window is the overrun this reservation exists to prevent",
                        child_live.usable_prompt_tokens()
                    );
                }
            }
        }
        assert!(checked > 50, "the sweep degenerated to {checked} cases");
        // Vacuity control: the sweep must reach the floor branch of the disjunction.
        assert!(
            floored > 0,
            "no case in the sweep reached MIN_HISTORY_TOKENS, so the conditional form of this \
             assertion was never exercised and an unconditional one would have looked correct"
        );
    }

    #[test]
    fn a_child_that_takes_the_whole_budget_leaves_the_parent_exactly_on_the_floor() {
        let parent = CompactionProfile::from_context_window(8_192);
        let claimable = parent
            .history_token_budget
            .min(parent.usable_history_tokens());
        assert_eq!(claimable, 3_668, "the clamp, not the declared 4,000");

        let child_live = parent.with_history_reserved(1.0);
        assert_eq!(
            child_live.history_token_budget, 0,
            "a fraction of 1.0 must leave the parent nothing to declare"
        );
        let effective = effective_history_budget(&child_live, None).tokens;
        assert_eq!(
            effective, MIN_HISTORY_TOKENS,
            "the trimmer's floor is what keeps the turn's own message alive; the parent must \
             land on it rather than at zero"
        );

        let sum = effective + claimable + parent.system_prompt_budget + parent.memory_token_budget;
        assert!(
            sum > parent.usable_prompt_tokens(),
            "this case no longer overshoots, so the unconditional form of the budget assertion \
             would pass here and the conditional form is untested"
        );
        assert_eq!(
            sum - parent.usable_prompt_tokens(),
            MIN_HISTORY_TOKENS,
            "the overshoot must be exactly the floor - anything else means the reservation is \
             being taken from the wrong number"
        );
    }

    #[test]
    fn a_live_child_makes_the_trimmer_keep_less_of_the_parents_history() {
        let parent = CompactionProfile::from_context_window(8_192);
        let msgs = || -> Vec<TrimMessage> {
            (0..40)
                .map(|i| {
                    if i % 2 == 0 {
                        user(i, &"x".repeat(1_000))
                    } else {
                        assistant(i, &"y".repeat(1_000))
                    }
                })
                .collect()
        };

        let unreserved = trim_history(
            msgs(),
            &parent,
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        let reserved = trim_history(
            msgs(),
            &parent.with_history_reserved(0.5),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );

        assert!(
            reserved.estimated_tokens < unreserved.estimated_tokens,
            "the parent kept {} tokens with a child live and {} without, so the reservation \
             never reached the trimmer",
            reserved.estimated_tokens,
            unreserved.estimated_tokens
        );
        assert!(
            reserved.dropped_turns > unreserved.dropped_turns,
            "the same conversation dropped {} turns with a child live and {} without",
            reserved.dropped_turns,
            unreserved.dropped_turns
        );
        assert!(
            unreserved.estimated_tokens > 0 && unreserved.dropped_turns < 20,
            "the unreserved run kept {} tokens over {} dropped turns; if it keeps nothing then \
             'reserved keeps less' is satisfied by a trimmer that ignores the budget",
            unreserved.estimated_tokens,
            unreserved.dropped_turns
        );
    }

    fn user(index: usize, text: &str) -> TrimMessage {
        TrimMessage {
            index,
            role: TrimRole::User,
            text: text.to_string(),
            is_summary: false,
            age_secs: None,
        }
    }
    fn assistant(index: usize, text: &str) -> TrimMessage {
        TrimMessage {
            index,
            role: TrimRole::Assistant,
            text: text.to_string(),
            is_summary: false,
            age_secs: None,
        }
    }
    fn tool(index: usize, text: &str) -> TrimMessage {
        TrimMessage {
            index,
            role: TrimRole::ToolResult,
            text: text.to_string(),
            is_summary: false,
            age_secs: None,
        }
    }

    fn aged_tool(index: usize, text: &str, age_secs: u64) -> TrimMessage {
        TrimMessage {
            age_secs: Some(age_secs),
            ..tool(index, text)
        }
    }

    fn horizon() -> Option<Duration> {
        verbatim_horizon_from_days(DEFAULT_VERBATIM_DAYS)
    }

    const DAY: u64 = 24 * 60 * 60;

    /// Mirrors the adapter's envelope: the user's own words FIRST, then `<system-context>`.
    #[test]
    fn stripping_preserves_everything_before_the_system_context_block() {
        let turn = "<user-message>\nwhat did I ask you yesterday?\n</user-message>\n\
                    <system-context>\nToday is X\n<memories>a memory</memories>\n\
                    </system-context>\n";

        let stripped = strip_system_context(turn);

        assert_eq!(
            stripped, "<user-message>\nwhat did I ask you yesterday?\n</user-message>\n",
            "the user's words must survive the strip byte for byte"
        );
        assert!(!stripped.contains("<system-context>"));
        assert!(!stripped.contains("a memory"));
    }

    #[test]
    fn stripping_a_leading_system_context_leaves_the_rest() {
        let turn =
            "<system-context>\nToday is X\n</system-context>\n<user-message>hi</user-message>";
        assert_eq!(
            strip_system_context(turn),
            "<user-message>hi</user-message>"
        );
    }

    #[test]
    fn every_user_message_is_stale_when_the_turn_has_not_been_appended_yet() {
        let wrapped =
            "<system-context>\nToday is X\n</system-context>\n<user-message>hi</user-message>";
        let msgs = vec![user(0, wrapped), assistant(1, "hello"), user(2, wrapped)];
        let out = trim_history(
            msgs,
            &profile(10_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(out.changed);
        for i in [0, 2] {
            assert!(
                !out.messages[i].text.contains("<system-context>"),
                "message {i} kept a stale system-context block"
            );
            assert!(out.messages[i]
                .text
                .contains("<user-message>hi</user-message>"));
        }
    }

    #[test]
    fn the_appended_current_turn_keeps_its_fresh_injection() {
        let wrapped =
            "<system-context>\nToday is X\n</system-context>\n<user-message>hi</user-message>";
        let msgs = vec![user(0, wrapped), assistant(1, "hello"), user(2, wrapped)];
        let out = trim_history(
            msgs,
            &profile(10_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::AlreadyAppended,
            None,
        );
        assert!(out.changed);
        assert!(!out.messages[0].text.contains("<system-context>"));
        assert!(out.messages[2].text.contains("<system-context>"));
    }

    /// Else the adapter's early return never fires and it rewrites goose's message table per turn.
    #[test]
    fn a_steady_state_conversation_reports_no_change() {
        let msgs = vec![
            user(0, "<user-message>hi</user-message>"),
            assistant(1, "hello"),
            user(2, "<user-message>again</user-message>"),
            assistant(3, "sure"),
        ];
        let out = trim_history(
            msgs,
            &profile(10_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(
            !out.changed,
            "nothing was stale or oversized, yet the adapter was told to rewrite"
        );
    }

    #[test]
    fn truncates_oversized_tool_results() {
        let big = format!("START{}END", "x".repeat(TOOL_RESULT_MAX_BYTES + 500));
        let msgs = vec![user(0, "check"), tool(1, &big), assistant(2, "done")];
        let out = trim_history(
            msgs,
            &profile(10_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(out.changed);
        assert!(out.messages[1].text.len() < TOOL_RESULT_MAX_BYTES + 64);
        // Head+tail: the conclusion at the end of a tool result survives.
        assert!(out.messages[1].text.starts_with("START"));
        assert!(out.messages[1].text.ends_with("END"));
        assert!(out.messages[1].text.contains("[... truncated "));
    }

    #[test]
    fn splices_summary_at_front_and_refreshes_it() {
        let msgs = vec![user(0, "a"), assistant(1, "b")];
        let out = trim_history(
            msgs,
            &profile(10_000),
            Some("we discussed ducks"),
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(out.messages[0].is_summary);
        assert!(out.messages[0].text.contains("<conversation-summary>"));
        assert!(out.messages[0].text.contains("we discussed ducks"));

        // Refresh replaces the body, does not duplicate.
        let out2 = trim_history(
            out.messages,
            &profile(10_000),
            Some("now geese"),
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        let summaries: Vec<_> = out2.messages.iter().filter(|m| m.is_summary).collect();
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].text.contains("now geese"));
    }

    #[test]
    fn drops_oldest_complete_turns_never_orphaning_tool_results() {
        // Three turns; tiny budget forces dropping the oldest two.
        let filler = "w".repeat(400);
        let msgs = vec![
            user(0, &filler),
            tool(1, &filler),
            assistant(2, &filler),
            user(3, &filler),
            assistant(4, &filler),
            user(5, "latest question"),
            assistant(6, "latest answer"),
        ];
        let out = trim_history(
            msgs,
            &profile(100),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert_eq!(out.dropped_turns, 2);
        // Whole turns went together: no leading tool/assistant orphans.
        assert_eq!(out.messages.first().unwrap().role, TrimRole::User);
        assert!(out
            .messages
            .first()
            .unwrap()
            .text
            .contains("latest question"));
    }

    #[test]
    fn last_turn_survives_even_over_budget() {
        let huge = "y".repeat(4_000);
        let msgs = vec![user(0, &huge), assistant(1, &huge)];
        let out = trim_history(
            msgs,
            &profile(50),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert_eq!(out.messages.len(), 2, "the current turn is never dropped");
    }

    #[test]
    fn trimming_is_idempotent() {
        let filler = "z".repeat(400);
        let msgs = vec![
            user(0, &filler),
            assistant(1, &filler),
            user(2, "q"),
            assistant(3, "a"),
        ];
        let once = trim_history(
            msgs,
            &profile(100),
            Some("sum"),
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        let twice = trim_history(
            once.messages.clone(),
            &profile(100),
            Some("sum"),
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(!twice.changed, "second pass must be a no-op");
        assert_eq!(once.messages.len(), twice.messages.len());
    }

    #[test]
    fn unchanged_input_reports_changed_false() {
        let msgs = vec![user(0, "hi"), assistant(1, "hello")];
        let out = trim_history(
            msgs,
            &profile(10_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(!out.changed);
        assert_eq!(out.dropped_turns, 0);
    }

    #[test]
    fn real_token_feedback_tightens_budget() {
        // 6144 real vs a 3072 window: the 3072 overshoot drops the 300 budget to the floor.
        let filler = "v".repeat(400);
        let msgs = vec![
            user(0, &filler),
            assistant(1, &filler),
            user(2, "q"),
            assistant(3, "a"),
        ];
        let relaxed = trim_history(
            msgs.clone(),
            &profile(300),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert_eq!(relaxed.dropped_turns, 0);
        let tightened = trim_history(
            msgs,
            &profile(300),
            None,
            Some(6144),
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        assert!(tightened.dropped_turns > 0);
    }

    // ── plan_replay (hydration) ──────────────────────────────────────────

    fn rows(pairs: &[(TrimRole, &str)]) -> Vec<(TrimRole, String)> {
        pairs.iter().map(|(r, t)| (*r, (*t).to_string())).collect()
    }

    /// The caller persists the incoming message first, so durable history ends with this turn's.
    #[test]
    fn replay_drops_the_trailing_user_message() {
        let out = plan_replay(
            rows(&[
                (TrimRole::User, "first"),
                (TrimRole::Assistant, "answer"),
                (TrimRole::User, "the message about to be sent"),
            ]),
            &profile(10_000),
            None,
            &HeuristicTokenCounter,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].role, TrimRole::Assistant);
        assert!(!out.iter().any(|m| m.text.contains("about to be sent")));
    }

    #[test]
    fn replay_of_only_user_messages_is_empty() {
        assert!(plan_replay(
            rows(&[(TrimRole::User, "hello")]),
            &profile(10_000),
            None,
            &HeuristicTokenCounter
        )
        .is_empty());
        assert!(plan_replay(vec![], &profile(10_000), None, &HeuristicTokenCounter).is_empty());
    }

    #[test]
    fn replay_skips_blank_messages() {
        let out = plan_replay(
            rows(&[
                (TrimRole::User, "q"),
                (TrimRole::Assistant, "   "),
                (TrimRole::Assistant, "real"),
            ]),
            &profile(10_000),
            None,
            &HeuristicTokenCounter,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].text, "real");
    }

    #[test]
    fn replay_splices_the_rolling_summary_and_respects_the_budget() {
        let filler = "z".repeat(2_000);
        let out = plan_replay(
            rows(&[
                (TrimRole::User, "oldest"),
                (TrimRole::Assistant, &filler),
                (TrimRole::User, "newer"),
                (TrimRole::Assistant, "kept"),
            ]),
            &profile(200),
            Some("earlier: the user set up two lamps"),
            &HeuristicTokenCounter,
        );
        assert!(out[0].is_summary);
        assert!(out[0].text.contains("<conversation-summary>"));
        // The oversized oldest turn was dropped to fit the budget.
        assert!(!out.iter().any(|m| m.text == filler));
        assert!(out.iter().any(|m| m.text == "kept"));
    }

    #[test]
    fn replay_is_idempotent() {
        let input = rows(&[
            (TrimRole::User, "q1"),
            (TrimRole::Assistant, "a1"),
            (TrimRole::User, "q2"),
            (TrimRole::Assistant, "a2"),
        ]);
        let first = plan_replay(
            input.clone(),
            &profile(10_000),
            Some("s"),
            &HeuristicTokenCounter,
        );
        let again: Vec<(TrimRole, String)> = first
            .iter()
            .filter(|m| !m.is_summary)
            .map(|m| (m.role, m.text.clone()))
            .collect();
        let second = plan_replay(again, &profile(10_000), Some("s"), &HeuristicTokenCounter);
        assert_eq!(
            first.iter().map(|m| m.text.clone()).collect::<Vec<_>>(),
            second.iter().map(|m| m.text.clone()).collect::<Vec<_>>()
        );
    }

    // ── Age-weighted retention ──────────────────────────────────────────────
    // The rung fires only over budget: each test forces an overflow or proves the rung fires.

    #[test]
    fn an_aged_tool_result_is_degraded_before_its_turn_is_dropped() {
        let big = "y".repeat(4_000);
        let msgs = vec![
            user(0, "first question"),
            aged_tool(1, &big, 9 * DAY),
            assistant(2, "answer one"),
            user(3, "second question"),
            aged_tool(4, &big, 8 * DAY),
            assistant(5, "answer two"),
            user(6, "current question"),
        ];

        let without = trim_history(
            msgs.clone(),
            &profile(600),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            None,
        );
        let with = trim_history(
            msgs,
            &profile(600),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
        );

        assert!(
            without.dropped_turns > 0,
            "fixture is not over budget -- the rung would never be invited to run"
        );
        assert_eq!(
            with.aged_truncations, 2,
            "both aged tool results should have been re-truncated"
        );
        assert!(
            with.dropped_turns < without.dropped_turns,
            "age weighting must save turns the flat trimmer dropped: with={} without={}",
            with.dropped_turns,
            without.dropped_turns
        );
        for m in with
            .messages
            .iter()
            .filter(|m| m.role == TrimRole::ToolResult)
        {
            assert!(
                m.text.len() <= AGED_TOOL_RESULT_MAX_BYTES + 64,
                "aged result was not re-truncated: {} chars",
                m.text.len()
            );
        }
    }

    /// Degrading a fitting conversation would pay a full re-prefill for tokens nobody needed.
    #[test]
    fn age_weighting_never_touches_a_conversation_that_already_fits() {
        let msgs = vec![
            user(0, "q"),
            aged_tool(1, &"y".repeat(4_000), 30 * DAY),
            assistant(2, "a"),
            user(3, "current"),
        ];
        let tight = trim_history(
            msgs.clone(),
            &profile(300),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
        );
        assert_eq!(
            tight.aged_truncations, 1,
            "control: this conversation IS degradable when over budget"
        );

        let roomy = trim_history(
            msgs,
            &profile(100_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
        );
        assert_eq!(roomy.aged_truncations, 0);
        assert_eq!(roomy.dropped_turns, 0);
        // Step 2's flat cap still applies; the tighter aged cap must not have.
        let tool_text = &roomy
            .messages
            .iter()
            .find(|m| m.role == TrimRole::ToolResult)
            .expect("tool result kept")
            .text;
        assert!(
            tool_text.len() > AGED_TOOL_RESULT_MAX_BYTES,
            "a fitting conversation was degraded anyway: {} chars",
            tool_text.len()
        );
    }

    #[test]
    fn the_last_turn_is_verbatim_even_when_the_whole_session_is_aged() {
        let big = "y".repeat(4_000);
        let out = trim_history(
            vec![
                user(0, "old question"),
                aged_tool(1, &big, 9 * DAY),
                assistant(2, "old answer"),
                user(3, "the question being answered"),
                aged_tool(4, &big, 9 * DAY),
            ],
            &profile(600),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
        );
        assert_eq!(
            out.aged_truncations, 1,
            "exactly the tool result OUTSIDE the last turn may be degraded"
        );
        let last = out.messages.last().expect("last turn survives");
        assert_eq!(last.role, TrimRole::ToolResult);
        assert!(
            last.text.len() > AGED_TOOL_RESULT_MAX_BYTES,
            "the last turn's tool result was degraded: {} chars",
            last.text.len()
        );
    }

    /// At a budget where an aged message provably IS degraded.
    #[test]
    fn an_unknown_or_fresh_age_is_never_degraded() {
        let big = "y".repeat(4_000);
        let run = |age: Option<u64>| {
            trim_history(
                vec![
                    user(0, "q"),
                    TrimMessage {
                        age_secs: age,
                        ..tool(1, &big)
                    },
                    assistant(2, "a"),
                    user(3, "current"),
                ],
                &profile(300),
                None,
                None,
                &HeuristicTokenCounter,
                CurrentTurn::NotYetAppended,
                horizon(),
            )
        };
        assert_eq!(
            run(Some(9 * DAY)).aged_truncations,
            1,
            "control: at this budget an AGED message is degraded"
        );
        for age in [None, Some(0), Some(DAY), Some(3 * DAY)] {
            assert_eq!(
                run(age).aged_truncations,
                0,
                "age {age:?} is inside the horizon and must not be degraded"
            );
        }
    }

    #[test]
    fn zero_verbatim_days_disables_age_weighting_entirely() {
        assert_eq!(verbatim_horizon_from_days(0), None);
        assert_eq!(
            verbatim_horizon_from_days(DEFAULT_VERBATIM_DAYS),
            Some(Duration::from_secs(3 * DAY))
        );

        let msgs = vec![
            user(0, "q"),
            aged_tool(1, &"y".repeat(4_000), 30 * DAY),
            assistant(2, "a"),
            user(3, "current"),
        ];
        let run = |h: Option<Duration>| {
            trim_history(
                msgs.clone(),
                &profile(300),
                None,
                None,
                &HeuristicTokenCounter,
                CurrentTurn::NotYetAppended,
                h,
            )
        };
        let on = run(horizon());
        let off = run(verbatim_horizon_from_days(0));
        let pre_p3 = run(None);

        assert_eq!(
            on.aged_truncations, 1,
            "control: the horizon is doing something at this budget"
        );
        assert_eq!(off.aged_truncations, 0);
        assert_eq!(off.dropped_turns, pre_p3.dropped_turns);
        assert_eq!(
            off.messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>(),
            pre_p3
                .messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>(),
            "zero days must reproduce pre-P3 output exactly"
        );
        assert_ne!(
            on.messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>(),
            off.messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>(),
            "on and off must differ, or the off switch proves nothing"
        );
    }

    #[test]
    fn age_weighting_is_idempotent_and_orphans_nothing() {
        let big = "y".repeat(4_000);
        let msgs = vec![
            user(0, "q1"),
            aged_tool(1, &big, 9 * DAY),
            assistant(2, "a1"),
            user(3, "current"),
        ];
        let first = trim_history(
            msgs,
            &profile(300),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
        );
        assert_eq!(first.aged_truncations, 1);
        for (i, m) in first.messages.iter().enumerate() {
            if m.role == TrimRole::ToolResult {
                assert!(
                    first.messages[..i]
                        .iter()
                        .any(|p| p.role == TrimRole::User && !p.is_summary),
                    "orphaned tool result at {i}"
                );
            }
        }

        let second = trim_history(
            first.messages.clone(),
            &profile(300),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
        );
        assert_eq!(second.aged_truncations, 0, "second pass degraded again");
        assert_eq!(
            first
                .messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>(),
            second
                .messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>()
        );
    }

    // ── The recompact-when-cold rule ──────────────────────────────────────

    /// Fits the budget but holds one aged tool result over the cap.
    fn roomy_with_one_aged_result() -> Vec<TrimMessage> {
        vec![
            user(0, "what did the sensor log say"),
            aged_tool(1, &"y".repeat(4_000), 9 * DAY),
            assistant(2, "here is the summary"),
            user(3, "and today?"),
        ]
    }

    #[test]
    fn a_cold_prefix_recompacts_a_conversation_that_still_fits_and_a_warm_one_does_not() {
        let warm = super::trim_history(
            roomy_with_one_aged_result(),
            &profile(5_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
            CachePosture::Warm,
        );
        assert_eq!(
            warm.aged_truncations, 0,
            "a warm prefix was perturbed for a token saving nothing had asked for"
        );
        assert!(!warm.cold_recompaction);

        let cold = super::trim_history(
            roomy_with_one_aged_result(),
            &profile(5_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
            CachePosture::Cold,
        );
        assert_eq!(
            cold.aged_truncations, 1,
            "a cold prefix declined free headroom"
        );
        assert!(cold.cold_recompaction);
        assert!(cold.changed);
        assert!(
            cold.estimated_tokens < warm.estimated_tokens,
            "the cold pass reclaimed nothing: {} vs {}",
            cold.estimated_tokens,
            warm.estimated_tokens
        );
        // Neither pass may drop a turn on a conversation that fits.
        assert_eq!(warm.dropped_turns, 0);
        assert_eq!(cold.dropped_turns, 0);
    }

    #[test]
    fn a_cold_prefix_still_respects_the_horizon_and_the_last_turn() {
        let big = "y".repeat(4_000);
        let msgs = vec![
            user(0, "old question"),
            // Inside the horizon: never aged, at any posture.
            aged_tool(1, &big, 60),
            assistant(2, "a1"),
            // Last turn: spared although nine days old.
            user(3, "current question"),
            aged_tool(4, &big, 9 * DAY),
        ];
        let cold = super::trim_history(
            msgs,
            &profile(5_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
            CachePosture::Cold,
        );
        assert_eq!(
            cold.aged_truncations, 0,
            "the cold rule degraded material the horizon or the last-turn guard protects"
        );
        assert!(!cold.cold_recompaction);
        for m in cold
            .messages
            .iter()
            .filter(|m| m.role == TrimRole::ToolResult)
        {
            // Only step 2's flat cap applied; both stay far above the aged cap.
            assert!(
                m.text.len() > AGED_FIXED_POINT_BYTES,
                "a spared tool result was cut to the aged cap: {} chars",
                m.text.len()
            );
        }
    }

    /// Guards [`AGED_FIXED_POINT_BYTES`]: fails if the elision marker outgrows its 64-byte slack.
    #[test]
    fn the_aged_cap_is_a_fixed_point_after_one_cut() {
        let huge = "y".repeat(100_000);
        let once = truncate_head_tail(&huge, AGED_TOOL_RESULT_MAX_BYTES)
            .expect("100k chars must exceed the aged cap");
        assert!(
            once.len() > AGED_TOOL_RESULT_MAX_BYTES,
            "truncate_head_tail became a fixed point of itself; the allowance \
             below can be removed, but do not assume it"
        );
        assert!(
            once.len() <= AGED_FIXED_POINT_BYTES,
            "the elision marker outgrew the {} char allowance: one cut yields {} \
             chars against a cap of {}",
            AGED_FIXED_POINT_BYTES - AGED_TOOL_RESULT_MAX_BYTES,
            once.len(),
            AGED_TOOL_RESULT_MAX_BYTES
        );
    }

    #[test]
    fn an_over_budget_cold_turn_is_not_reported_as_a_cold_recompaction() {
        let over_budget = super::trim_history(
            roomy_with_one_aged_result(),
            &profile(300),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
            CachePosture::Cold,
        );
        assert_eq!(over_budget.aged_truncations, 1);
        assert!(
            !over_budget.cold_recompaction,
            "P5 took credit for a degradation the budget already forced"
        );
    }

    #[test]
    fn a_cold_turn_with_nothing_to_degrade_reports_no_recompaction() {
        let out = super::trim_history(
            vec![user(0, "hello"), assistant(1, "hi"), user(2, "again")],
            &profile(5_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
            CachePosture::Cold,
        );
        assert_eq!(out.aged_truncations, 0);
        assert!(!out.cold_recompaction);
        assert!(!out.changed);
    }

    #[test]
    fn a_cold_recompaction_is_idempotent() {
        let first = super::trim_history(
            roomy_with_one_aged_result(),
            &profile(5_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
            CachePosture::Cold,
        );
        let second = super::trim_history(
            first.messages.clone(),
            &profile(5_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            horizon(),
            CachePosture::Cold,
        );
        assert_eq!(second.aged_truncations, 0);
        assert!(!second.cold_recompaction);
        assert!(!second.changed);
        assert_eq!(
            first
                .messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>(),
            second
                .messages
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn zero_verbatim_days_disables_the_cold_rule_as_well() {
        let out = super::trim_history(
            roomy_with_one_aged_result(),
            &profile(5_000),
            None,
            None,
            &HeuristicTokenCounter,
            CurrentTurn::NotYetAppended,
            verbatim_horizon_from_days(0),
            CachePosture::Cold,
        );
        assert_eq!(out.aged_truncations, 0);
        assert!(!out.cold_recompaction);
    }
}
