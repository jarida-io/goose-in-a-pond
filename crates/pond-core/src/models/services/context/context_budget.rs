const CHARS_PER_TOKEN: usize = 4;
const MIN_USABLE_HISTORY_CHARS: usize = 256;

// ── Compaction profile ──────────────────────────────────────────────────────

/// Per-model compaction budgets derived from the effective context window. Needed because
/// Goose auto-compacts at ~80% of `context_limit`, leaving a tiny KV cache no headroom.
#[derive(Debug, Clone)]
pub struct CompactionProfile {
    /// Fraction (0.0-1.0) of the window at which compaction should trigger.
    pub compaction_threshold: f32,
    /// Max tokens to allocate for memory injection into the system prompt.
    pub memory_token_budget: usize,
    /// Max number of memory fragments to inject per turn.
    pub max_memory_fragments: usize,
    /// Max tokens for the full system prompt (base + extras + memories).
    pub system_prompt_budget: usize,
    /// Max tokens to allocate for conversation history injection (per request).
    pub history_token_budget: usize,
    /// Tokens held back for the model's OWN output (reasoning plus answer); nothing else does.
    /// An overrun is ContextLengthExceeded, which Goose compacts past any threshold setting.
    pub output_reserve_tokens: usize,
    /// The effective context window this profile was derived from.
    pub context_window_tokens: usize,
    /// Window the PREAMBLE budgets came from; locally the clamped `ContextGovernor::prompt_window`.
    pub prompt_window_tokens: usize,
}

impl CompactionProfile {
    /// Ceiling for the engine's reported `prompt_tokens` (preamble plus history).
    pub fn usable_prompt_tokens(&self) -> usize {
        self.context_window_tokens
            .saturating_sub(self.output_reserve_tokens)
    }

    /// Prompt room minus the promised preamble; saturating, as `turn_trimmer` owns the floor.
    pub fn usable_history_tokens(&self) -> usize {
        self.usable_prompt_tokens()
            .saturating_sub(self.system_prompt_budget + self.memory_token_budget)
    }
}

/// One point on the budget curve: the exact profile for this window.
struct ProfileAnchor {
    window: usize,
    compaction_threshold: f32,
    memory_token_budget: usize,
    max_memory_fragments: usize,
    system_prompt_budget: usize,
    history_token_budget: usize,
    output_reserve_tokens: usize,
}

/// The budget curve, ascending by window; flat beyond both ends.
static PROFILE_ANCHORS: [ProfileAnchor; 6] = [
    // Jetson-class; 768 reserve since a thinking block alone measured 306 tokens here.
    ProfileAnchor {
        window: 4_096,
        compaction_threshold: 0.60,
        memory_token_budget: 200,
        max_memory_fragments: 3,
        system_prompt_budget: 1_500,
        history_token_budget: 1_200,
        output_reserve_tokens: 768,
    },
    // The local prompt clamp, and the macOS Metal default.
    ProfileAnchor {
        window: 8_192,
        compaction_threshold: 0.70,
        memory_token_budget: 500,
        max_memory_fragments: 5,
        system_prompt_budget: 3_000,
        history_token_budget: 4_000,
        output_reserve_tokens: 1_024,
    },
    // Same values as 8,192, so 8,192..12,288 is flat.
    ProfileAnchor {
        window: 12_288,
        compaction_threshold: 0.70,
        memory_token_budget: 500,
        max_memory_fragments: 5,
        system_prompt_budget: 3_000,
        history_token_budget: 4_000,
        output_reserve_tokens: 1_024,
    },
    // What the name heuristic gives qwen and mistral.
    ProfileAnchor {
        window: 32_768,
        compaction_threshold: 0.75,
        memory_token_budget: 1_500,
        max_memory_fragments: 10,
        system_prompt_budget: 6_000,
        history_token_budget: 20_000,
        output_reserve_tokens: 2_048,
    },
    // Same values as 32,768, so 32,768..65,536 is flat.
    ProfileAnchor {
        window: 65_536,
        compaction_threshold: 0.75,
        memory_token_budget: 1_500,
        max_memory_fragments: 10,
        system_prompt_budget: 6_000,
        history_token_budget: 20_000,
        output_reserve_tokens: 2_048,
    },
    // Large-context HTTP models (gemma-4 by name heuristic).
    ProfileAnchor {
        window: 128_000,
        compaction_threshold: 0.80,
        memory_token_budget: 4_000,
        max_memory_fragments: 15,
        system_prompt_budget: 10_000,
        history_token_budget: 80_000,
        output_reserve_tokens: 4_096,
    },
];

/// The `t` short-circuits make an exact anchor return its own integer, not a value one ULP off.
fn lerp_budget(a: usize, b: usize, t: f64) -> usize {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    (a as f64 + (b as f64 - a as f64) * t).round() as usize
}

fn lerp_threshold(a: f32, b: f32, t: f64) -> f32 {
    if t <= 0.0 {
        return a;
    }
    if t >= 1.0 {
        return b;
    }
    (a as f64 + (b as f64 - a as f64) * t) as f32
}

impl CompactionProfile {
    /// Piecewise-linear over [`PROFILE_ANCHORS`]; every budget is non-decreasing in the window.
    pub fn from_context_window(context_tokens: usize) -> Self {
        let first = &PROFILE_ANCHORS[0];
        let last = &PROFILE_ANCHORS[PROFILE_ANCHORS.len() - 1];

        let (lo, hi, t) = if context_tokens <= first.window {
            (first, first, 0.0)
        } else if context_tokens >= last.window {
            (last, last, 0.0)
        } else {
            // Ascending table, `context_tokens` strictly inside: the index is always >= 1.
            let i = PROFILE_ANCHORS
                .iter()
                .position(|a| a.window >= context_tokens)
                .expect("context_tokens is below the last anchor");
            let lo = &PROFILE_ANCHORS[i - 1];
            let hi = &PROFILE_ANCHORS[i];
            let t = (context_tokens - lo.window) as f64 / (hi.window - lo.window) as f64;
            (lo, hi, t)
        };

        Self {
            compaction_threshold: lerp_threshold(
                lo.compaction_threshold,
                hi.compaction_threshold,
                t,
            ),
            memory_token_budget: lerp_budget(lo.memory_token_budget, hi.memory_token_budget, t),
            max_memory_fragments: lerp_budget(lo.max_memory_fragments, hi.max_memory_fragments, t),
            system_prompt_budget: lerp_budget(lo.system_prompt_budget, hi.system_prompt_budget, t),
            history_token_budget: lerp_budget(lo.history_token_budget, hi.history_token_budget, t),
            output_reserve_tokens: lerp_budget(
                lo.output_reserve_tokens,
                hi.output_reserve_tokens,
                t,
            ),
            context_window_tokens: context_tokens,
            prompt_window_tokens: context_tokens,
        }
    }

