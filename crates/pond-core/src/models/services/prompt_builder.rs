//! System prompt split into a KV-cache-reusable static prefix and a per-turn suffix.
//! Extras and skills come later via `extend_system_prompt`; `prefix_hash` must not cover them.

use crate::prompts::{
    render_jinja_template, sanitize_field, ProfileContext, PromptState, PROMPT_BALANCED,
};
use crate::user_data::domain::settings::Settings;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Partitioned prompt; an unchanged `prefix_hash` lets callers skip `override_system_prompt()`.
#[derive(Debug, Clone)]
pub struct PromptPartition {
    /// Turn-stable part: changes only with settings, model capabilities or the selected tools.
    pub static_prefix: String,
    /// Per-turn content: date/time, profile lines, addendum.
    pub dynamic_suffix: String,
    /// Hash of `static_prefix` for cheap equality checks.
    pub prefix_hash: u64,
}

/// Change detection only: not cryptographic, and stable only within one process.
fn hash_string(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Build the partitioned prompt; date and time go to the suffix only.
pub fn build_prompt_partition(
    settings: &Settings,
    profile: Option<&ProfileContext>,
    state: &PromptState,
    template_content: &str,
) -> PromptPartition {
    // ── Static prefix: render template with stable vars, blank temporal ──
    let static_state = PromptState {
        current_date: String::new(),
        current_time: String::new(),
        // Carry all non-temporal fields from the caller's state.
        voice_mode: state.voice_mode,
        available_tools: state.available_tools.clone(),
        thinking_enabled: state.thinking_enabled,
        compact_prompt: state.compact_prompt,
        native_tools_json: state.native_tools_json,
        tools_offered: state.tools_offered,
        prefix_hash: None,
    };

    let template = if let Some(ref custom) = settings.custom_system_prompt {
        sanitize_field(custom, 4000)
    } else {
        template_content.to_string()
    };

    let static_prefix = render_jinja_template(&template, settings, Some(&static_state), profile);

    // ── Dynamic suffix: temporal context + profile lines + addendum ──────
    let mut dynamic_parts: Vec<String> = Vec::with_capacity(8);

    // First, so the model answers time/date questions without tools.
    if !state.current_date.is_empty() || !state.current_time.is_empty() {
        let mut temporal = String::with_capacity(120);
        temporal.push_str("CURRENT CONTEXT: ");
        if !state.current_date.is_empty() {
            temporal.push_str("Today is ");
            temporal.push_str(&state.current_date);
            temporal.push('.');
        }
        if !state.current_time.is_empty() {
            temporal.push_str(" The time is ");
            temporal.push_str(&state.current_time);
            temporal.push('.');
        }
        temporal.push_str(" Answer time/date questions directly from this — no tools needed.");
        dynamic_parts.push(temporal);
    }

    dynamic_parts.extend(crate::prompts::profile_context_lines(
        profile,
        &settings.user_name,
    ));

    let addendum = sanitize_field(&settings.prompt_addendum, 500);
    if !addendum.is_empty() {
        dynamic_parts.push(addendum);
    }

    let dynamic_suffix = dynamic_parts.join("\n");
    let prefix_hash = hash_string(&static_prefix);

    PromptPartition {
        static_prefix,
        dynamic_suffix,
        prefix_hash,
    }
}

/// Prefix hash without rendering: must cover every input baked into the static prefix.
pub fn compute_prefix_hash_fast(
    settings: &Settings,
    state: &PromptState,
    template_content: &str,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Settings fields that affect the static prefix
    settings.assistant_name.hash(&mut hasher);
    settings.user_name.hash(&mut hasher);
    settings.assistant_personality.hash(&mut hasher);
    settings.timezone.hash(&mut hasher);
    settings.weather_location_name.hash(&mut hasher);
    settings.prompt_style.hash(&mut hasher);
    settings.custom_system_prompt.hash(&mut hasher);
    // Renders the <thinking> word cap.
    settings.reasoning_effort.hash(&mut hasher);
    template_content.hash(&mut hasher);
    // State baked into the static prefix; never hash anything that moves under it.
    state.voice_mode.hash(&mut hasher);
    state.thinking_enabled.hash(&mut hasher);
    // Selects four sections' compact variants and the thinking word-cap tier.
    state.compact_prompt.hash(&mut hasher);
    // Reaches the prompt through `tools_offered`, which gates every tool section.
    state.native_tools_json.hash(&mut hasher);
    // Feeds `tools_offered`; hashed in full so a same-count swap never reads as unchanged.
    state.available_tools.hash(&mut hasher);
    state.tools_offered.hash(&mut hasher);
    hasher.finish()
}

/// Built-in template for `settings.prompt_style` (balanced if unknown); never reads the DB.
pub fn resolve_builtin_template(settings: &Settings) -> &'static str {
    // An unknown style must yield balanced, never an error or an empty prompt.
    crate::prompts::builtin_template_content(&settings.prompt_style)
        .map(|(content, _)| content)
        .unwrap_or(PROMPT_BALANCED)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompts::{PROMPT_BALANCED, PROMPT_CONCISE, PROMPT_TECHNICAL, PROMPT_WARM};

    fn default_state() -> PromptState {
        PromptState {
            current_date: "Thursday, 1 May 2026".to_string(),
            current_time: "14:32".to_string(),
            voice_mode: false,
            available_tools: vec!["wikipedia — Look up factual info".to_string()],
            thinking_enabled: false,
            compact_prompt: false,
            native_tools_json: false,
            tools_offered: true,
            prefix_hash: None,
        }
    }

    #[test]
    fn static_prefix_excludes_date_and_time() {
        let settings = Settings::default();
        let state = default_state();

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(
            !partition.static_prefix.contains("Thursday, 1 May 2026"),
            "Static prefix must not contain the current date"
        );
        assert!(
            !partition.static_prefix.contains("14:32"),
            "Static prefix must not contain the current time"
        );
    }

    #[test]
    fn dynamic_suffix_contains_date_and_time() {
        let settings = Settings::default();
        let state = default_state();

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(
            partition.dynamic_suffix.contains("Thursday, 1 May 2026"),
            "Dynamic suffix must contain the current date"
        );
        assert!(
            partition.dynamic_suffix.contains("14:32"),
            "Dynamic suffix must contain the current time"
        );
    }

    #[test]
    fn static_prefix_contains_identity() {
        let mut settings = Settings::default();
        settings.assistant_name = "Duck".to_string();
        settings.user_name = "Jerry".to_string();
        let state = default_state();

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(partition.static_prefix.contains("Duck"));
        assert!(partition.static_prefix.contains("Jerry"));
    }

    #[test]
    fn consecutive_turns_produce_identical_static_prefix() {
        let settings = Settings::default();

        let state1 = PromptState {
            current_date: "Thursday, 1 May 2026".to_string(),
            current_time: "14:32".to_string(),
            ..default_state()
        };

        let state2 = PromptState {
            current_date: "Thursday, 1 May 2026".to_string(),
            current_time: "14:33".to_string(), // time changed!
            ..default_state()
        };

        let p1 = build_prompt_partition(&settings, None, &state1, PROMPT_BALANCED);
        let p2 = build_prompt_partition(&settings, None, &state2, PROMPT_BALANCED);

        assert_eq!(
            p1.static_prefix, p2.static_prefix,
            "Static prefix must be identical across turns when only time changes"
        );
        assert_eq!(
            p1.prefix_hash, p2.prefix_hash,
            "Prefix hash must be identical across turns when only time changes"
        );
    }

    #[test]
    fn dynamic_suffix_changes_when_time_changes() {
        let settings = Settings::default();

        let state1 = PromptState {
            current_date: "Thursday, 1 May 2026".to_string(),
            current_time: "14:32".to_string(),
            ..default_state()
        };

        let state2 = PromptState {
            current_date: "Thursday, 1 May 2026".to_string(),
            current_time: "14:33".to_string(),
            ..default_state()
        };

        let p1 = build_prompt_partition(&settings, None, &state1, PROMPT_BALANCED);
        let p2 = build_prompt_partition(&settings, None, &state2, PROMPT_BALANCED);

        assert_ne!(
            p1.dynamic_suffix, p2.dynamic_suffix,
            "Dynamic suffix must change when time changes"
        );
    }

    #[test]
    fn prefix_hash_changes_when_settings_change() {
        let mut settings1 = Settings::default();
        settings1.assistant_name = "Goose".to_string();

        let mut settings2 = Settings::default();
        settings2.assistant_name = "Duck".to_string();

        let state = default_state();

        let p1 = build_prompt_partition(&settings1, None, &state, PROMPT_BALANCED);
        let p2 = build_prompt_partition(&settings2, None, &state, PROMPT_BALANCED);

        assert_ne!(
            p1.prefix_hash, p2.prefix_hash,
            "Prefix hash must change when assistant name changes"
        );
    }

    #[test]
    fn a_device_going_quiet_does_not_move_the_static_prefix() {
        let settings = Settings::default();

        let registered = PromptState { ..default_state() };
        let all_quiet = PromptState {
            ..registered.clone()
        };

        let busy = build_prompt_partition(&settings, None, &registered, PROMPT_BALANCED);
        let quiet = build_prompt_partition(&settings, None, &all_quiet, PROMPT_BALANCED);

        assert_eq!(
            busy.static_prefix, quiet.static_prefix,
            "the heartbeat window must not reach the cacheable prefix"
        );
        assert_eq!(
            compute_prefix_hash_fast(&settings, &registered, PROMPT_BALANCED),
            compute_prefix_hash_fast(&settings, &all_quiet, PROMPT_BALANCED),
            "the fast hash must agree that nothing cacheable changed"
        );
    }

    #[test]
    fn the_online_list_no_longer_rides_the_dynamic_suffix() {
        let settings = Settings::default();
        let state = PromptState { ..default_state() };

        let p = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(
            !p.dynamic_suffix.contains("Online right now"),
            "the live device list must not be in the prompt: {}",
            p.dynamic_suffix
        );
        assert!(
            !p.static_prefix.contains("Washer"),
            "and it must not also be in the prefix"
        );
    }

    /// A hash over `.len()` alone would call a same-count swap unchanged.
    #[test]
    fn swapping_one_tool_for_another_moves_the_fast_hash() {
        let settings = Settings::default();

        let before = PromptState {
            available_tools: vec!["giap-home__set_light".into(), "giap-memory__recall".into()],
            ..default_state()
        };
        let swapped = PromptState {
            available_tools: vec![
                "giap-home__set_light".into(),
                "giap-weather__forecast".into(),
            ],
            ..default_state()
        };
        let reordered = PromptState {
            available_tools: vec!["giap-memory__recall".into(), "giap-home__set_light".into()],
            ..default_state()
        };

        let h = |s: &PromptState| compute_prefix_hash_fast(&settings, s, PROMPT_BALANCED);

        assert_ne!(
            h(&before),
            h(&swapped),
            "same count, different tools — this is the case that was wrong"
        );
        assert_ne!(
            h(&before),
            h(&reordered),
            "order is part of the render, so it is part of the identity"
        );
    }

    /// The device list is a tool's job (`giap-device__list_registered_devices`), not the prompt's.
    #[test]
    fn registering_a_device_no_longer_moves_the_prefix() {
        let settings = Settings::default();
        let state = PromptState { ..default_state() };

        let p1 = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);
        let p2 = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert_eq!(
            p1.prefix_hash, p2.prefix_hash,
            "device state is not prompt input any more"
        );
        assert!(
            !p1.static_prefix.contains("<home-devices>"),
            "the home-devices section is gone: {}",
            p1.static_prefix
        );
    }

    #[test]
    fn prefix_hash_changes_when_thinking_mode_changes() {
        let settings = Settings::default();

        let state1 = PromptState {
            thinking_enabled: false,
            ..default_state()
        };

        let state2 = PromptState {
            thinking_enabled: true,
            ..default_state()
        };

        let p1 = build_prompt_partition(&settings, None, &state1, PROMPT_BALANCED);
        let p2 = build_prompt_partition(&settings, None, &state2, PROMPT_BALANCED);

        assert_ne!(
            p1.prefix_hash, p2.prefix_hash,
            "Prefix hash must change when thinking mode changes"
        );
    }

    #[test]
    fn profile_context_goes_to_dynamic_suffix() {
        let settings = Settings::default();
        let state = default_state();
        let profile = ProfileContext {
            preferred_name: Some("Captain".to_string()),
            birthday: Some("1990-03-05".to_string()),
            language: Some("sw".to_string()),
            atypical_speech: false,
        };

        let partition = build_prompt_partition(&settings, Some(&profile), &state, PROMPT_BALANCED);

        assert!(
            partition.dynamic_suffix.contains("Captain"),
            "Profile preferred name must be in dynamic suffix"
        );
        assert!(
            partition.dynamic_suffix.contains("Swahili"),
            "Profile language must be in dynamic suffix"
        );
        assert!(
            partition.dynamic_suffix.contains("1990-03-05"),
            "Profile birthday must be in dynamic suffix"
        );
    }

    #[test]
    fn addendum_goes_to_dynamic_suffix() {
        let mut settings = Settings::default();
        settings.prompt_addendum = "Always respond in French.".to_string();
        let state = default_state();

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(
            partition
                .dynamic_suffix
                .contains("Always respond in French."),
            "Addendum must be in dynamic suffix"
        );
        assert!(
            !partition
                .static_prefix
                .contains("Always respond in French."),
            "Addendum must NOT be in static prefix"
        );
    }

    #[test]
    fn empty_date_produces_no_temporal_line() {
        let settings = Settings::default();
        let state = PromptState {
            current_date: String::new(),
            current_time: String::new(),
            ..default_state()
        };

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);
        assert!(
            !partition.dynamic_suffix.contains("Today is"),
            "No temporal line when date is empty"
        );
    }

    #[test]
    fn custom_system_prompt_used_in_prefix() {
        let mut settings = Settings::default();
        settings.assistant_name = "Pond".to_string();
        settings.custom_system_prompt =
            Some("I am {{assistant_name}} and I serve {{user_name}}.".to_string());
        let state = default_state();

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(partition.static_prefix.contains("Pond"));
        assert!(partition.static_prefix.contains("Friend"));
    }

    #[test]
    fn fast_hash_matches_full_partition_hash_for_same_input() {
        let settings = Settings::default();
        let state = default_state();

        let fast = compute_prefix_hash_fast(&settings, &state, PROMPT_BALANCED);

        // Field-level vs string-level hashes never match numerically; check determinism instead.
        let fast2 = compute_prefix_hash_fast(&settings, &state, PROMPT_BALANCED);
        assert_eq!(fast, fast2, "Fast hash must be deterministic");

        let mut settings2 = Settings::default();
        settings2.assistant_name = "Changed".to_string();
        let fast3 = compute_prefix_hash_fast(&settings2, &state, PROMPT_BALANCED);
        assert_ne!(fast, fast3, "Fast hash must change when settings change");
    }

    #[test]
    fn both_inputs_to_the_thinking_budget_move_the_prefix_and_the_fast_hash() {
        let state = PromptState {
            thinking_enabled: true,
            compact_prompt: false,
            ..default_state()
        };

        // 1. reasoning_effort.
        let brief = Settings {
            reasoning_effort: "brief".to_string(),
            ..Default::default()
        };
        let thorough = Settings {
            reasoning_effort: "thorough".to_string(),
            ..Default::default()
        };
        let p_brief = build_prompt_partition(&brief, None, &state, PROMPT_BALANCED);
        let p_thorough = build_prompt_partition(&thorough, None, &state, PROMPT_BALANCED);
        assert_ne!(
            p_brief.static_prefix, p_thorough.static_prefix,
            "reasoning_effort does not reach the static prefix at all"
        );
        assert_ne!(
            p_brief.prefix_hash, p_thorough.prefix_hash,
            "reasoning_effort changed the prefix without changing its hash"
        );
        assert_ne!(
            compute_prefix_hash_fast(&brief, &state, PROMPT_BALANCED),
            compute_prefix_hash_fast(&thorough, &state, PROMPT_BALANCED),
            "the fast hash misses reasoning_effort — it would report a changed prefix unchanged"
        );

        // 2. compact_prompt, which picks the tier the budget is drawn from.
        let compact_state = PromptState {
            compact_prompt: true,
            ..state.clone()
        };
        assert_ne!(
            compute_prefix_hash_fast(&brief, &state, PROMPT_BALANCED),
            compute_prefix_hash_fast(&brief, &compact_state, PROMPT_BALANCED),
            "the fast hash misses compact_prompt"
        );
    }

    #[test]
    fn combined_output_matches_full_prompt() {
        let settings = Settings::default();
        let state = default_state();

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        // Prefix + suffix must hold everything the full builder produces.
        let combined = if partition.dynamic_suffix.is_empty() {
            partition.static_prefix.clone()
        } else {
            format!(
                "{}\n\n{}",
                partition.static_prefix, partition.dynamic_suffix
            )
        };

        assert!(combined.contains("Goose"));
        assert!(combined.contains("Friend"));

        assert!(combined.contains("Thursday, 1 May 2026"));
    }

    /// By value, not pointer: `const` items inline per use site, so `std::ptr::eq` says unequal.
    #[test]
    fn resolve_builtin_template_selects_correct_style() {
        let mut s = Settings::default();

        for (style, expected) in [
            ("balanced", PROMPT_BALANCED),
            ("concise", PROMPT_CONCISE),
            ("technical", PROMPT_TECHNICAL),
            ("warm", PROMPT_WARM),
        ] {
            s.prompt_style = style.to_string();
            assert_eq!(
                resolve_builtin_template(&s),
                expected,
                "style '{style}' did not resolve to its own template"
            );
        }

        s.prompt_style = "nonexistent".to_string();
        assert_eq!(
            resolve_builtin_template(&s),
            PROMPT_BALANCED,
            "an unrecognised style must fall back to balanced"
        );
    }

    #[test]
    fn voice_mode_section_in_static_prefix() {
        let settings = Settings::default();

        let state = PromptState {
            voice_mode: true,
            ..default_state()
        };

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        assert!(
            partition.static_prefix.contains("<voice-mode>"),
            "Voice mode section must be in static prefix"
        );
    }

    /// `native_tools_json` comes from the provider, so it must stay independent of reasoning.
    #[test]
    fn a_model_that_does_not_reason_is_not_handed_the_tools_twice() {
        let settings = Settings::default();
        let tools = vec![
            "wikipedia — Look up factual info".to_string(),
            "weather — Current conditions".to_string(),
        ];

        let mut sizes = Vec::new();
        for thinking in [true, false] {
            let state = PromptState {
                available_tools: tools.clone(),
                native_tools_json: true,
                thinking_enabled: thinking,
                ..default_state()
            };
            let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);
            assert!(
                !partition.static_prefix.contains("wikipedia"),
                "native_tools_json must suppress the prose listing (thinking={thinking})"
            );
            sizes.push(partition.static_prefix.len());
        }

        // Thinking is a small section; a tool listing on one side would be a big gap.
        let gap = sizes[0].abs_diff(sizes[1]);
        assert!(
            gap < 2_000,
            "thinking should change the prefix by a section, not by a tool \
             listing: {} vs {} ({gap} chars apart)",
            sizes[0],
            sizes[1]
        );
    }

    #[test]
    fn tool_names_never_reach_the_prompt() {
        let settings = Settings::default();
        let state = PromptState {
            available_tools: vec!["wikipedia — Look up factual info".to_string()],
            ..default_state()
        };

        let partition = build_prompt_partition(&settings, None, &state, PROMPT_BALANCED);

        // Every shipped provider feeds tools via the chat template; naming them here duplicates.
        assert!(
            !partition.static_prefix.contains("wikipedia"),
            "no individual tool may be named in the prompt"
        );
        assert!(
            partition.static_prefix.contains("<tool-usage>"),
            "a non-empty tool list must still light up the tool-usage guidance"
        );
    }
}
