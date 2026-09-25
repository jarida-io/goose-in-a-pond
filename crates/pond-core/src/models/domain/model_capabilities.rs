use serde::{Deserialize, Serialize};

/// Runtime capabilities of the active model; defaults are conservative (false / 4096).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Reasoning: `<|channel>thought...<channel|>` (Gemma 4), `<think>...</think>` (Qwen3, R1).
    pub thinking: bool,

    /// Model accepts image content in messages (multimodal vision).
    pub vision: bool,

    /// Model accepts raw audio input (skip Whisper ASR).
    pub audio_input: bool,

    /// Maximum context window in tokens.
    pub context_window_tokens: u32,

    /// Supports constrained/structured output (GBNF grammar, JSON mode).
    pub structured_output: bool,

    /// Native tool calling: tools go via the chat template, else as system-prompt text.
    pub tool_calling: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            thinking: false,
            vision: false,
            audio_input: false,
            context_window_tokens: 4096,
            structured_output: false,
            tool_calling: false,
        }
    }
}

/// Substrings that mark a vision model outright, safe for `contains` (`llava` covers `bakllava`).
const VISION_NAME_FRAGMENTS: &[&str] = &[
    "llava",
    "moondream",
    "pixtral",
    "minicpm-v",
    "minicpm_v",
    "minicpm-o",
    "internvl",
    "cogvlm",
    "smolvlm",
    "idefics",
    "multimodal",
    // Ollama's unhyphenated tag splits into `qwen2` / `5vl`, so the `vl` segment rule misses it.
    "qwen2.5vl",
];

/// Vision-marking name segments (split on non-alphanumerics): `vl` is too common a substring.
const VISION_NAME_SEGMENTS: &[&str] = &["vision", "vl", "vlm"];

/// Gemma 4 spellings, incl. `gemma3n` (Ollama/HF's name for the same weights). Vision rule only:
/// widening the other axes would change thinking, tool-calling and context-window behaviour.
const GEMMA4_NAME_FRAGMENTS: &[&str] = &[
    "gemma-4", "gemma4", "gemma_4", "gemma-3n", "gemma3n", "gemma_3n",
];

