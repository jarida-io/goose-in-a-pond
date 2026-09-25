//! Which compaction mechanisms a model can afford, by window size and [`runs_on_this_device`].
//! Only [`ModelClass::Large`] unlocks an LLM call; an unknown provider above 64K fails open.

use super::context_governor::WindowResolution;

/// Small tier ceiling (inclusive); matches where `CompactionProfile::use_compact_prompt` steps.
pub const SMALL_WINDOW_CEILING: usize = 12_288;

/// Large-tier floor (inclusive) for hosted windows: the design's ">= 64K", in tokens.
pub const LARGE_WINDOW_FLOOR: usize = 65_536;

/// Providers using THIS box's compute (ollama/llamafile are localhost HTTP); too wide is safe.
pub const ON_DEVICE_PROVIDERS: [&str; 5] = ["local", "gguf", "ollama", "llamafile", "mistralrs"];

/// Providers known to run elsewhere; not the complement of the on-device list. Omission is safe.
pub const HOSTED_PROVIDERS: [&str; 6] = [
    "anthropic",
    "openai",
    "openrouter",
    "google",
    "databricks",
    "snowflake",
];

/// Where a provider's inference runs; an unknown name supports neither claim, hence not a `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderLocality {
    /// In [`ON_DEVICE_PROVIDERS`]: a call competes with this pond's own inference.
    OnDevice,
    /// In [`HOSTED_PROVIDERS`]: a call to it is somebody else's hardware.
    Hosted,
    /// In neither list: every caller must take the narrower answer.
    Unknown,
}

impl ProviderLocality {
    /// A short label for logs and refusal messages.
    pub fn label(&self) -> &'static str {
        match self {
            ProviderLocality::OnDevice => "on-device",
            ProviderLocality::Hosted => "hosted",
            ProviderLocality::Unknown => "unknown",
        }
    }
}

/// Trimmed and case-folded: `chat_provider` is human-typed, and `"ollama "` must stay on-device.
pub fn provider_locality(provider: &str) -> ProviderLocality {
    let provider = provider.trim();
    if ON_DEVICE_PROVIDERS
        .iter()
        .any(|p| provider.eq_ignore_ascii_case(p))
    {
        return ProviderLocality::OnDevice;
    }
    if HOSTED_PROVIDERS
        .iter()
        .any(|p| provider.eq_ignore_ascii_case(p))
    {
        return ProviderLocality::Hosted;
    }
    ProviderLocality::Unknown
}

/// Its negation is NOT "runs elsewhere": to require that, test for [`ProviderLocality::Hosted`].
pub fn runs_on_this_device(provider: &str) -> bool {
    provider_locality(provider) == ProviderLocality::OnDevice
}

/// What a compaction path may do for a given [`ModelClass`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionStrategy {
    /// `super::turn_trimmer`; always on, being the only mechanism that cannot stall a turn.
    pub deterministic_trim: bool,
    /// `SessionSummaryService`: idle-only, cancelled by a new turn, never awaited by one.
    pub idle_rolling_summary: bool,
    /// LLM re-summarisation of a stale rolling summary; nothing implements it yet.
    pub llm_resummarisation: bool,
}

/// How expensive an extra model call is here; ordered, bigger variants strictly more permissive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelClass {
    /// Window at or below [`SMALL_WINDOW_CEILING`], even hosted: too little to re-summarise.
    Small,
    /// Between the boundaries, plus larger windows on this box (roomy window, scarce compute).
    Medium,
    /// At or above [`LARGE_WINDOW_FLOOR`] and served elsewhere; only this may call an LLM.
    Large,
}