    /// Preamble budgets from `prompt_window`, history from the full window plus what that frees.
    pub fn for_windows(context_window: usize, prompt_window: usize) -> Self {
        let full = Self::from_context_window(context_window);
        if prompt_window >= context_window {
            return full;
        }
        let capped = Self::from_context_window(prompt_window);
        let freed = (full.system_prompt_budget + full.memory_token_budget)
            .saturating_sub(capped.system_prompt_budget + capped.memory_token_budget);
        Self {
            memory_token_budget: capped.memory_token_budget,
            max_memory_fragments: capped.max_memory_fragments,
            system_prompt_budget: capped.system_prompt_budget,
            history_token_budget: full.history_token_budget + freed,
            prompt_window_tokens: prompt_window,
            ..full
        }
    }

    /// Hold back `fraction` of history for a second agent on the same window. Only history
    /// moves (the preamble is the KV prefix); a fraction outside `0.0..=1.0` reserves all.
    pub fn with_history_reserved(&self, fraction: f32) -> Self {
        let fraction = if fraction.is_finite() && (0.0..=1.0).contains(&fraction) {
            fraction
        } else {
            1.0
        };
        if fraction <= 0.0 {
            return self.clone();
        }
        let claimable = self.history_token_budget.min(self.usable_history_tokens());
        // `ceil`, so an awkward ratio errs toward reserving MORE.
        let reserved = (claimable as f64 * fraction as f64).ceil() as usize;
        Self {
            history_token_budget: claimable.saturating_sub(reserved),
            ..self.clone()
        }
    }

    /// Reads the PROMPT window: a big KV cache must not buy a local provider a verbose prefix.
    pub fn use_compact_prompt(&self) -> bool {
        self.prompt_window_tokens <= 12288
    }
}

// ── Reasoning effort ────────────────────────────────────────────────────────

/// Thinking-room preference: picks a share of the budget [`reasoning_budget_tokens`] derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    /// On-device default: Orin decode runs at ~`102 / model_GB` tok/s, so thinking is silence.
    Brief,
    Balanced,
    /// For HTTP providers, where thinking tokens are cheap and fast.
    Thorough,
}

impl ReasoningEffort {
    /// Every variant, in increasing order of spend.
    pub const ALL: &'static [ReasoningEffort] = &[
        ReasoningEffort::Brief,
        ReasoningEffort::Balanced,
        ReasoningEffort::Thorough,
    ];

    /// Unrecognised values fall back to the SMALLEST budget, [`ReasoningEffort::Brief`].
    pub fn parse(raw: &str) -> Self {
        match raw {
            "balanced" => ReasoningEffort::Balanced,
            "thorough" => ReasoningEffort::Thorough,
            "brief" => ReasoningEffort::Brief,
            other => {
                if !other.is_empty() {
                    tracing::warn!(
                        "unrecognised reasoning_effort {other:?} — falling back to \"brief\""
                    );
                }
                ReasoningEffort::Brief
            }
        }
    }

    /// The stored string form.
    pub fn as_str(&self) -> &'static str {
        match self {
            ReasoningEffort::Brief => "brief",
            ReasoningEffort::Balanced => "balanced",
            ReasoningEffort::Thorough => "thorough",
        }
    }
}

/// Floor under any thinking budget. Zero is `thinking_mode = "off"`'s job (no `<thinking>` at all).
const MIN_REASONING_BUDGET_TOKENS: usize = 32;

/// A `<thinking>` block's token budget: a slice of the output reserve, which the answer also uses.
/// At most half, so the answer always keeps at least half the reserve.
pub fn reasoning_budget_tokens(profile: &CompactionProfile, effort: ReasoningEffort) -> usize {
    let reserve = profile.output_reserve_tokens;
    let share = match effort {
        ReasoningEffort::Brief => reserve / 8,
        ReasoningEffort::Balanced => reserve / 4,
        ReasoningEffort::Thorough => reserve / 2,
    };
    share.max(MIN_REASONING_BUDGET_TOKENS)
}

// ── A reserve derived from what reasoning actually costs ────────────────────

/// Fewest observations before this pond's own behaviour may move the reserve off the anchor.
pub const MIN_REASONING_SAMPLES: usize = 20;

/// Round-up step for a derived reserve, so the trim point doesn't move on most turns.
pub const RESERVE_QUANTUM_TOKENS: usize = 256;

/// Max window share observation alone may give the reserve; the anchor may exceed it.
pub const MAX_OBSERVED_RESERVE_SHARE: f32 = 0.25;

/// Not the mean (half the turns would overrun) nor the max (one outlier taxes all history).
const REASONING_PERCENTILE: f64 = 0.95;

/// Reserve from observed `reasoning_tokens`; pass only MEASURED turns (`None` as 0 shrinks it).
/// `2 * p95` (Thorough gets half), quantised up, share-clamped, then floored at `anchor`.
pub fn observed_output_reserve(samples: &[u32], anchor: usize, window: usize) -> usize {
    if samples.len() < MIN_REASONING_SAMPLES {
        return anchor;
    }

    let mut sorted: Vec<u32> = samples.to_vec();
    sorted.sort_unstable();
    // Nearest-rank: on 20 samples the 19th, so one outlier doesn't set the reserve.
    let rank = ((sorted.len() as f64) * REASONING_PERCENTILE).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    let p95 = sorted[index] as usize;

    let needed = p95.saturating_mul(2);
    let quantised = needed
        .div_ceil(RESERVE_QUANTUM_TOKENS)
        .saturating_mul(RESERVE_QUANTUM_TOKENS);

    let ceiling = ((window as f32) * MAX_OBSERVED_RESERVE_SHARE) as usize;
    quantised.min(ceiling).max(anchor)
}

