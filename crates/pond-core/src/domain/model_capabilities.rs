use serde::{Deserialize, Serialize};

/// Runtime capabilities declared by an LLM provider.
///
/// Each adapter populates this based on the active model's known features.
/// Services and routes branch on these to enable model-specific behaviour
/// (thinking mode, vision input, larger context windows) while keeping the
/// core architecture model-agnostic.
///
/// All fields default to the most conservative assumption (false / 4096)
/// so that unknown models work safely out of the box.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Model supports internal chain-of-thought reasoning.
    /// Gemma 4: `<|channel>thought...<channel|>`
    /// Qwen3 / DeepSeek-R1: `<think>...</think>`
    pub thinking: bool,

    /// Model accepts image content in messages (multimodal vision).
    pub vision: bool,

    /// Model accepts raw audio input (skip Whisper ASR).
    pub audio_input: bool,

    /// Maximum context window in tokens.
    pub context_window_tokens: u32,

    /// Supports constrained/structured output (GBNF grammar, JSON mode).
    pub structured_output: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            thinking: false,
            vision: false,
            audio_input: false,
            context_window_tokens: 4096,
            structured_output: false,
        }
    }
}

impl ModelCapabilities {
    /// Detect capabilities from a model name string.
    ///
    /// This is a heuristic based on known model families. Adapters can
    /// override with more precise information from provider APIs.
    pub fn from_model_name(name: &str) -> Self {
        let lower = name.to_lowercase();
        let mut caps = Self::default();

        // Thinking-capable model families
        if lower.contains("gemma-4") || lower.contains("gemma4")
            || lower.contains("gemma_4")
            || lower.contains("qwen3") || lower.contains("qwq")
            || lower.contains("deepseek-r1") || lower.contains("deepseek_r1")
        {
            caps.thinking = true;
        }

        // Vision-capable model families
        if lower.contains("gemma-4") || lower.contains("gemma4")
            || lower.contains("gemma_4")
            || lower.contains("llava")
            || lower.contains("bakllava")
            || lower.contains("moondream")
        {
            caps.vision = true;
        }

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
        if lower.contains(".gguf") || lower.contains("q4_k") || lower.contains("q5_k")
            || lower.contains("q8_0") || lower.contains("q6_k")
        {
            caps.structured_output = true;
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
