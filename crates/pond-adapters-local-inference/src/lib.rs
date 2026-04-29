//! In-process GGUF inference adapter and memory-aware model scheduler.
//!
//! Wraps Goose's [`LocalInferenceProvider`] so GIAP can load model weights
//! directly into the process — no llamafile/Ollama subprocess required.
//!
//! # Hardware acceleration
//! - **macOS**: Metal activated automatically via `llama-cpp-2` cfg flags.
//! - **Jetson Orin Nano (NVIDIA)**: Requires `--features cuda` at build time.
//!   CUDA settings are applied to the model registry at init time:
//!   - `n_gpu_layers = 99` — full offload into unified 8 GB DRAM (no separate VRAM)
//!   - `context_size = 3072` — safe headroom for GIAP's chat workflow on 8 GB
//!   - `n_batch = 512` — maximise GPU throughput on Ampere (sm_87)
//!   - `n_threads = 4` — 6-core A78AE; leave headroom for OS + voice pipeline
//!   - `flash_attention = true` — reduces KV-cache memory by ~40 % on Ampere
//!   - `use_mlock = false` — unified memory; mlock causes kernel page faults
//!
//! # Usage
//! ```no_run
//! # async fn example() -> anyhow::Result<()> {
//! use pond_adapters_local_inference::LocalInferenceLlmAdapter;
//! use std::sync::Arc;
//!
//! let llm = Arc::new(LocalInferenceLlmAdapter::new(
//!     LocalInferenceLlmAdapter::DEFAULT_MODEL,
//! ).await?);
//! # Ok(())
//! # }
//! ```

pub mod scheduler;
pub mod tool_caller;
pub use scheduler::{NoopScheduler, ResourceAwareModelScheduler, LLM_BUDGET_MB, JETSON_TOTAL_RAM_MB};
pub use tool_caller::ToolCallerEngine;

use anyhow::Result;
use async_trait::async_trait;
use goose::model::ModelConfig;
use goose::providers::base::Provider as GooseProvider;
use goose::providers::local_inference::LocalInferenceProvider;
use pond_adapters_goose::provider_adapter::GooseProviderAdapter;
use pond_core::domain::message::ChatMessage;
use pond_core::ports::provider::LlmProvider;
use std::sync::Arc;

/// Default GGUF model for Jetson Orin Nano (8 GB).
///
/// 3B Q4_K_M ≈ 2.0 GB on disk + ~2.5 GB at runtime — leaves ample headroom
/// for the OS, voice pipeline, and other GIAP services.
pub const DEFAULT_MODEL: &str = "bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M";

/// GIAP `LlmProvider` adapter backed by in-process GGUF inference.
///
/// Internally delegates to [`GooseProviderAdapter`] for `ChatMessage` ↔
/// Goose [`Message`] conversion so there is no duplication of that logic.
pub struct LocalInferenceLlmAdapter {
    inner: GooseProviderAdapter,
}

impl LocalInferenceLlmAdapter {
    /// Default GGUF model identifier (re-exported as an associated constant for
    /// ergonomic use via `LocalInferenceLlmAdapter::DEFAULT_MODEL`).
    pub const DEFAULT_MODEL: &'static str = DEFAULT_MODEL;

    /// Build the adapter for the given model identifier.
    ///
    /// The `model_id` can be a HuggingFace repo+filename such as
    /// `"bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M"`, a local file path,
    /// or any identifier accepted by Goose's `LocalInferenceProvider`.
    ///
    /// Model weights are **not** loaded here — they load on the first
    /// `complete()` call via `InferenceRuntime::get_or_init()` (global
    /// singleton, thread-safe `StdMutex<Weak<>>`).
    pub async fn new(model_id: &str) -> Result<Self> {
        let model_config = ModelConfig {
            model_name: model_id.to_string(),
            ..Default::default()
        };

        // On Jetson Orin Nano (CUDA build) apply hardware-specific settings to
        // the model registry entry so llama-cpp-2 picks them up at load time.
        // Jetson has unified 8 GB DRAM (CPU + GPU share the same pool), an
        // Ampere GPU (sm_87) with 1024 CUDA cores, and CUDA 12.6 on JetPack 6.2.
        #[cfg(feature = "cuda")]
        Self::apply_jetson_settings(model_id);
        #[cfg(not(feature = "cuda"))]
        Self::apply_platform_settings(model_id);

        tracing::info!("initialising LocalInferenceProvider for model: {}", model_id);
        let provider = LocalInferenceProvider::from_env(model_config, vec![]).await?;
        let session_id = uuid::Uuid::new_v4().to_string();

        Ok(Self {
            inner: GooseProviderAdapter::new(Arc::new(provider) as Arc<dyn GooseProvider>, session_id),
        })
    }