/// Compact-tier sample window: `ContextGovernor::prompt_window` clamps local providers to it.
const COMPACT_TIER_SAMPLE_WINDOW: usize = 8_192;

/// Roomy-tier sample window: the name heuristic's qwen/mistral value; flat to 65,536.
const ROOMY_TIER_SAMPLE_WINDOW: usize = 32_768;

/// Token budget as words (~3 per 4 tokens), rounded down; models heed word counts better.
pub fn budget_as_words(budget_tokens: usize) -> usize {
    budget_tokens * 3 / 4
}

/// Words a `<thinking>` block may run to, i.e. [`reasoning_budget_tokens`] in words. The
/// renderer knows only `PromptState::compact_prompt`, so this samples one window per tier.
pub fn reasoning_budget_words(effort: ReasoningEffort, compact_prompt: bool) -> usize {
    let window = if compact_prompt {
        COMPACT_TIER_SAMPLE_WINDOW
    } else {
        ROOMY_TIER_SAMPLE_WINDOW
    };
    let profile = CompactionProfile::from_context_window(window);
    budget_as_words(reasoning_budget_tokens(&profile, effort))
}

/// History budget in chars after prompt and schema overhead; floored so the latest turn survives.
pub fn available_history_chars(
    profile: &CompactionProfile,
    system_prompt_chars: usize,
    tool_schema_chars: usize,
) -> usize {
    let total_budget_chars = profile.history_token_budget * CHARS_PER_TOKEN;
    let overhead = system_prompt_chars + tool_schema_chars;
    total_budget_chars
        .saturating_sub(overhead)
        .max(MIN_USABLE_HISTORY_CHARS)
}

/// Max tool output kept verbatim in history, in BYTES (unlike chat.rs's `TOOL_RESULT_MAX_CHARS`).
pub const TOOL_RESULT_MAX_BYTES: usize = 1_500;

