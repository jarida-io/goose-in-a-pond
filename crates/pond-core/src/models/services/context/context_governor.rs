//! The single answer to a model's context-window size; must never read the environment.
//! Precedence: EngineReported, Registry (allocations), CatalogRecord, Override, Heuristic.

use crate::models::domain::model_capabilities::ModelCapabilities;

/// Upper bound for an unpinned local model; the engine narrows it by its memory estimate.
const UNPINNED_LOCAL_CEILING: usize = 32_768;

/// Prompt-side clamp for local providers. See [`ContextGovernor::prompt_window`].
const LOCAL_PROMPT_CLAMP: usize = 8_192;

/// Providers whose preamble is re-prefilled locally on every turn.
fn is_local_provider(provider: &str) -> bool {
    matches!(provider, "local" | "gguf")
}

/// Where a resolved window came from; the Models UI shows it to explain the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSource {
    /// The engine reported its actual allocation.
    EngineReported,
    /// A pinned `context_size` in the local model registry.
    Registry,
    /// `ModelRecord.context_length` from the model catalog.
    CatalogRecord,
    /// The user's `context_window_override`.
    Override,
    /// Derived from the model name.
    Heuristic,
}

impl WindowSource {
    /// A short label for logs and the Models UI.
    pub fn label(&self) -> &'static str {
        match self {
            WindowSource::EngineReported => "engine",
            WindowSource::Registry => "registry",
            WindowSource::CatalogRecord => "catalog",
            WindowSource::Override => "override",
            WindowSource::Heuristic => "heuristic",
        }
    }

    /// Whether this is a real allocation; an inexact window keeps `turn_trimmer`'s overshoot net.
    pub fn is_exact(&self) -> bool {
        matches!(self, WindowSource::EngineReported | WindowSource::Registry)
    }
}

/// Engine-reported window, tagged by model: after a swap the latest reading is the OLD model's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineWindow {
    pub tokens: u32,
    pub model: String,
}

impl EngineWindow {
    pub fn new(model: impl Into<String>, tokens: u32) -> Self {
        Self {
            tokens,
            model: model.into(),
        }
    }
}

/// Unknown fields are `None` and fall through; the name heuristic always yields a number.
#[derive(Debug, Clone, Default)]
pub struct ContextInputs<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    /// `Settings.context_window_override`. Zero means unset.
    pub override_tokens: u32,
    /// A pinned `context_size` from the local model registry, if reachable.
    pub registry_pinned: Option<usize>,
    /// `ModelRecord.context_length` for catalog models.
    pub catalog_context_length: Option<u32>,
    /// The engine's own report from a previous turn of this session.
    pub engine_reported: Option<EngineWindow>,
    /// Live capability-reported window; used at the heuristic rung in place of the name guess.
    pub capability_window: Option<u32>,
}

/// A resolved window and its provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowResolution {
    pub tokens: usize,
    pub source: WindowSource,
}

impl WindowResolution {
    pub fn is_exact(&self) -> bool {
        self.source.is_exact()
    }
}

pub struct ContextGovernor;

impl ContextGovernor {
    pub fn resolve(inputs: &ContextInputs<'_>) -> WindowResolution {
        // 1. The engine's own report, if it is for this model.
        if let Some(engine) = &inputs.engine_reported {
            if engine.tokens > 0 && engine.model == inputs.model {
                return WindowResolution {
                    tokens: engine.tokens as usize,
                    source: WindowSource::EngineReported,
                };
            }
        }

        // 2. A pinned registry size IS the allocation, so it outranks the override.
        if let Some(pinned) = inputs.registry_pinned.filter(|p| *p > 0) {
            return WindowResolution {
                tokens: pinned,
                source: WindowSource::Registry,
            };
        }

        // 3. Catalog: a DECLARED maximum, not an allocation, so it is clamped to the ceiling
        //    on-device and bounded by any LOWER override.
        if let Some(catalog) = inputs.catalog_context_length.filter(|c| *c > 0) {
            let mut tokens = catalog as usize;
            // Not `is_local_provider`, which misses ollama and llamafile (they run here too).
            if super::model_class::runs_on_this_device(inputs.provider) {
                tokens = tokens.min(UNPINNED_LOCAL_CEILING);
            }
            let override_tokens = inputs.override_tokens as usize;
            if override_tokens > 0 && override_tokens < tokens {
                return WindowResolution {
                    tokens: override_tokens,
                    source: WindowSource::Override,
                };
            }
            return WindowResolution {
                tokens,
                source: WindowSource::CatalogRecord,
            };
        }

        // 4. The user's escape hatch.
        if inputs.override_tokens > 0 {
            return WindowResolution {
                tokens: inputs.override_tokens as usize,
                source: WindowSource::Override,
            };
        }

        // 5. Heuristic; the only rung that may return the conservative 4096 default.
        let tokens = inputs
            .capability_window
            .filter(|c| *c > 0)
            .map(|c| c as usize)
            .unwrap_or_else(|| Self::heuristic_window(inputs.provider, inputs.model));
        WindowResolution {
            tokens,
            source: WindowSource::Heuristic,
        }
    }