    /// Build the adapter, registering the model path in GIAP's data directory.
    ///
    /// Unlike `new()`, this method registers the model's `local_path` in Goose's
    /// global model registry so that `LocalInferenceProvider` can find the GGUF
    /// file at `$data_dir/models/gguf/{filename}` instead of Goose's default
    /// `~/.local/share/goose/models/` location.
    ///
    /// Accepts two formats:
    /// - HuggingFace: `"bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M"`
    /// - Raw filename: `"gemma-4-E2B-it-Q4_K_M.gguf"` (file must exist in `$data_dir/models/gguf/`)
    pub async fn new_with_data_dir(model_id: &str, data_dir: &std::path::Path) -> Result<Self> {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, LocalModelEntry, ModelSettings, model_id_from_repo,
        };

        let gguf_dir = data_dir.join("models").join("gguf");

        // ── Filename stem (e.g. "gemma-4-E2B-it-Q4_K_M") ───────────────────
        // Detected when: no '/', no ':', no ".gguf" extension.
        // The model catalog stores name = stem (without extension); the file on
        // disk is {stem}.gguf in the gguf directory.  Normalise by appending
        // ".gguf" and falling through to the raw filename path below.
        let owned_with_ext;
        let model_id = if !model_id.contains('/') && !model_id.contains(':') && !model_id.ends_with(".gguf") {
            let candidate = gguf_dir.join(format!("{}.gguf", model_id));
            if candidate.exists() {
                owned_with_ext = format!("{}.gguf", model_id);
                owned_with_ext.as_str()
            } else {
                model_id // not a local stem — fall through to HF path
            }
        } else {
            model_id
        };

        // ── Raw filename (e.g. "gemma-4-E2B-it-Q4_K_M.gguf") ────────────────
        // Detected when: no ':' separator and ends with ".gguf".
        if model_id.ends_with(".gguf") && !model_id.contains(':') {
            let path = std::path::Path::new(model_id);
            let filename = path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| model_id.to_string());

            // Stable synthetic registry key = stem (strip ".gguf").
            let stem = filename.trim_end_matches(".gguf").to_string();