impl ModelCapabilities {
    /// Whether a model name implies image input. Biased to `false`: a false positive makes a
    /// text-only model invent images. `E1B` is the one Gemma 4 without an mmproj.
    #[must_use]
    pub fn name_implies_vision(name: &str) -> bool {
        let lower = name.to_ascii_lowercase();

        if GEMMA4_NAME_FRAGMENTS.iter().any(|f| lower.contains(f)) {
            return !lower.contains("e1b");
        }

        VISION_NAME_FRAGMENTS.iter().any(|f| lower.contains(f))
            || lower
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|segment| VISION_NAME_SEGMENTS.contains(&segment))
    }

    /// Heuristic capabilities from known model-family names; adapters may know better.
    pub fn from_model_name(name: &str) -> Self {
        let lower = name.to_lowercase();
        let mut caps = Self::default();

        // Thinking-capable model families
        if lower.contains("gemma-4")
            || lower.contains("gemma4")
            || lower.contains("gemma_4")
            || lower.contains("qwen3")
            || lower.contains("qwq")
            || lower.contains("deepseek-r1")
            || lower.contains("deepseek_r1")
        {
            caps.thinking = true;
        }

        // Deliberately narrow: only this flag puts a claim about the model's senses in its prompt.
        caps.vision = Self::name_implies_vision(name);

        // Audio-capable (Gemma 4 E2B/E4B only)
        if (lower.contains("gemma-4") || lower.contains("gemma4") || lower.contains("gemma_4"))
            && (lower.contains("e2b") || lower.contains("e4b"))
        {
            caps.audio_input = true;
        }

        // Context window heuristics
        if lower.contains("gemma-4") || lower.contains("gemma4") || lower.contains("gemma_4") {
            // Gemma 4 E2B/E4B: 128K, 26B/31B: 256K
            if lower.contains("e2b") || lower.contains("e4b") {
                caps.context_window_tokens = 128_000;
            } else {
                caps.context_window_tokens = 128_000; // conservative for GGUF
            }
        } else if lower.contains("llama-3") || lower.contains("llama3") {
            caps.context_window_tokens = 8_192;
        } else if lower.contains("qwen") {
            caps.context_window_tokens = 32_768;
        } else if lower.contains("mistral") {
            caps.context_window_tokens = 32_768;
        }

        // Structured output — all local GGUF models support GBNF via llama.cpp
        if lower.contains(".gguf")
            || lower.contains("q4_k")
            || lower.contains("q5_k")
            || lower.contains("q8_0")
            || lower.contains("q6_k")
        {
            caps.structured_output = true;
        }

        // Native tool calling — Gemma 4 uses <|tool_call> format via Jinja template
        if lower.contains("gemma-4")
            || lower.contains("gemma4")
            || lower.contains("gemma_4")
            || lower.contains("qwen3")
            || lower.contains("mistral")
        {
            caps.tool_calling = true;
        }

        caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_conservative() {
        let caps = ModelCapabilities::default();
        assert!(!caps.thinking);
        assert!(!caps.vision);
        assert!(!caps.audio_input);
        assert_eq!(caps.context_window_tokens, 4096);
        assert!(!caps.structured_output);
        assert!(!caps.tool_calling);
    }

    #[test]
    fn detects_gemma4_capabilities() {
        let caps = ModelCapabilities::from_model_name("gemma-4-E2B-it-Q4_K_M.gguf");
        assert!(caps.thinking);
        assert!(caps.vision);
        assert!(caps.audio_input);
        assert_eq!(caps.context_window_tokens, 128_000);
        assert!(caps.structured_output);
    }

    #[test]
    fn detects_qwen3_thinking() {
        let caps = ModelCapabilities::from_model_name("qwen3-8b-instruct-q4_k_m");
        assert!(caps.thinking);
        assert!(!caps.vision);
        assert_eq!(caps.context_window_tokens, 32_768);
    }

    #[test]
    fn llama_defaults() {
        let caps = ModelCapabilities::from_model_name("llama3.2");
        assert!(!caps.thinking);
        assert!(!caps.vision);
        assert_eq!(caps.context_window_tokens, 8_192);
    }

    #[test]
    fn unknown_model_gets_safe_defaults() {
        let caps = ModelCapabilities::from_model_name("my-custom-model");
        assert!(!caps.thinking);
        assert!(!caps.vision);
        assert_eq!(caps.context_window_tokens, 4096);
    }

    // ── Vision detection ──────────────────────────────────────────────────

    /// E1B has no vision encoder (`mmproj: None`), so "gemma-4 means vision" is wrong.
    #[test]
    fn gemma4_e1b_has_no_vision_in_any_spelling() {
        for name in [
            "gemma-4-E1B-it",
            "gemma-4-E1B-it-Q4_K_M.gguf",
            "gemma4-e1b",
            "gemma3n:e1b",
        ] {
            assert!(
                !ModelCapabilities::from_model_name(name).vision,
                "{name} declares no mmproj encoder"
            );
        }
    }

    #[test]
    fn the_gemma4_variants_that_do_carry_an_encoder_are_recognised() {
        for name in [
            "gemma-4-E2B-it",
            "gemma-4-E4B-it-Q4_K_M",
            "gemma-4-12B-A4B-it",
            "gemma-4-27B-it",
            // The spelling Ollama serves the same weights under.
            "gemma3n:e4b",
            "gemma3n:e2b",
            "gemma-3n-E4B-it",
        ] {
            assert!(
                ModelCapabilities::from_model_name(name).vision,
                "{name} is multimodal"
            );
        }
    }

    #[test]
    fn common_http_vision_models_are_recognised() {
        for name in [
            "llama3.2-vision",
            "llama3.2-vision:11b",
            "llama-3.2-90b-vision-instruct",
            "qwen2.5-vl",
            "qwen2.5-vl:7b",
            // Ollama's real, unhyphenated tag.
            "qwen2.5vl",
            "qwen2.5vl:7b",
            "Qwen2-VL-7B-Instruct",
            "qwen3-vl:8b",
            "minicpm-v",
            "minicpm-v:8b",
            "pixtral-12b",
            "llava:13b",
            "bakllava",
            "moondream",
            "internvl2-8b",
            "phi-4-multimodal-instruct",
        ] {
            assert!(
                ModelCapabilities::from_model_name(name).vision,
                "{name} accepts images"
            );
        }
    }

    #[test]
    fn text_only_models_are_not_credited_with_vision() {
        for name in [
            "llama3.2",
            "llama3.2:3b",
            "Llama-3.2-3B-Instruct",
            "qwen3-8b-instruct-q4_k_m",
            "mistral-small-24b",
            "Hermes-2-Pro-Mistral-7B",
            "gpt-oss:20b",
            "my-custom-model",
            // "vl" inside a word is not a vision marker.
            "vlad-tuned-7b",
            "nvlink-test-model",
        ] {
            assert!(
                !ModelCapabilities::from_model_name(name).vision,
                "{name} has no image input"
            );
        }
    }

    #[test]
    fn serialization_roundtrip() {
        let caps = ModelCapabilities::from_model_name("gemma-4-E2B-it-Q4_K_M.gguf");
        let json = serde_json::to_string(&caps).unwrap();
        let caps2: ModelCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(caps.thinking, caps2.thinking);
        assert_eq!(caps.vision, caps2.vision);
        assert_eq!(caps.context_window_tokens, caps2.context_window_tokens);
    }
}

#[cfg(test)]
mod probe_gap_tests {
    use super::*;

    /// "auto" thinking reads the name, which hides that Nemotron reasons; `ModelProbe` sees it.
    #[test]
    fn the_name_heuristic_does_not_know_nemotron_reasons() {
        let caps = ModelCapabilities::from_model_name("NVIDIA-Nemotron3-Nano-4B-Q4_K_M");
        assert!(
            !caps.thinking,
            "if the name heuristic has learned Nemotron, this gap is closed and \
             thinking_mode=auto no longer needs the probe"
        );
    }
}