/// Shrink to ~`max_chars`, keeping ~60% head and ~40% tail (where totals and summaries sit).
/// `None` if it fits; may exceed `max_chars` by the marker's length.
pub fn truncate_head_tail(text: &str, max_chars: usize) -> Option<String> {
    if text.len() <= max_chars {
        return None;
    }
    // Degenerate budgets: a head-only cut is all that fits.
    if max_chars < 64 {
        let mut end = max_chars.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        return Some(format!(
            "{}\n[... truncated {} chars ...]",
            &text[..end],
            text.len() - end
        ));
    }

    let head_len = max_chars * 3 / 5;
    let tail_len = max_chars - head_len;

    let mut head_end = head_len;
    while head_end > 0 && !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len().saturating_sub(tail_len);
    while tail_start < text.len() && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    // A multi-byte boundary walk can cross the head, leaving no tail to keep.
    if tail_start <= head_end {
        return Some(format!(
            "{}\n[... truncated {} chars ...]",
            &text[..head_end],
            text.len() - head_end
        ));
    }

    let dropped = tail_start - head_end;
    Some(format!(
        "{}\n[... truncated {dropped} chars ...]\n{}",
        &text[..head_end],
        &text[tail_start..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── truncate_head_tail ───────────────────────────────────────────────

    #[test]
    fn text_within_budget_is_left_alone() {
        assert!(truncate_head_tail("short", TOOL_RESULT_MAX_BYTES).is_none());
        let exact = "x".repeat(TOOL_RESULT_MAX_BYTES);
        assert!(truncate_head_tail(&exact, TOOL_RESULT_MAX_BYTES).is_none());
    }

    #[test]
    fn both_ends_survive_and_the_marker_states_the_loss() {
        let text = format!("HEAD-MARKER{}TAIL-MARKER", "x".repeat(50_000));
        let out = truncate_head_tail(&text, TOOL_RESULT_MAX_BYTES).unwrap();
        assert!(out.starts_with("HEAD-MARKER"), "{}", &out[..40]);
        assert!(out.ends_with("TAIL-MARKER"), "{}", &out[out.len() - 40..]);
        assert!(out.contains("[... truncated "));
        assert!(out.len() < TOOL_RESULT_MAX_BYTES + 64, "len {}", out.len());
        let dropped: usize = out
            .split("[... truncated ")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .and_then(|s| s.parse().ok())
            .unwrap();
        assert_eq!(
            dropped,
            text.len() - (out.len() - format!("\n[... truncated {dropped} chars ...]\n").len())
        );
    }

    #[test]
    fn multibyte_text_is_never_split_mid_char() {
        // Every char is 4 bytes, so naive byte slicing would panic.
        let text = "\u{1F600}".repeat(2_000);
        let out = truncate_head_tail(&text, TOOL_RESULT_MAX_BYTES).unwrap();
        assert!(out.contains("[... truncated "));
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn a_tiny_budget_degrades_to_a_head_cut() {
        let text = "y".repeat(500);
        let out = truncate_head_tail(&text, 10).unwrap();
        assert!(out.starts_with("yyyyyyyyyy"));
        assert!(out.contains("[... truncated 490 chars ...]"));
    }

    // ── CompactionProfile tests ─────────────────────────────────────────

    #[test]
    fn compaction_profile_jetson_3k() {
        let p = CompactionProfile::from_context_window(3072);
        assert!((p.compaction_threshold - 0.60).abs() < f32::EPSILON);
        assert_eq!(p.memory_token_budget, 200);
        assert_eq!(p.max_memory_fragments, 3);
        assert_eq!(p.system_prompt_budget, 1500);
        assert_eq!(p.history_token_budget, 1200);
        assert!(p.use_compact_prompt());
    }

    #[test]
    fn compaction_profile_macos_8k() {
        let p = CompactionProfile::from_context_window(8192);
        assert!((p.compaction_threshold - 0.70).abs() < f32::EPSILON);
        assert_eq!(p.memory_token_budget, 500);
        assert_eq!(p.max_memory_fragments, 5);
        assert_eq!(p.system_prompt_budget, 3000);
        assert_eq!(p.history_token_budget, 4000);
        assert!(p.use_compact_prompt());
    }

    #[test]
    fn compaction_profile_32k() {
        let p = CompactionProfile::from_context_window(32768);
        assert!((p.compaction_threshold - 0.75).abs() < f32::EPSILON);
        assert_eq!(p.memory_token_budget, 1500);
        assert_eq!(p.max_memory_fragments, 10);
        assert_eq!(p.system_prompt_budget, 6000);
        assert_eq!(p.history_token_budget, 20000);
        assert!(!p.use_compact_prompt());
    }

    #[test]
    fn compaction_profile_128k() {
        let p = CompactionProfile::from_context_window(128_000);
        assert!((p.compaction_threshold - 0.80).abs() < f32::EPSILON);
        assert_eq!(p.memory_token_budget, 4000);
        assert_eq!(p.max_memory_fragments, 15);
        assert_eq!(p.system_prompt_budget, 10000);
        assert_eq!(p.history_token_budget, 80000);
        assert!(!p.use_compact_prompt());
    }

    #[test]
    fn available_history_chars_subtracts_overhead() {
        let profile = CompactionProfile::from_context_window(8192);
        // 4000 tokens * 4 chars/token = 16000 chars total budget
        let avail = available_history_chars(&profile, 1000, 500);
        assert_eq!(avail, 16_000 - 1500);
    }

    #[test]
    fn available_history_chars_clamps_to_min_when_overhead_exceeds_budget() {
        let profile = CompactionProfile::from_context_window(3072);
        // 1200 * 4 = 4800 budget, overhead 10000 → should clamp to MIN_USABLE_HISTORY_CHARS
        let avail = available_history_chars(&profile, 6000, 4000);
        assert_eq!(avail, MIN_USABLE_HISTORY_CHARS);
    }

    #[test]
    fn compaction_profile_boundary_4096() {
        let p = CompactionProfile::from_context_window(4096);
        assert!((p.compaction_threshold - 0.60).abs() < f32::EPSILON);
        assert_eq!(p.max_memory_fragments, 3);
    }

    #[test]
    fn compaction_profile_boundary_12288() {
        let p = CompactionProfile::from_context_window(12288);
        assert!((p.compaction_threshold - 0.70).abs() < f32::EPSILON);
        assert_eq!(p.max_memory_fragments, 5);
    }

    #[test]
    fn compaction_profile_stores_context_window() {
        let p = CompactionProfile::from_context_window(8192);
        assert_eq!(p.context_window_tokens, 8192);
    }
    // ── The continuous profile curve, with the tiers as fixtures ─────────

    /// The old discrete tiers, verbatim, as the reference the properties below check against.
    fn tiers_before_p4(context_tokens: usize) -> (f32, usize, usize, usize, usize, usize) {
        if context_tokens <= 4096 {
            (0.60, 200, 3, 1500, 1200, 768)
        } else if context_tokens <= 12288 {
            (0.70, 500, 5, 3000, 4000, 1024)
        } else if context_tokens <= 65536 {
            (0.75, 1500, 10, 6000, 20000, 2048)
        } else {
            (0.80, 4000, 15, 10000, 80000, 4096)
        }
    }

    fn budget_sum(p: &CompactionProfile) -> usize {
        p.output_reserve_tokens
            + p.system_prompt_budget
            + p.memory_token_budget
            + p.history_token_budget
    }

    /// Windows the old tiers were pinned at, and the answers the curve must still give there.
    const TIER_FIXTURES: &[(usize, f32, usize, usize, usize, usize, usize)] = &[
        // window,   threshold, memory, frags, system, history, reserve
        (0, 0.60, 200, 3, 1500, 1200, 768),
        (3_072, 0.60, 200, 3, 1500, 1200, 768),
        (4_096, 0.60, 200, 3, 1500, 1200, 768),
        (8_192, 0.70, 500, 5, 3000, 4000, 1024),
        (12_288, 0.70, 500, 5, 3000, 4000, 1024),
        (32_768, 0.75, 1500, 10, 6000, 20000, 2048),
        (65_536, 0.75, 1500, 10, 6000, 20000, 2048),
        (128_000, 0.80, 4000, 15, 10000, 80000, 4096),
        (200_000, 0.80, 4000, 15, 10000, 80000, 4096),
    ];

    #[test]
    fn the_curve_reproduces_every_tier_fixture_exactly() {
        for &(w, thr, mem, frags, sys, hist, reserve) in TIER_FIXTURES {
            let p = CompactionProfile::from_context_window(w);
            assert!(
                (p.compaction_threshold - thr).abs() < 1e-6,
                "window {w}: compaction_threshold {} != {thr}",
                p.compaction_threshold
            );
            assert_eq!(
                p.memory_token_budget, mem,
                "window {w}: memory_token_budget"
            );
            assert_eq!(
                p.max_memory_fragments, frags,
                "window {w}: max_memory_fragments"
            );
            assert_eq!(
                p.system_prompt_budget, sys,
                "window {w}: system_prompt_budget"
            );
            assert_eq!(
                p.history_token_budget, hist,
                "window {w}: history_token_budget"
            );
            assert_eq!(
                p.output_reserve_tokens, reserve,
                "window {w}: output_reserve_tokens"
            );
            assert_eq!(
                p.context_window_tokens, w,
                "window {w}: context_window_tokens"
            );
        }
    }

    #[test]
    fn the_curve_never_promises_more_budget_than_the_tiers_did() {
        for w in 4_096..=200_000usize {
            let p = CompactionProfile::from_context_window(w);
            let (_, mem, _, sys, hist, reserve) = tiers_before_p4(w);
            let before = mem + sys + hist + reserve;
            let after = budget_sum(&p);
            assert!(
                after <= before,
                "window {w}: curve promises {after} tokens, tiers promised {before}"
            );
        }
    }

    /// The curve may still over-commit (the 8,192 fixture sums to 8,524), but the tiers did more.
    #[test]
    fn the_curve_over_commits_strictly_less_often_than_the_tiers() {
        let mut curve_violations = 0usize;
        let mut tier_violations = 0usize;
        for w in 4_096..=200_000usize {
            let p = CompactionProfile::from_context_window(w);
            let (_, mem, _, sys, hist, reserve) = tiers_before_p4(w);
            let tier_sum = mem + sys + hist + reserve;
            if budget_sum(&p) > w {
                curve_violations += 1;
                assert!(
                    tier_sum > w,
                    "window {w}: the curve over-commits where the tiers did not"
                );
            }
            if tier_sum > w {
                tier_violations += 1;
            }
        }
        assert_eq!(curve_violations, 2_116, "curve over-commitment count");
        assert_eq!(tier_violations, 54_245, "tier over-commitment count");
    }

    /// The Orin's registry pins n_ctx at 16,384, a window no tier fixture covers.
    #[test]
    fn the_orins_pinned_window_stops_promising_more_than_the_window_holds() {
        let p = CompactionProfile::from_context_window(16_384);
        let sum = budget_sum(&p);
        assert!(
            sum <= p.context_window_tokens,
            "budgets sum to {sum} against a {}-token window",
            p.context_window_tokens
        );
        assert_eq!(sum, 12_729, "the Orin budget sum moved; re-derive it");
        assert_eq!(p.output_reserve_tokens, 1_229);
        assert_eq!(p.system_prompt_budget, 3_600);
        assert_eq!(p.memory_token_budget, 700);
        assert_eq!(p.max_memory_fragments, 6);
        assert_eq!(p.history_token_budget, 7_200);

        let (_, mem, _, sys, hist, reserve) = tiers_before_p4(16_384);
        assert_eq!(mem + sys + hist + reserve, 29_548);
    }

    #[test]
    fn a_24k_model_and_a_64k_model_no_longer_share_a_bucket() {
        let k24 = CompactionProfile::from_context_window(24_576);
        let k64 = CompactionProfile::from_context_window(65_536);
        assert!(
            k24.history_token_budget < k64.history_token_budget,
            "24K got {} history tokens, 64K got {}",
            k24.history_token_budget,
            k64.history_token_budget
        );
        assert_eq!(k24.history_token_budget, 13_600);
        assert_eq!(k64.history_token_budget, 20_000);
    }

    #[test]
    fn the_output_reserve_is_never_below_its_floor_at_any_window() {
        for w in [0, 1, 512, 3_072, 4_096, 6_000, 8_192, 100_000, 1_000_000] {
            let p = CompactionProfile::from_context_window(w);
            assert!(
                p.output_reserve_tokens >= 768,
                "window {w}: reserve {}",
                p.output_reserve_tokens
            );
        }
    }

    /// Raising `context_window_override` by one token must never cost the user history.
    #[test]
    fn every_budget_is_non_decreasing_in_the_window() {
        let mut prev = CompactionProfile::from_context_window(4_096);
        for w in 4_097..=200_000usize {
            let p = CompactionProfile::from_context_window(w);
            assert!(
                p.compaction_threshold >= prev.compaction_threshold,
                "threshold at {w}"
            );
            assert!(
                p.memory_token_budget >= prev.memory_token_budget,
                "memory at {w}"
            );
            assert!(
                p.max_memory_fragments >= prev.max_memory_fragments,
                "fragments at {w}"
            );
            assert!(
                p.system_prompt_budget >= prev.system_prompt_budget,
                "system at {w}"
            );
            assert!(
                p.history_token_budget >= prev.history_token_budget,
                "history at {w}"
            );
            assert!(
                p.output_reserve_tokens >= prev.output_reserve_tokens,
                "reserve at {w}"
            );
            prev = p;
        }
    }

    #[test]
    fn use_compact_prompt_is_still_a_hard_step_at_12288() {
        assert!(CompactionProfile::from_context_window(12_288).use_compact_prompt());
        assert!(!CompactionProfile::from_context_window(12_289).use_compact_prompt());
    }

    // ── Asymmetric budgeting - preamble capped, working set scaled ───────

    /// The preamble is the KV prefix, re-prefilled every turn: growing it grows TTFT for good.
    #[test]
    fn growing_the_window_buys_history_and_never_preamble() {
        let small = CompactionProfile::for_windows(8_192, 8_192);
        let big = CompactionProfile::for_windows(32_768, 8_192);

        assert_eq!(
            big.system_prompt_budget, small.system_prompt_budget,
            "a 4x window bought a bigger system prompt: {} vs {}",
            big.system_prompt_budget, small.system_prompt_budget
        );
        assert_eq!(
            big.memory_token_budget, small.memory_token_budget,
            "a 4x window bought a bigger memory block: {} vs {}",
            big.memory_token_budget, small.memory_token_budget
        );
        assert_eq!(
            big.max_memory_fragments, small.max_memory_fragments,
            "a 4x window bought more memory fragments"
        );
        assert_eq!(
            big.use_compact_prompt(),
            small.use_compact_prompt(),
            "a 4x window flipped the prompt to the verbose tier"
        );

        assert!(
            big.history_token_budget > small.history_token_budget,
            "a 4x window bought no extra history: {} vs {}",
            big.history_token_budget,
            small.history_token_budget
        );
        // The exact numbers, so a silent re-tune is visible in the diff.
        assert_eq!(small.history_token_budget, 4_000);
        assert_eq!(big.history_token_budget, 24_000);
        assert_eq!(big.system_prompt_budget, 3_000);
        assert_eq!(big.memory_token_budget, 500);
    }

    #[test]
    fn capping_the_preamble_moves_tokens_to_history_and_creates_none() {
        for window in [8_193usize, 12_288, 16_384, 32_768, 65_536, 128_000] {
            let symmetric = CompactionProfile::from_context_window(window);
            let asymmetric = CompactionProfile::for_windows(window, 8_192);
            assert_eq!(
                budget_sum(&asymmetric),
                budget_sum(&symmetric),
                "window {window}: the split changed the TOTAL budget"
            );
            assert!(
                asymmetric.history_token_budget >= symmetric.history_token_budget,
                "window {window}: capping the preamble cost history"
            );
        }
    }

    /// HTTP providers take this path; any drift silently re-tunes all of them.
    #[test]
    fn an_unclamped_prompt_window_reproduces_from_context_window_exactly() {
        for &(w, ..) in TIER_FIXTURES {
            let a = CompactionProfile::from_context_window(w);
            for prompt in [w, w + 1, w * 2 + 1] {
                let b = CompactionProfile::for_windows(w, prompt);
                assert_eq!(budget_sum(&a), budget_sum(&b), "window {w} prompt {prompt}");
                assert_eq!(a.history_token_budget, b.history_token_budget, "window {w}");
                assert_eq!(a.system_prompt_budget, b.system_prompt_budget, "window {w}");
                assert_eq!(a.memory_token_budget, b.memory_token_budget, "window {w}");
                assert_eq!(a.max_memory_fragments, b.max_memory_fragments, "window {w}");
                assert_eq!(a.prompt_window_tokens, b.prompt_window_tokens, "window {w}");
                assert_eq!(a.use_compact_prompt(), b.use_compact_prompt(), "window {w}");
            }
        }
    }

    #[test]
    fn the_preamble_allowance_is_flat_across_every_clamped_window() {
        let clamp = 8_192usize;
        let reference = CompactionProfile::from_context_window(clamp);
        for window in (clamp..=200_000).step_by(97) {
            let p = CompactionProfile::for_windows(window, clamp.min(window));
            assert_eq!(
                p.system_prompt_budget, reference.system_prompt_budget,
                "window {window}"
            );
            assert_eq!(
                p.memory_token_budget, reference.memory_token_budget,
                "window {window}"
            );
            assert_eq!(
                p.max_memory_fragments, reference.max_memory_fragments,
                "window {window}"
            );
            assert!(p.use_compact_prompt(), "window {window}");
        }
    }

    #[test]
    fn the_history_ceiling_subtracts_the_preamble_the_prompt_ceiling_does_not() {
        let p = CompactionProfile::from_context_window(8_192);
        // The engine's prompt_tokens ceiling: must still cover preamble plus history.
        assert_eq!(p.usable_prompt_tokens(), 8_192 - 1_024);
        // History alone may not claim the preamble's room.
        assert_eq!(p.usable_history_tokens(), 7_168 - 3_000 - 500);
        assert!(
            p.usable_history_tokens() < p.history_token_budget,
            "the declared 4,000-token history budget was payable after all"
        );
    }

    #[test]
    fn a_preamble_bigger_than_the_window_leaves_zero_history_room() {
        let p = CompactionProfile::from_context_window(1_024);
        assert_eq!(p.system_prompt_budget, 1_500);
        assert_eq!(p.usable_prompt_tokens(), 256);
        assert_eq!(p.usable_history_tokens(), 0);
    }

    // ── Reasoning effort ────────────────────────────────────────────────────

    /// Bidirectional: a new effort added to only the enum or only the settings constant fails.
    #[test]
    fn reasoning_effort_strings_agree_with_settings() {
        use crate::user_data::domain::settings::REASONING_EFFORTS;

        let from_enum: Vec<&str> = ReasoningEffort::ALL.iter().map(|e| e.as_str()).collect();
        assert_eq!(
            from_enum, REASONING_EFFORTS,
            "ReasoningEffort::ALL and settings::REASONING_EFFORTS have diverged"
        );
        for s in REASONING_EFFORTS {
            assert_eq!(
                ReasoningEffort::parse(s).as_str(),
                *s,
                "round trip failed for {s:?}"
            );
        }
    }

    #[test]
    fn an_unrecognised_reasoning_effort_narrows_to_brief() {
        for bad in ["", "Thorough", "maximum", "high", "off", "true"] {
            assert_eq!(
                ReasoningEffort::parse(bad),
                ReasoningEffort::Brief,
                "{bad:?} must fall back to the smallest budget"
            );
        }
        // And the fallback really is the smallest, not merely a named variant.
        let p = CompactionProfile::from_context_window(8_192);
        let brief = reasoning_budget_tokens(&p, ReasoningEffort::Brief);
        for e in ReasoningEffort::ALL {
            assert!(
                brief <= reasoning_budget_tokens(&p, *e),
                "Brief is not the smallest budget — the parse fallback widens scope"
            );
        }
    }

    #[test]
    fn the_reasoning_budget_is_a_share_of_the_output_reserve() {
        for w in [3_072usize, 4_096, 8_192, 12_288, 32_768, 65_536, 128_000] {
            let p = CompactionProfile::from_context_window(w);
            let b = reasoning_budget_tokens(&p, ReasoningEffort::Brief);
            let m = reasoning_budget_tokens(&p, ReasoningEffort::Balanced);
            let t = reasoning_budget_tokens(&p, ReasoningEffort::Thorough);
            assert!(b <= m && m <= t, "window {w}: not monotonic in effort");
            assert!(
                b >= MIN_REASONING_BUDGET_TOKENS,
                "window {w}: budget fell to zero — that is thinking_mode's job"
            );
            assert!(
                t * 2 <= p.output_reserve_tokens,
                "window {w}: thinking may claim more than half the answer's room"
            );
        }
        // Bigger window, bigger think.
        let small = CompactionProfile::from_context_window(8_192);
        let big = CompactionProfile::from_context_window(65_536);
        assert!(
            reasoning_budget_tokens(&small, ReasoningEffort::Brief)
                < reasoning_budget_tokens(&big, ReasoningEffort::Brief)
        );
    }

    #[test]
    fn the_two_point_sample_matches_the_profile_it_stands_for() {
        for (compact, window) in [(true, 8_192usize), (false, 32_768)] {
            let p = CompactionProfile::from_context_window(window);
            assert_eq!(p.use_compact_prompt(), compact, "window {window}");
            for e in ReasoningEffort::ALL {
                assert_eq!(
                    reasoning_budget_words(*e, compact),
                    budget_as_words(reasoning_budget_tokens(&p, *e)),
                    "window {window}, effort {}",
                    e.as_str()
                );
            }
        }
    }

    #[test]
    fn every_effort_renders_a_distinct_non_zero_word_budget() {
        for compact in [true, false] {
            let words: Vec<usize> = ReasoningEffort::ALL
                .iter()
                .map(|e| reasoning_budget_words(*e, compact))
                .collect();
            assert!(
                words.iter().all(|w| *w > 0),
                "compact={compact}: a zero word budget reached the prompt"
            );
            assert!(
                words[0] < words[1] && words[1] < words[2],
                "compact={compact}: efforts do not produce distinct budgets: {words:?}"
            );
        }
        // The tight tier must ask for less than the roomy one at equal effort.
        for e in ReasoningEffort::ALL {
            assert!(
                reasoning_budget_words(*e, true) < reasoning_budget_words(*e, false),
                "effort {}: the on-device tier is not cheaper",
                e.as_str()
            );
        }
    }

    // ── Reserving history for a live subagent ───────────────────────────────

    /// Field by field, so `the_reservation_covers_every_field_of_the_profile` catches a new field.
    #[test]
    fn a_reservation_takes_working_set_and_never_preamble() {
        let base = CompactionProfile::for_windows(32_768, 8_192);
        let reserved = base.with_history_reserved(0.3);

        assert!(
            reserved.history_token_budget < base.history_token_budget,
            "the history budget did not shrink: {} vs {}",
            reserved.history_token_budget,
            base.history_token_budget
        );
        assert_eq!(
            reserved.system_prompt_budget, base.system_prompt_budget,
            "the system prompt allowance moved, so the preamble is rebuilt and the KV prefix \
             moves with it"
        );
        assert_eq!(
            reserved.memory_token_budget, base.memory_token_budget,
            "the memory allowance moved"
        );
        assert_eq!(
            reserved.max_memory_fragments, base.max_memory_fragments,
            "the fragment count moved"
        );
        assert_eq!(
            reserved.output_reserve_tokens, base.output_reserve_tokens,
            "the output reserve moved - the model would have less room to answer because a \
             SUBAGENT is running"
        );
        assert_eq!(
            reserved.context_window_tokens, base.context_window_tokens,
            "the window itself moved"
        );
        assert_eq!(
            reserved.prompt_window_tokens, base.prompt_window_tokens,
            "the prompt-side window moved, which is the clamp the preamble is built from"
        );
        assert!(
            (reserved.compaction_threshold - base.compaction_threshold).abs() < f32::EPSILON,
            "the compaction threshold moved"
        );
        assert_eq!(
            reserved.use_compact_prompt(),
            base.use_compact_prompt(),
            "the prompt TIER flipped under a reservation, which rewrites the system prompt \
             wholesale"
        );
    }

    #[test]
    fn the_prompt_tier_cannot_flip_however_much_is_reserved() {
        let roomy = CompactionProfile::from_context_window(32_768);
        assert!(!roomy.use_compact_prompt(), "fixture is not the roomy tier");
        assert!(
            !roomy.with_history_reserved(1.0).use_compact_prompt(),
            "reserving the whole history budget moved the profile into the compact prompt tier"
        );
    }

    /// Every turn on a pond that never delegates takes this path, so it must be byte-identical.
    #[test]
    fn no_reservation_is_the_same_profile() {
        for window in [4_096, 8_192, 16_384, 128_000] {
            let base = CompactionProfile::for_windows(window, window.min(8_192));
            let same = base.with_history_reserved(0.0);
            assert_eq!(
                same.history_token_budget, base.history_token_budget,
                "window {window}: a zero reservation changed the history budget"
            );
            assert_eq!(same.system_prompt_budget, base.system_prompt_budget);
            assert_eq!(same.memory_token_budget, base.memory_token_budget);
            assert_eq!(same.usable_history_tokens(), base.usable_history_tokens());
        }
    }

    /// Bad fractions are ledger defects (`AgentRole::new` rejects them): lost recall beats overrun.
    #[test]
    fn a_fraction_that_is_not_a_fraction_reserves_the_whole_budget() {
        let base = CompactionProfile::from_context_window(8_192);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1.5, -0.5, 47.0] {
            let reserved = base.with_history_reserved(bad);
            assert_eq!(
                reserved.history_token_budget, 0,
                "fraction {bad} left the parent {} tokens instead of reserving everything",
                reserved.history_token_budget
            );
        }
        // Vacuity control: a GOOD fraction must still leave the parent something.
        assert!(
            base.with_history_reserved(0.5).history_token_budget > 0,
            "an ordinary fraction also left the parent nothing, so the bad-input assertion above \
             proves nothing"
        );
    }

    /// At 8,192 declared and usable history differ by 332 tokens, enough to overflow two claims.
    #[test]
    fn the_reservation_is_taken_from_the_clamped_budget_not_the_declared_one() {
        let base = CompactionProfile::from_context_window(8_192);
        assert_eq!(base.history_token_budget, 4_000, "declared");
        assert_eq!(
            base.usable_history_tokens(),
            3_668,
            "what the window allows"
        );

        let half = base.with_history_reserved(0.5);
        assert_eq!(
            half.history_token_budget, 1_834,
            "half of the CLAMPED 3,668 is what the parent keeps; half of the declared 4,000 \
             would leave it 2,000, and 2,000 + 2,000 + the preamble overruns the window"
        );
    }

    #[test]
    fn the_reservation_covers_every_field_of_the_profile() {
        let source = include_str!("context_budget.rs");
        const DECL: &str = "pub struct CompactionProfile {";
        let start = source
            .find(DECL)
            .expect("the profile struct is no longer declared here");
        let body = &source[start + DECL.len()..];
        let end = body.find("\n}").expect("unterminated struct");
        let fields: Vec<&str> = body[..end]
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let rest = line.strip_prefix("pub ")?;
                let name = rest.split(':').next()?.trim();
                (!name.is_empty()).then_some(name)
            })
            .collect();

        assert_eq!(
            fields.len(),
            8,
            "CompactionProfile now has {} fields ({fields:?}); \
             a_reservation_takes_working_set_and_never_preamble asserts on each of them by hand, \
             so decide whether a subagent reservation may move the new one and add it there",
            fields.len()
        );
    }

    // ── The observed reserve ───────────────────────────────────────────────

    /// Twenty samples of `cost`, the minimum that lets observation speak.
    fn samples(cost: u32) -> Vec<u32> {
        vec![cost; MIN_REASONING_SAMPLES]
    }

    #[test]
    fn a_pond_with_too_little_evidence_keeps_the_measured_anchor() {
        for n in 0..MIN_REASONING_SAMPLES {
            let observed = vec![4_000u32; n];
            assert_eq!(
                observed_output_reserve(&observed, 768, 4_096),
                768,
                "{n} samples moved the reserve. Below the minimum the anchor stands, because a \
                 handful of observations is not evidence about this pond -- and these samples are \
                 huge, so a test that only tried SMALL ones would pass while the guard was gone"
            );
        }
        // Vacuity control: with enough samples the same cost DOES move it.
        assert!(observed_output_reserve(&samples(4_000), 768, 4_096) > 768);
    }

    /// A reserve moving with each sample would re-prefill every turn (4.19 s on the Orin at 4K).
    #[test]
    fn a_growing_sample_set_does_not_move_the_reserve_every_turn() {
        // Monotonically climbing cost: a cycling set would pass even with quantisation deleted.
        let mut observed: Vec<u32> = Vec::new();
        let mut reserves: Vec<usize> = Vec::new();
        let mut raw: Vec<usize> = Vec::new();
        for turn in 0..120u32 {
            observed.push(200 + turn * 3);
            reserves.push(observed_output_reserve(&observed, 768, 32_768));

            // The unquantised control.
            let mut sorted = observed.clone();
            sorted.sort_unstable();
            let rank = ((sorted.len() as f64) * REASONING_PERCENTILE).ceil() as usize;
            let index = rank.saturating_sub(1).min(sorted.len() - 1);
            raw.push(((sorted[index] as usize) * 2).max(768));
        }
        let changes = reserves.windows(2).filter(|w| w[0] != w[1]).count();
        let raw_changes = raw.windows(2).filter(|w| w[0] != w[1]).count();

        assert!(
            raw_changes > 40,
            "the control moved only {raw_changes} times, so this fixture does not exercise \
             quantisation at all and the assertion below would pass without it"
        );
        assert!(
            changes * 5 < raw_changes,
            "the reserve moved {changes} times across 120 turns where the unquantised derivation \
             moved {raw_changes}. Every move shifts the trim point, which truncates the \
             reusable KV prefix back to the preamble and re-prefills the history. Quantisation to \
             {RESERVE_QUANTUM_TOKENS} tokens is what bounds it; without it PAI-5 P5 pays that on \
             nearly every turn"
        );
        // Vacuity control: a step change in reasoning cost must still move the reserve.
        let cheap = observed_output_reserve(&samples(100), 256, 8_192);
        let dear = observed_output_reserve(&samples(900), 256, 8_192);
        assert!(
            dear > cheap,
            "the reserve is inert: {cheap} for 100-token reasoning and {dear} for 900. A constant \
             that never moves is the thing this phase replaced"
        );
    }

    #[test]
    fn the_reserve_is_sized_so_a_typical_thinking_block_still_fits_at_thorough() {
        // Thorough gets half the reserve, so a typical observed block must fit in that half.
        for cost in [120u32, 300, 640] {
            let reserve = observed_output_reserve(&samples(cost), 256, 32_768);
            let profile = CompactionProfile {
                output_reserve_tokens: reserve,
                ..CompactionProfile::from_context_window(32_768)
            };
            let budget = reasoning_budget_tokens(&profile, ReasoningEffort::Thorough);
            assert!(
                budget >= cost as usize,
                "reasoning costs {cost} tokens here and Thorough is budgeted {budget}. The block \
                 would be cut off or overrun the window mid-generation, which is the failure \
                 PAI-5 P5 exists to remove"
            );
        }
    }

    #[test]
    fn observation_can_never_lower_the_reserve_below_the_measured_anchor() {
        // A pond that has only ever seen trivial reasoning.
        let reserve = observed_output_reserve(&samples(8), 768, 4_096);
        assert_eq!(
            reserve, 768,
            "cheap reasoning talked the reserve below the anchor. The anchor came from an \
             observed mid-generation overrun on real hardware; observation may raise it and must \
             never lower it"
        );
    }

    #[test]
    fn a_verbose_model_cannot_reserve_the_whole_window_away_from_history() {
        let window = 4_096;
        let reserve = observed_output_reserve(&samples(9_000), 768, window);
        let share = reserve as f32 / window as f32;
        assert!(
            share <= MAX_OBSERVED_RESERVE_SHARE + f32::EPSILON,
            "a verbose model took {share} of the window ({reserve} of {window}). A conversation \
             with no room for history is not a conversation"
        );
    }

    #[test]
    fn one_outlier_does_not_set_the_reserve_but_the_top_of_the_range_does() {
        let mut mostly_cheap = vec![100u32; MIN_REASONING_SAMPLES];
        mostly_cheap[0] = 20_000;
        let with_outlier = observed_output_reserve(&mostly_cheap, 256, 32_768);
        let without = observed_output_reserve(&samples(100), 256, 32_768);
        assert_eq!(
            with_outlier, without,
            "a single 20,000-token turn moved the reserve. The mean would let it; a percentile \
             must not, or every pond is taxed forever by its worst turn"
        );

        // A dearer TOP of the range must move it, or a median-only p95 would pass too.
        let mut top_heavy = vec![100u32; MIN_REASONING_SAMPLES];
        for slot in top_heavy.iter_mut().take(3) {
            *slot = 1_200;
        }
        assert!(
            observed_output_reserve(&top_heavy, 256, 32_768) > without,
            "three dear turns in twenty did not move the reserve; the percentile is reading too \
             low and turns will be cut off"
        );
    }
}