impl ModelClass {
    /// A short label for logs, tests and the Models UI.
    pub fn label(&self) -> &'static str {
        match self {
            ModelClass::Small => "small",
            ModelClass::Medium => "medium",
            ModelClass::Large => "large",
        }
    }

    pub fn classify(provider: &str, resolved_window_tokens: usize) -> Self {
        if resolved_window_tokens <= SMALL_WINDOW_CEILING {
            return ModelClass::Small;
        }
        if resolved_window_tokens >= LARGE_WINDOW_FLOOR && !runs_on_this_device(provider) {
            return ModelClass::Large;
        }
        ModelClass::Medium
    }

    /// Preferred entry point: keeps the governor the single source of the window.
    pub fn from_resolution(provider: &str, resolution: &WindowResolution) -> Self {
        Self::classify(provider, resolution.tokens)
    }

    /// `idle_rolling_summary` stays on for Small (unlike the design table): it cannot stall a turn.
    pub fn strategy(&self) -> CompactionStrategy {
        CompactionStrategy {
            deterministic_trim: true,
            idle_rolling_summary: true,
            llm_resummarisation: matches!(self, ModelClass::Large),
        }
    }

    /// Whether compaction may call a model to reshape history (not the idle summary feeding it).
    pub fn permits_compaction_model_call(&self) -> bool {
        self.strategy().llm_resummarisation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::services::context::context_governor::{
        ContextGovernor, ContextInputs, EngineWindow, WindowSource,
    };

    #[test]
    fn the_tier_table_classifies_every_row_the_design_states() {
        let cases: &[(&str, usize, ModelClass)] = &[
            // "Small on-device (local/gguf, <= 12K window)".
            ("local", 3_072, ModelClass::Small),
            ("local", 4_096, ModelClass::Small),
            ("gguf", 8_192, ModelClass::Small),
            ("local", 12_288, ModelClass::Small),
            // "Medium (32K, Ollama/llamafile)".
            ("ollama", 32_768, ModelClass::Medium),
            ("llamafile", 32_768, ModelClass::Medium),
            // The Orin's pinned window, which no row of the table covers.
            ("local", 16_384, ModelClass::Medium),
            // "Large / HTTP (>= 64K)".
            ("openai", 128_000, ModelClass::Large),
            ("anthropic", 200_000, ModelClass::Large),
            ("google", 65_536, ModelClass::Large),
            // Undefined in the table: a SMALL hosted window takes the small strategy.
            ("openai", 8_192, ModelClass::Small),
            // Undefined in the table: a LARGE window served from this box.
            ("ollama", 131_072, ModelClass::Medium),
            ("llamafile", 131_072, ModelClass::Medium),
            ("local", 1_000_000, ModelClass::Medium),
        ];
        for &(provider, window, expected) in cases {
            assert_eq!(
                ModelClass::classify(provider, window),
                expected,
                "{provider} at {window} tokens classified as {}, expected {}",
                ModelClass::classify(provider, window).label(),
                expected.label()
            );
        }
    }

    /// On-device, that call would compete with the prefill of the turn the user awaits.
    #[test]
    fn the_on_device_tiers_never_permit_an_llm_in_the_compaction_path() {
        for class in [ModelClass::Small, ModelClass::Medium] {
            let strategy = class.strategy();
            assert!(
                !strategy.llm_resummarisation,
                "the {} tier selected a strategy that re-summarises with an LLM; \
                 on this tier that call competes with the next turn's prefill \
                 (PAI-4 invariant 3)",
                class.label()
            );
            assert!(
                !class.permits_compaction_model_call(),
                "the {} tier reports that a compaction model call is permitted",
                class.label()
            );
        }
        assert!(
            ModelClass::Large.strategy().llm_resummarisation,
            "the large tier is the whole reason the dispatch exists and it \
             selected no re-summarisation"
        );
        assert!(ModelClass::Large.permits_compaction_model_call());
    }

    #[test]
    fn every_class_keeps_the_deterministic_trimmer() {
        for class in [ModelClass::Small, ModelClass::Medium, ModelClass::Large] {
            assert!(
                class.strategy().deterministic_trim,
                "{} lost the deterministic trimmer",
                class.label()
            );
        }
    }

    #[test]
    fn p1_changes_nothing_for_the_small_and_medium_tiers() {
        assert_eq!(
            ModelClass::Small.strategy(),
            ModelClass::Medium.strategy(),
            "P1 was meant to preserve today's behaviour on both on-device tiers, \
             and they now select different strategies"
        );
        assert!(ModelClass::Small.strategy().idle_rolling_summary);
        assert!(ModelClass::Medium.strategy().idle_rolling_summary);
        // The large tier adds a mechanism; it never removes one.
        let medium = ModelClass::Medium.strategy();
        let large = ModelClass::Large.strategy();
        assert!(large.deterministic_trim && large.idle_rolling_summary);
        assert_ne!(medium, large);
    }

    #[test]
    fn an_on_device_provider_can_never_reach_the_large_tier() {
        for provider in ON_DEVICE_PROVIDERS {
            for window in (0..=1_000_000usize).step_by(1_021) {
                let class = ModelClass::classify(provider, window);
                assert!(
                    class < ModelClass::Large,
                    "{provider} at {window} tokens reached the {} tier",
                    class.label()
                );
            }
        }
    }

    #[test]
    fn provider_matching_ignores_case() {
        assert!(runs_on_this_device("Ollama"));
        assert!(runs_on_this_device("LLAMAFILE"));
        assert_eq!(
            ModelClass::classify("Ollama", 131_072),
            ModelClass::Medium,
            "a capitalised provider escaped the on-device deny-list"
        );
    }

    /// `mock` (in-process) and `lmstudio`/`llama_swap`/`omlx` (localhost) are all `Unknown`.
    #[test]
    fn a_provider_in_neither_list_supports_neither_claim() {
        for provider in ON_DEVICE_PROVIDERS {
            assert_eq!(
                provider_locality(provider),
                ProviderLocality::OnDevice,
                "{provider} is in ON_DEVICE_PROVIDERS"
            );
        }
        for provider in HOSTED_PROVIDERS {
            assert_eq!(
                provider_locality(provider),
                ProviderLocality::Hosted,
                "{provider} is in HOSTED_PROVIDERS"
            );
        }
        for provider in [
            "",
            "   ",
            "mock",
            "lmstudio",
            "llama_swap",
            "omlx",
            "pond-spark",
        ] {
            assert_eq!(
                provider_locality(provider),
                ProviderLocality::Unknown,
                "`{provider}` was claimed to run somewhere this pond has not been told about"
            );
            assert!(
                !runs_on_this_device(provider),
                "`{provider}` is not in the on-device list and must not be reported as if it were"
            );
        }
    }

    #[test]
    fn no_provider_is_in_both_lists() {
        for on_device in ON_DEVICE_PROVIDERS {
            assert!(
                !HOSTED_PROVIDERS
                    .iter()
                    .any(|hosted| hosted.eq_ignore_ascii_case(on_device)),
                "{on_device} is in both provider lists, so its locality depends on match order"
            );
        }
    }

    #[test]
    fn provider_matching_ignores_surrounding_whitespace() {
        assert!(runs_on_this_device(" ollama "));
        assert!(runs_on_this_device("\tGGUF\n"));
        assert_eq!(provider_locality("  anthropic  "), ProviderLocality::Hosted);
        assert_eq!(
            ModelClass::classify(" ollama ", 131_072),
            ModelClass::Medium,
            "a padded provider escaped the on-device deny-list"
        );
    }

    /// The last assertion pins the module doc's admitted fail-open.
    #[test]
    fn an_unknown_provider_narrows_below_the_large_floor() {
        assert_eq!(ModelClass::classify("", 4_096), ModelClass::Small);
        assert_eq!(ModelClass::classify("", 0), ModelClass::Small);
        assert_eq!(
            ModelClass::classify("some-new-thing", 32_768),
            ModelClass::Medium
        );
        assert_eq!(
            ModelClass::classify("some-new-thing", 65_536),
            ModelClass::Large
        );
    }

    #[test]
    fn the_boundaries_step_exactly_where_the_budget_curve_does() {
        assert_eq!(
            ModelClass::classify("openai", SMALL_WINDOW_CEILING),
            ModelClass::Small
        );
        assert_eq!(
            ModelClass::classify("openai", SMALL_WINDOW_CEILING + 1),
            ModelClass::Medium
        );
        assert_eq!(
            ModelClass::classify("openai", LARGE_WINDOW_FLOOR - 1),
            ModelClass::Medium
        );
        assert_eq!(
            ModelClass::classify("openai", LARGE_WINDOW_FLOOR),
            ModelClass::Large
        );
    }

    /// Raising `context_window_override` must never cost a hosted model its re-summarisation.
    #[test]
    fn the_class_is_non_decreasing_in_the_window() {
        for provider in ["local", "ollama", "openai", "unknown"] {
            let mut prev = ModelClass::classify(provider, 0);
            for window in (0..=300_000usize).step_by(311) {
                let class = ModelClass::classify(provider, window);
                assert!(
                    class >= prev,
                    "{provider}: the window grew to {window} and the class fell \
                     from {} to {}",
                    prev.label(),
                    class.label()
                );
                prev = class;
            }
        }
    }

    /// The classes aren't derived from the budget curve, so pin that they agree.
    #[test]
    fn the_small_tier_is_exactly_the_compact_prompt_tier() {
        use crate::models::services::context::context_budget::CompactionProfile;
        for window in [0usize, 3_072, 4_096, 8_192, 12_288, 12_289, 32_768, 128_000] {
            let profile = CompactionProfile::from_context_window(window);
            assert_eq!(
                ModelClass::classify("local", window) == ModelClass::Small,
                profile.use_compact_prompt(),
                "window {window}: the small tier and the compact-prompt tier \
                 disagree, so there are two boundaries where there should be one"
            );
        }
    }

    // -- composed with the real governor, not with a hand-written number -------

    #[test]
    fn a_registry_pinned_orin_resolves_and_classifies_as_medium() {
        let inputs = ContextInputs {
            provider: "local",
            model: "gemma-4-e2b",
            registry_pinned: Some(16_384),
            ..Default::default()
        };
        let resolution = ContextGovernor::resolve(&inputs);
        assert_eq!(resolution.source, WindowSource::Registry);
        assert_eq!(
            ModelClass::from_resolution(inputs.provider, &resolution),
            ModelClass::Medium
        );
    }

    #[test]
    fn an_engine_reported_3k_jetson_classifies_as_small() {
        let inputs = ContextInputs {
            provider: "local",
            model: "gemma-4-e2b",
            engine_reported: Some(EngineWindow::new("gemma-4-e2b", 3_072)),
            ..Default::default()
        };
        let resolution = ContextGovernor::resolve(&inputs);
        assert_eq!(resolution.source, WindowSource::EngineReported);
        assert_eq!(
            ModelClass::from_resolution(inputs.provider, &resolution),
            ModelClass::Small
        );

        // Without the report the same pond takes the heuristic ceiling and is Medium.
        let bare = ContextInputs {
            provider: "local",
            model: "gemma-4-e2b",
            ..Default::default()
        };
        assert_eq!(
            ModelClass::from_resolution(bare.provider, &ContextGovernor::resolve(&bare)),
            ModelClass::Medium
        );
    }

    /// Uses rung 1 (never clamped): through rung 3 it would land in Medium on width alone.
    #[test]
    fn a_large_window_on_an_on_device_provider_stays_out_of_the_large_tier() {
        let inputs = ContextInputs {
            provider: "ollama",
            model: "gemma4:e2b",
            engine_reported: Some(EngineWindow::new("gemma4:e2b", 131_072)),
            ..Default::default()
        };
        let resolution = ContextGovernor::resolve(&inputs);
        assert_eq!(resolution.tokens, 131_072, "an allocation is not clamped");
        assert_eq!(resolution.source, WindowSource::EngineReported);
        assert_eq!(
            ModelClass::from_resolution(inputs.provider, &resolution),
            ModelClass::Medium,
            "an Ollama model on this box was handed the tier that spends a model \
             call on compaction"
        );
    }

    #[test]
    fn an_ollama_catalog_maximum_is_bounded_by_the_governor() {
        let inputs = ContextInputs {
            provider: "ollama",
            model: "gemma4:e2b",
            catalog_context_length: Some(131_072),
            ..Default::default()
        };
        let resolution = ContextGovernor::resolve(&inputs);
        assert_eq!(
            resolution.tokens, 32_768,
            "a declared maximum is not an allocation, whoever declared it"
        );
        assert_eq!(resolution.source, WindowSource::CatalogRecord);
    }

    /// Proves the tests above don't pass merely because Large is unreachable via the governor.
    #[test]
    fn a_hosted_128k_model_reaches_the_large_tier_through_the_governor() {
        let inputs = ContextInputs {
            provider: "anthropic",
            model: "claude-sonnet",
            catalog_context_length: Some(200_000),
            ..Default::default()
        };
        let resolution = ContextGovernor::resolve(&inputs);
        assert_eq!(
            ModelClass::from_resolution(inputs.provider, &resolution),
            ModelClass::Large
        );
    }

    #[test]
    fn every_class_has_a_distinct_label() {
        let labels = [
            ModelClass::Small.label(),
            ModelClass::Medium.label(),
            ModelClass::Large.label(),
        ];
        let mut sorted = labels.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len());
    }
}