            // Use absolute path if provided, otherwise place under data_dir.
            let local_path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                gguf_dir.join(&filename)
            };

            {
                match get_registry().lock() {
                    Ok(mut registry) => {
                        if !registry.has_model(&stem) {
                            let mut settings = ModelSettings::default();
                            settings.native_tool_calling = true;
                            let entry = LocalModelEntry {
                                id:           stem.clone(),
                                repo_id:      format!("local/{}", stem),
                                filename:     filename.clone(),
                                quantization: String::new(),
                                local_path,
                                source_url:   String::new(),
                                settings,
                                size_bytes:   0,
                            };
                            if let Err(e) = registry.add_model(entry) {
                                tracing::warn!("Could not register GGUF model '{}': {}", stem, e);
                            }
                        } else if let Some(entry) = registry.get_model(&stem) {
                            let mut s = entry.settings.clone();
                            if !s.native_tool_calling {
                                s.native_tool_calling = true;
                                let _ = registry.update_model_settings(&stem, s);
                            }
                        }
                    }
                    Err(e) => tracing::warn!("GGUF registry lock poisoned: {}", e),
                }
            }

            return Self::new(&stem).await;
        }

        // ── HuggingFace format ("repo_id:quantization") ───────────────────────
        // Parse "repo_id:quantization" — e.g. "bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M"
        let (repo_id, quantization) = model_id
            .rsplit_once(':')
            .unwrap_or((model_id, "Q4_K_M"));

        let id = model_id_from_repo(repo_id, quantization);

        // Derive filename: strip "-GGUF" suffix from the repo name, append "-{quant}.gguf"
        let model_name = repo_id.split('/').last().unwrap_or(repo_id);
        let base_name  = model_name.strip_suffix("-GGUF").unwrap_or(model_name);
        let filename   = format!("{}-{}.gguf", base_name, quantization);

        let local_path = gguf_dir.join(&filename);
        let source_url = format!(
            "https://huggingface.co/{}/resolve/main/{}",
            repo_id, filename
        );

        // Register / update local_path in Goose's global registry.
        // The lock is dropped before calling Self::new() to avoid deadlock.
        {
            match get_registry().lock() {
                Ok(mut registry) => {
                    if !registry.has_model(&id) {
                        let mut settings = ModelSettings::default();
                        settings.native_tool_calling = true;
                        let entry = LocalModelEntry {
                            id:           id.clone(),
                            repo_id:      repo_id.to_string(),
                            filename:     filename.clone(),
                            quantization: quantization.to_string(),
                            local_path,
                            source_url,
                            settings,
                            size_bytes:   0,
                        };
                        if let Err(e) = registry.add_model(entry) {
                            tracing::warn!("Could not register GGUF model '{}': {}", id, e);
                        }
                    } else if let Some(entry) = registry.get_model(&id) {
                        let mut s = entry.settings.clone();
                        if !s.native_tool_calling {
                            s.native_tool_calling = true;
                            let _ = registry.update_model_settings(&id, s);
                        }
                    }
                }
                Err(e) => tracing::warn!("GGUF registry lock poisoned: {}", e),
            }
        }

        Self::new(model_id).await
    }

    /// Apply platform-optimised model settings for non-CUDA builds (macOS Metal, CPU).
    ///
    /// On Apple Silicon (M1-M4), enables full Metal GPU offload, flash attention,
    /// and sets a reasonable 8K context window. Without this, ALL inference runs
    /// on CPU despite Metal being available — `n_gpu_layers` defaults to `None`.
    #[cfg(not(feature = "cuda"))]
    fn apply_platform_settings(model_id: &str) {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, ModelSettings,
        };

        let settings = ModelSettings {
            // Full GPU offload — Apple Silicon has unified memory so all layers
            // fit without any CPU/GPU split.
            n_gpu_layers: Some(99),
            // 8K context balances memory usage and conversation depth.
            // Fits ~6K tokens of history + system prompt + 2K generation headroom.
            // On M4 with 18GB this uses ~322MB KV cache for E4B — very comfortable.
            context_size: Some(8192),
            // Batch 512 is optimal for Metal prefill throughput.
            n_batch: Some(512),
            // Flash attention reduces KV-cache memory by ~40%.
            flash_attention: Some(true),
            // Unified memory — mlock is unnecessary and can cause issues.
            use_mlock: false,
            // Let llama.cpp auto-detect thread count (good on Apple Silicon).
            ..Default::default()
        };

        match get_registry().lock() {
            Ok(mut registry) => {
                if let Err(e) = registry.update_model_settings(model_id, settings) {
                    tracing::debug!(
                        "Platform settings not applied to '{}' (model not yet registered): {}",
                        model_id, e
                    );
                } else {
                    tracing::info!(
                        "Applied Metal/platform settings to model '{}' (n_gpu_layers=99, ctx=8192, flash_attn=true)",
                        model_id
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Could not acquire model registry lock for platform settings: {}", e);
            }
        }
    }

    /// Patch the Goose model registry with Jetson Orin Nano–optimised settings.
    ///
    /// These settings are applied at startup and saved to `~/.local/share/goose/
    /// models/registry.json` so they persist across restarts on Jetson.
    ///
    /// The function silently ignores errors (model not yet downloaded, registry
    /// lock poisoned) — defaults will be used in that case.
    #[cfg(feature = "cuda")]
    fn apply_jetson_settings(model_id: &str) {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, ModelSettings,
        };

        let jetson_settings = ModelSettings {
            // Full GPU offload: Jetson unified memory means all layers fit in
            // the same 8 GB pool — no split between CPU and GPU DRAM.
            n_gpu_layers: Some(99),
            // 3072-token context fits the GIAP chat workflow with room for the
            // system prompt + history, while keeping KV-cache pressure manageable.
            context_size: Some(3072),
            // Batch size 512 keeps Ampere SMs saturated during prefill without
            // exceeding the available memory bandwidth (68 GB/s).
            n_batch: Some(512),
            // Use 4 CPU threads for tokenisation / sampling on the 6-core A78AE.
            // Leaving 2 cores free for the OS, audio pipeline, and GIAP services.
            n_threads: Some(4),
            // Flash attention halves KV-cache memory on Ampere (native support).
            flash_attention: Some(true),
            // mlock pins pages in RAM; on unified memory this triggers kernel
            // page faults for every GPU access. Disable for correct performance.
            use_mlock: false,
            ..Default::default()
        };

        match get_registry().lock() {
            Ok(mut registry) => {
                if let Err(e) = registry.update_model_settings(model_id, jetson_settings) {
                    tracing::debug!(
                        "Jetson settings not applied to '{}' (model not yet registered): {}",
                        model_id, e
                    );
                } else {
                    tracing::info!(
                        "Applied Jetson Orin Nano CUDA settings to model '{}'",
                        model_id
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Could not acquire model registry lock for Jetson settings: {}", e);
            }
        }
    }
}

/// Strip thinking-token preambles emitted by reasoning-capable models.
///
/// Handles two formats:
///
/// 1. **Gemma 4**: `<|channel>thought … <channel|>ACTUAL REPLY`
///    Everything after the last `<channel|>` is the real response.
///
/// 2. **Qwen3 / DeepSeek-R1 / QwQ**: `<think>…</think>ACTUAL REPLY`
///    Everything inside `<think>…</think>` tags is stripped.
///
/// If neither pattern is present the original text is returned unchanged.
fn strip_thinking_tokens(text: &str) -> String {
    // Gemma 4 format
    const CHANNEL_CLOSE: &str = "<channel|>";
    if let Some(pos) = text.rfind(CHANNEL_CLOSE) {
        return text[pos + CHANNEL_CLOSE.len()..].trim().to_string();
    }

    // <think>…</think> format — strip all blocks
    if text.contains("<think>") {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        loop {
            if let Some(start) = rest.find("<think>") {
                out.push_str(&rest[..start]);
                if let Some(end) = rest[start..].find("</think>") {
                    rest = &rest[start + end + "</think>".len()..];
                } else {
                    // Unclosed <think> — discard the rest
                    break;
                }
            } else {
                out.push_str(rest);
                break;
            }
        }
        let trimmed = out.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }

    text.to_string()
}

#[async_trait]
impl LlmProvider for LocalInferenceLlmAdapter {
    fn capabilities(&self) -> pond_core::domain::model_capabilities::ModelCapabilities {
        let name = self.inner.model_name();
        pond_core::domain::model_capabilities::ModelCapabilities::from_model_name(&name)
    }

    async fn complete(
        &self,
        system: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        let mut msg = self.inner.complete(system, messages).await?;
        msg.content = strip_thinking_tokens(&msg.content);
        Ok(msg)
    }

    fn model_name(&self) -> String {
        self.inner.model_name()
    }
}

// ── Unit tests (no model weights required) ────────────────────────────────────

#[cfg(test)]
mod tests {
    /// Integration tests that require a real model are marked `#[ignore]` and
    /// gated on the `GIAP_TEST_MODEL_PATH` environment variable.
    ///
    /// Run with:
    /// ```bash
    /// GIAP_TEST_MODEL_PATH=/path/to/model.gguf \
    ///   cargo test -p pond-adapters-local-inference -- --ignored
    /// ```
    use super::*;

    #[test]
    fn default_model_constant_is_set() {
        assert!(!DEFAULT_MODEL.is_empty());
    }

    #[test]
    fn default_model_has_huggingface_format() {
        // Expected: "org/repo-GGUF:QUANTIZATION"
        assert!(
            DEFAULT_MODEL.contains('/'),
            "DEFAULT_MODEL should be a HuggingFace repo path: {DEFAULT_MODEL}"
        );
        assert!(
            DEFAULT_MODEL.contains(':'),
            "DEFAULT_MODEL should have a quantization suffix (':'): {DEFAULT_MODEL}"
        );
    }

    #[test]
    fn associated_constant_matches_module_constant() {
        assert_eq!(
            LocalInferenceLlmAdapter::DEFAULT_MODEL,
            DEFAULT_MODEL,
            "associated constant must re-export the same value"
        );
    }

    // ── data_dir path-construction logic (no model weights required) ──────────

    /// Exercise the filename-derivation logic inside `new_with_data_dir` without
    /// touching the filesystem or loading a model.  We cannot call
    /// `new_with_data_dir` directly (it eventually calls `LocalInferenceProvider::
    /// from_env` which tries to download weights), so we replicate the pure
    /// filename logic here and assert the expected result.
    #[test]
    fn data_dir_filename_derivation_strips_gguf_suffix() {
        let model_id = "bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M";
        let (repo_id, quantization) = model_id.rsplit_once(':').unwrap();
        let model_name = repo_id.split('/').last().unwrap();
        let base_name  = model_name.strip_suffix("-GGUF").unwrap_or(model_name);
        let filename   = format!("{}-{}.gguf", base_name, quantization);

        assert_eq!(filename, "Llama-3.2-3B-Instruct-Q4_K_M.gguf");
    }

    #[test]
    fn data_dir_filename_without_gguf_suffix_kept_as_is() {
        let model_id = "bartowski/SomeModel:Q8_0";
        let (repo_id, quantization) = model_id.rsplit_once(':').unwrap();
        let model_name = repo_id.split('/').last().unwrap();
        let base_name  = model_name.strip_suffix("-GGUF").unwrap_or(model_name);
        let filename   = format!("{}-{}.gguf", base_name, quantization);

        assert_eq!(filename, "SomeModel-Q8_0.gguf");
    }

    #[test]
    fn data_dir_gguf_path_is_under_models_gguf() {
        let data_dir = std::path::Path::new("/home/user/.giap");
        let filename = "Llama-3.2-3B-Instruct-Q4_K_M.gguf";
        let gguf_dir = data_dir.join("models").join("gguf");
        let local_path = gguf_dir.join(filename);

        assert_eq!(
            local_path.to_string_lossy(),
            "/home/user/.giap/models/gguf/Llama-3.2-3B-Instruct-Q4_K_M.gguf"
        );
    }

    #[test]
    fn data_dir_source_url_is_huggingface_resolve() {
        let repo_id = "bartowski/Llama-3.2-3B-Instruct-GGUF";
        let filename = "Llama-3.2-3B-Instruct-Q4_K_M.gguf";
        let source_url = format!(
            "https://huggingface.co/{}/resolve/main/{}",
            repo_id, filename
        );

        assert!(source_url.starts_with("https://huggingface.co/"));
        assert!(source_url.contains("/resolve/main/"));
        assert!(source_url.ends_with(filename));
    }

    #[test]
    fn strip_thinking_tokens_removes_gemma4_preamble() {
        // Actual Gemma 4 format: opening = <|channel>thought, closing = <channel|>
        let raw = "<|channel>thought Some reasoning here.<channel|>Hello! I am Goose.";
        assert_eq!(strip_thinking_tokens(raw), "Hello! I am Goose.");
    }

    #[test]
    fn strip_thinking_tokens_no_tag_returns_original() {
        let raw = "Hello! I am Goose.";
        assert_eq!(strip_thinking_tokens(raw), "Hello! I am Goose.");
    }

    #[test]
    fn strip_thinking_tokens_multiline_thinking() {
        let raw = "<|channel>thought\nStep 1.\nStep 2.\n<channel|>The answer is 4.";
        assert_eq!(strip_thinking_tokens(raw), "The answer is 4.");
    }

    #[test]
    fn strip_thinking_tokens_uses_last_close_tag() {
        // If multiple <channel|> appear, we take everything after the last one
        let raw = "<|channel>thought Step 1.<channel|>intermediate<channel|>Final answer.";
        assert_eq!(strip_thinking_tokens(raw), "Final answer.");
    }

    #[test]
    fn model_id_rsplit_fallback_uses_q4_k_m() {
        // When no ':' quantization suffix is present, rsplit_once returns None
        // and the fallback "Q4_K_M" is used.
        let model_id = "some-model-without-quant";
        let (_repo_id, quantization) = model_id
            .rsplit_once(':')
            .unwrap_or((model_id, "Q4_K_M"));
        assert_eq!(quantization, "Q4_K_M");
    }
}