    fn heuristic_window(provider: &str, model: &str) -> usize {
        if is_local_provider(provider) {
            UNPINNED_LOCAL_CEILING
        } else {
            ModelCapabilities::from_model_name(model).context_window_tokens as usize
        }
    }

    /// Window for PROMPT-side budgets; history uses the full one. Locally the preamble is
    /// re-prefilled every turn, so a bigger window must buy history room, not prompt.
    pub fn prompt_window(provider: &str, resolved: usize) -> usize {
        if is_local_provider(provider) {
            resolved.min(LOCAL_PROMPT_CLAMP)
        } else {
            resolved
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>(provider: &'a str, model: &'a str) -> ContextInputs<'a> {
        ContextInputs {
            provider,
            model,
            ..Default::default()
        }
    }

    #[test]
    fn engine_report_outranks_everything() {
        let mut i = inputs("local", "gemma-4-e2b");
        i.registry_pinned = Some(4096);
        i.catalog_context_length = Some(8192);
        i.override_tokens = 16384;
        i.engine_reported = Some(EngineWindow::new("gemma-4-e2b", 3072));

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 3072);
        assert_eq!(r.source, WindowSource::EngineReported);
    }

    #[test]
    fn engine_report_for_a_different_model_is_discarded() {
        let mut i = inputs("local", "qwen3-4b");
        i.registry_pinned = Some(4096);
        i.engine_reported = Some(EngineWindow::new("gemma-4-e2b", 3072));

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 4096);
        assert_eq!(r.source, WindowSource::Registry);
    }

    #[test]
    fn a_zero_engine_report_is_treated_as_absent() {
        let mut i = inputs("local", "gemma-4-e2b");
        i.registry_pinned = Some(4096);
        i.engine_reported = Some(EngineWindow::new("gemma-4-e2b", 0));

        assert_eq!(ContextGovernor::resolve(&i).source, WindowSource::Registry);
    }

    #[test]
    fn registry_outranks_the_user_override() {
        // Budgeting above the allocation only makes the engine truncate.
        let mut i = inputs("local", "gemma-4-e2b");
        i.registry_pinned = Some(4096);
        i.override_tokens = 32768;

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 4096);
        assert_eq!(r.source, WindowSource::Registry);
    }

    #[test]
    fn catalog_length_is_used_when_no_registry_entry_exists() {
        let mut i = inputs("ollama", "some-unknown-model");
        i.catalog_context_length = Some(16384);

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 16384);
        assert_eq!(r.source, WindowSource::CatalogRecord);
    }

    #[test]
    fn a_lower_override_bounds_the_catalog_maximum() {
        // Else a hand-tuned 8192 on Ollama becomes gemma4's declared 131072 and prompts truncate.
        let mut i = inputs("ollama", "gemma4:e2b");
        i.catalog_context_length = Some(131_072);
        i.override_tokens = 8_192;

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 8_192);
        assert_eq!(
            r.source,
            WindowSource::Override,
            "provenance must name the value that actually won"
        );

        // The override cannot WIDEN past the declared maximum.
        let mut wide = inputs("ollama", "gemma4:e2b");
        wide.catalog_context_length = Some(16_384);
        wide.override_tokens = 65_536;
        let w = ContextGovernor::resolve(&wide);
        assert_eq!(w.tokens, 16_384);
        assert_eq!(w.source, WindowSource::CatalogRecord);

        // Locally the override is compared with the ALREADY ceiling-clamped value.
        let mut local = inputs("local", "gemma-4-e2b");
        local.catalog_context_length = Some(131_072);
        local.override_tokens = 65_536;
        let l = ContextGovernor::resolve(&local);
        assert_eq!(l.tokens, UNPINNED_LOCAL_CEILING);
        assert_eq!(l.source, WindowSource::CatalogRecord);

        // And a registry pin still beats both, unchanged.
        let mut pinned = inputs("local", "gemma-4-e2b");
        pinned.catalog_context_length = Some(131_072);
        pinned.override_tokens = 8_192;
        pinned.registry_pinned = Some(4_096);
        let p = ContextGovernor::resolve(&pinned);
        assert_eq!(p.tokens, 4_096);
        assert_eq!(p.source, WindowSource::Registry);
    }

    #[test]
    fn a_catalog_length_cannot_widen_an_unpinned_local_window() {
        // Else the catalog hands the trimmer a 128K history budget on a 32K machine.
        let mut i = inputs("local", "gemma-4-e2b");
        i.catalog_context_length = Some(131_072);

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, UNPINNED_LOCAL_CEILING);
        assert_eq!(r.source, WindowSource::CatalogRecord);

        // gguf is the same class of provider.
        let mut g = inputs("gguf", "gemma-4-e2b");
        g.catalog_context_length = Some(131_072);
        assert_eq!(ContextGovernor::resolve(&g).tokens, UNPINNED_LOCAL_CEILING);

        // Below the ceiling it passes through untouched.
        let mut small = inputs("local", "gemma-2-2b-it");
        small.catalog_context_length = Some(8_192);
        assert_eq!(ContextGovernor::resolve(&small).tokens, 8_192);

        // Ollama serves over HTTP but runs on this box, so its declared window is clamped too.
        let mut ollama = inputs("ollama", "gemma4:e2b");
        ollama.catalog_context_length = Some(131_072);
        let o = ContextGovernor::resolve(&ollama);
        assert_eq!(
            o.tokens, UNPINNED_LOCAL_CEILING,
            "ollama runs on this device, so a declared maximum is still bounded"
        );
        assert_eq!(o.source, WindowSource::CatalogRecord);

        let mut llamafile = inputs("llamafile", "gemma4-e2b");
        llamafile.catalog_context_length = Some(131_072);
        assert_eq!(
            ContextGovernor::resolve(&llamafile).tokens,
            UNPINNED_LOCAL_CEILING
        );

        // A hosted provider: nothing here prefills it, so the declared window stands.
        let mut hosted = inputs("openai", "gpt-4o");
        hosted.catalog_context_length = Some(131_072);
        let h = ContextGovernor::resolve(&hosted);
        assert_eq!(h.tokens, 131_072);
        assert_eq!(h.source, WindowSource::CatalogRecord);
    }

    #[test]
    fn override_wins_when_nothing_authoritative_is_known() {
        let mut i = inputs("ollama", "some-unknown-model");
        i.override_tokens = 2048;

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 2048);
        assert_eq!(r.source, WindowSource::Override);
    }

    #[test]
    fn heuristic_is_the_last_resort() {
        let r = ContextGovernor::resolve(&inputs("ollama", "some-unknown-model"));
        assert_eq!(r.source, WindowSource::Heuristic);
        // The conservative default from ModelCapabilities.
        assert_eq!(r.tokens, 4096);
    }

    #[test]
    fn unpinned_local_gets_the_generous_ceiling_not_the_conservative_default() {
        let r = ContextGovernor::resolve(&inputs("local", "some-unknown-model"));
        assert_eq!(r.tokens, UNPINNED_LOCAL_CEILING);
        assert_eq!(r.source, WindowSource::Heuristic);
        assert_eq!(
            ContextGovernor::resolve(&inputs("gguf", "some-unknown-model")).tokens,
            UNPINNED_LOCAL_CEILING
        );
    }

    #[test]
    fn zero_valued_authoritative_inputs_fall_through() {
        let mut i = inputs("ollama", "some-unknown-model");
        i.registry_pinned = Some(0);
        i.catalog_context_length = Some(0);
        i.override_tokens = 5000;

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 5000);
        assert_eq!(r.source, WindowSource::Override);
    }

    #[test]
    fn a_capability_window_is_preferred_over_the_name_heuristic() {
        let mut i = inputs("ollama", "some-unknown-model");
        i.capability_window = Some(16384);

        let r = ContextGovernor::resolve(&i);
        // Still the heuristic rung: a declared window, not an allocation.
        assert_eq!(r.tokens, 16384);
        assert_eq!(r.source, WindowSource::Heuristic);
        assert!(!r.is_exact());
    }

    #[test]
    fn a_capability_window_still_loses_to_the_override() {
        let mut i = inputs("ollama", "some-unknown-model");
        i.capability_window = Some(16384);
        i.override_tokens = 2048;

        let r = ContextGovernor::resolve(&i);
        assert_eq!(r.tokens, 2048);
        assert_eq!(r.source, WindowSource::Override);
    }

    #[test]
    fn a_zero_capability_window_falls_back_to_the_name() {
        let mut i = inputs("ollama", "some-unknown-model");
        i.capability_window = Some(0);

        assert_eq!(ContextGovernor::resolve(&i).tokens, 4096);
    }

    #[test]
    fn prompt_window_clamps_local_providers_only() {
        assert_eq!(ContextGovernor::prompt_window("local", 32768), 8192);
        assert_eq!(ContextGovernor::prompt_window("gguf", 32768), 8192);
        // Already below the clamp: unchanged, so a 3K Jetson keeps 3K.
        assert_eq!(ContextGovernor::prompt_window("local", 3072), 3072);
        // HTTP providers keep the raw window.
        assert_eq!(ContextGovernor::prompt_window("ollama", 32768), 32768);
    }

    #[test]
    fn exactness_tracks_whether_the_source_is_an_allocation() {
        assert!(WindowSource::EngineReported.is_exact());
        assert!(WindowSource::Registry.is_exact());
        assert!(!WindowSource::CatalogRecord.is_exact());
        assert!(!WindowSource::Override.is_exact());
        assert!(!WindowSource::Heuristic.is_exact());
    }

    #[test]
    fn every_source_has_a_distinct_label() {
        let labels = [
            WindowSource::EngineReported.label(),
            WindowSource::Registry.label(),
            WindowSource::CatalogRecord.label(),
            WindowSource::Override.label(),
            WindowSource::Heuristic.label(),
        ];
        let mut sorted = labels.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len());
    }

    #[test]
    fn this_module_does_not_read_the_environment() {
        let src = include_str!("context_governor.rs");
        let body = src.split("mod tests").next().unwrap_or(src);
        assert!(
            !body.contains("env::var") && !body.contains("std::env"),
            "context_governor must not read process environment variables"
        );
    }
}
