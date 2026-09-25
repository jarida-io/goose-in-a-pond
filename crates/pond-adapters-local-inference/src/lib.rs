//! In-process GGUF inference via Goose's [`LocalInferenceProvider`], plus the memory-aware model
//! scheduler. Metal is automatic on macOS; the Jetson needs `--features cuda`.

/// Whether this build has CUDA; `cuda` is this crate's feature, invisible to `cfg!` elsewhere.
pub const CUDA_ENABLED: bool = cfg!(feature = "cuda");

pub mod scheduler;
pub mod tool_caller;
pub use scheduler::{
    NoopScheduler, ResourceAwareModelScheduler, JETSON_TOTAL_RAM_MB, LLM_BUDGET_MB,
};
pub use tool_caller::ToolCallerEngine;

use anyhow::Result;
use async_trait::async_trait;
use goose::providers::base::Provider as GooseProvider;
use goose::providers::local_inference::LocalInferenceProvider;
use goose_providers::model::ModelConfig;
use pond_adapters_goose::provider_adapter::GooseProviderAdapter;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use std::sync::Arc;

/// Default GGUF: a 3B Q4_K_M (~2.5 GB at runtime) leaves headroom on an 8 GB Orin Nano.
pub const DEFAULT_MODEL: &str = "bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M";

/// `LlmProvider` over in-process GGUF; [`GooseProviderAdapter`] does the message conversion.
pub struct LocalInferenceLlmAdapter {
    inner: GooseProviderAdapter,
}

impl LocalInferenceLlmAdapter {
    pub const DEFAULT_MODEL: &'static str = DEFAULT_MODEL;

    /// `model_id`: HuggingFace `repo:QUANT` or a local path. Weights load on first `complete()`.
    pub async fn new(model_id: &str) -> Result<Self> {
        let model_config = ModelConfig::new(model_id);

        Self::apply_model_settings(model_id);

        tracing::info!(
            "initialising LocalInferenceProvider for model: {}",
            model_id
        );
        goose::providers::local_inference::configure_local_inference();
        let provider = LocalInferenceProvider::from_env().await?;

        Ok(Self {
            inner: GooseProviderAdapter::new(
                Arc::new(provider) as Arc<dyn GooseProvider>,
                model_config,
            ),
        })
    }

    /// Like `new`, but points Goose's registry at `$data_dir/models/gguf/` instead of Goose's own
    /// models dir. `model_id` is a `repo:QUANT` id or a `.gguf` file already in that dir.
    pub async fn new_with_data_dir(model_id: &str, data_dir: &std::path::Path) -> Result<Self> {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, model_id_from_repo, LocalModelEntry, LocalModelStorage, ModelSettings,
        };

        let gguf_dir = data_dir.join("models").join("gguf");

        // The catalog stores bare stems but files are `{stem}.gguf`: normalise to the filename.
        let owned_with_ext;
        let model_id =
            if !model_id.contains('/') && !model_id.contains(':') && !model_id.ends_with(".gguf") {
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
        if model_id.ends_with(".gguf") && !model_id.contains(':') {
            let path = std::path::Path::new(model_id);
            let filename = path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| model_id.to_string());

            // Stable synthetic registry key = stem (strip ".gguf").
            let stem = filename.trim_end_matches(".gguf").to_string();

            let local_path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                gguf_dir.join(&filename)
            };

            let (tool_mode, thinking) = Self::registration_settings(&local_path);
            {
                match get_registry().lock() {
                    Ok(mut registry) => {
                        if !registry.has_model(&stem) {
                            let mut settings = ModelSettings::default();
                            settings.tool_calling = tool_mode;
                            settings.enable_thinking = thinking;
                            let entry = LocalModelEntry {
                                id: stem.clone(),
                                repo_id: format!("local/{}", stem),
                                filename: filename.clone(),
                                quantization: String::new(),
                                local_path,
                                source_url: String::new(),
                                backend_id: None,
                                storage: LocalModelStorage::ManualPath,
                                settings,
                                size_bytes: 0,
                                mmproj_path: None,
                                mmproj_source_url: None,
                                mmproj_size_bytes: 0,
                                mmproj_checked: false,
                                shard_files: vec![],
                            };
                            if let Err(e) = registry.add_model(entry) {
                                tracing::warn!("Could not register GGUF model '{}': {}", stem, e);
                            }
                        } else if let Some(entry) = registry.get_model(&stem) {
                            let mut s = entry.settings.clone();
                            if s.tool_calling != tool_mode || s.enable_thinking != thinking {
                                s.tool_calling = tool_mode;
                                s.enable_thinking = thinking;
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
        let (repo_id, quantization) = model_id.rsplit_once(':').unwrap_or((model_id, "Q4_K_M"));

        let id = model_id_from_repo(repo_id, quantization);

        let model_name = repo_id.split('/').last().unwrap_or(repo_id);
        let base_name = model_name.strip_suffix("-GGUF").unwrap_or(model_name);
        let filename = format!("{}-{}.gguf", base_name, quantization);

        let local_path = gguf_dir.join(&filename);
        let source_url = format!(
            "https://huggingface.co/{}/resolve/main/{}",
            repo_id, filename
        );

        // The registry lock must be released before `Self::new()`, which locks it again.
        let (tool_mode, thinking) = Self::registration_settings(&local_path);
        {
            match get_registry().lock() {
                Ok(mut registry) => {
                    if !registry.has_model(&id) {
                        let mut settings = ModelSettings::default();
                        settings.tool_calling = tool_mode;
                        settings.enable_thinking = thinking;
                        let entry = LocalModelEntry {
                            id: id.clone(),
                            repo_id: repo_id.to_string(),
                            filename: filename.clone(),
                            quantization: quantization.to_string(),
                            local_path,
                            source_url,
                            backend_id: None,
                            storage: LocalModelStorage::ManualPath,
                            settings,
                            size_bytes: 0,
                            mmproj_path: None,
                            mmproj_source_url: None,
                            mmproj_size_bytes: 0,
                            mmproj_checked: false,
                            shard_files: vec![],
                        };
                        if let Err(e) = registry.add_model(entry) {
                            tracing::warn!("Could not register GGUF model '{}': {}", id, e);
                        }
                    } else if let Some(entry) = registry.get_model(&id) {
                        let mut s = entry.settings.clone();
                        if s.tool_calling != tool_mode || s.enable_thinking != thinking {
                            s.tool_calling = tool_mode;
                            s.enable_thinking = thinking;
                            let _ = registry.update_model_settings(&id, s);
                        }
                    }
                }
                Err(e) => tracing::warn!("GGUF registry lock poisoned: {}", e),
            }
        }

        Self::new(model_id).await
    }

    /// Drafter id to use, or `None`; requires the weights on disk, not just a registry row.
    fn registered_drafter(model_id: &str) -> Option<String> {
        use goose::providers::local_inference::local_model_registry::get_registry;
        use pond_core::models::domain::drafter::drafter_for;

        let spec = drafter_for(model_id)?;
        let registry = get_registry().lock().ok()?;
        let entry = registry.get_model(spec.id)?;
        entry.local_path.exists().then(|| spec.id.to_string())
    }

    /// Stamp device settings into the registry before llama-cpp-2 loads. A device profile sends a
    /// non-CUDA build down the Jetson path so `jetson_context_size` gets exercised off-device.
    fn apply_model_settings(model_id: &str) {
        #[cfg(feature = "cuda")]
        Self::apply_jetson_settings(model_id);

        #[cfg(not(feature = "cuda"))]
        if pond_core::models::domain::device_profile::stamping_device_model_settings() {
            Self::apply_jetson_settings(model_id);
        } else {
            Self::apply_platform_settings(model_id);
        }
    }

    /// Non-CUDA settings. Without them `n_gpu_layers` is `None` and Apple Silicon runs on the CPU.
    #[cfg(not(feature = "cuda"))]
    fn apply_platform_settings(model_id: &str) {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, ModelSettings, ToolCallingMode,
        };

        // An unreadable file leaves goose its own judgement (`Auto`).
        let probe = get_registry()
            .lock()
            .ok()
            .and_then(|reg| reg.get_model(model_id).map(|e| e.local_path.clone()))
            .and_then(|p| Self::probe_model(&p));
        let (tools, thinking) = match &probe {
            Some(p) => Self::tool_and_thinking_for(p),
            None => (ToolCallingMode::Auto, true),
        };
        if let Some(p) = &probe {
            tracing::info!(
                model = model_id,
                tools = ?p.tools,
                thinking = ?p.thinking,
                "model capabilities read from its chat template"
            );
        }

        let settings = ModelSettings {
            // Full offload: unified memory needs no CPU/GPU split.
            n_gpu_layers: Some(99),
            // Goose's `estimate_max_context_for_memory()` sizes it from free RAM.
            context_size: None,
            // Batch 512 is optimal for Metal prefill throughput.
            n_batch: Some(512),
            // Flash attention reduces KV-cache memory by ~40%.
            flash_attention: Some(true),
            // Unified memory — mlock is unnecessary and can cause issues.
            use_mlock: false,
            // From the model's own chat template; see `tool_and_thinking_for`.
            tool_calling: tools,
            enable_thinking: thinking,
            // Threads: llama.cpp's auto-detect suits Apple Silicon.
            ..Default::default()
        };

        match get_registry().lock() {
            Ok(mut registry) => {
                if let Err(e) = registry.update_model_settings(model_id, settings) {
                    tracing::debug!(
                        "Platform settings not applied to '{}' (model not yet registered): {}",
                        model_id,
                        e
                    );
                } else {
                    tracing::info!(
                        "{} settings applied to model '{}' (n_gpu_layers=99, ctx=dynamic, flash_attn=true)",
                        // On aarch64 Linux this means a build without `cuda`: inference is on
                        // the CPU, and saying "Metal" would hide the bad build.
                        if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
                            "CPU-ONLY (built without the cuda feature) —"
                        } else {
                            "Metal/platform"
                        },
                        model_id
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Could not acquire model registry lock for platform settings: {}",
                    e
                );
            }
        }
    }

    /// Tool mode and thinking flag for a GGUF, from its own chat template. Always re-stamps, so an
    /// entry once persisted as `ForceNative` can't keep a mode its template can't honour.
    fn registration_settings(
        path: &std::path::Path,
    ) -> (
        goose::providers::local_inference::local_model_registry::ToolCallingMode,
        bool,
    ) {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        match Self::probe_model(path) {
            Some(probe) => Self::tool_and_thinking_for(&probe),
            None => (ToolCallingMode::Auto, true),
        }
    }

    /// Registry settings from a [`ModelProbe`]; a template without `tools` gets `ForceEmulated`.
    #[cfg_attr(not(feature = "cuda"), allow(dead_code))]
    fn tool_and_thinking_for(
        probe: &pond_core::models::domain::model_probe::ModelProbe,
    ) -> (
        goose::providers::local_inference::local_model_registry::ToolCallingMode,
        bool,
    ) {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{Thinking, ToolSupport};

        let tools = match probe.tools {
            ToolSupport::Native => ToolCallingMode::ForceNative,
            ToolSupport::Absent => ToolCallingMode::ForceEmulated,
            ToolSupport::Unknown => ToolCallingMode::Auto,
        };
        let thinking = matches!(
            probe.thinking,
            Thinking::Gated { .. } | Thinking::Always { .. }
        );
        (tools, thinking)
    }

    /// Probe a GGUF's metadata; reaching `tokenizer.chat_template` (3.8-15 MB in) takes ~40 ms.
    #[cfg_attr(not(feature = "cuda"), allow(dead_code))]
    fn probe_model(
        path: &std::path::Path,
    ) -> Option<pond_core::models::domain::model_probe::ModelProbe> {
        use pond_core::models::domain::gguf::parse_gguf_file;
        use pond_core::models::domain::model_probe::ModelProbe;
        parse_gguf_file(path).map(|info| ModelProbe::from_gguf(&info))
    }

    /// KV KiB per token from the GGUF header, or `None` to keep the measured constant. Exact for
    /// dense models; SWA needs a confirmed layer ratio (the header omits it, and 2x off OOMs).
    #[cfg_attr(not(feature = "cuda"), allow(dead_code))]
    fn kv_cost_from_header(path: &std::path::Path) -> Option<u64> {
        use pond_core::models::domain::gguf::parse_gguf_header;

        /// SWA layers per global layer, confirmed against llama.cpp's KV-cache log on the Orin.
        const CONFIRMED_SWA_PATTERNS: &[(&str, u32)] = &[("gemma4", 5)];

        // Geometry sits in the first ~2 KB, ahead of the token array; 64 KiB covers it.
        const HEAD_BYTES: usize = 64 * 1024;

        let mut buf = vec![0u8; HEAD_BYTES];
        let n = {
            use std::io::Read as _;
            let mut f = std::fs::File::open(path).ok()?;
            f.read(&mut buf).ok()?
        };
        buf.truncate(n);

        let info = parse_gguf_header(&buf)?;
        let arch = info.architecture.as_deref().unwrap_or_default();

        if info.key_length_swa.is_none() {
            // Dense: exact for any architecture; the ratio argument is unused.
            return info.kv_kib_per_token(0);
        }

        let (_, swa_per_global) = CONFIRMED_SWA_PATTERNS
            .iter()
            .find(|(name, _)| *name == arch)?;
        info.kv_kib_per_token(*swa_per_global)
    }

    /// Context that fits this model in the Jetson LLM budget: (budget - weights - compute buffers)
    /// / per-token KV cost, floored. Only Orin measurements may move the constants.
    fn jetson_context_size(
        model_bytes: u64,
        drafter_bytes: u64,
        kv_kib_per_token: Option<u64>,
    ) -> u32 {
        // The emulated board's budget when a device profile is active (`scripts/jetson-emu.sh`).
        Self::context_size_for_budget(
            crate::scheduler::llm_budget_mb(),
            model_bytes,
            drafter_bytes,
            kv_kib_per_token,
        )
    }

    /// [`Self::jetson_context_size`] against an explicit budget, so tests needn't touch the env.
    fn context_size_for_budget(
        budget_mb: u64,
        model_bytes: u64,
        drafter_bytes: u64,
        kv_kib_per_token: Option<u64>,
    ) -> u32 {
        /// KV KiB/token of the widest shipped geometry (E4B), measured on the Orin, not the Mac.
        /// Unpadded: margin is in the budget; padding to 64 floored E4B below its turn-1 prompt.
        const KV_KIB_PER_TOKEN: u64 = 56;
        /// llama.cpp compute buffers, near-flat in `n_ctx` (522 MiB measured at 4096-16384).
        const COMPUTE_BUFFER_MB: u64 = 600;
        /// Drafter buffers beyond its weights; no KV, since `ctx_other` shares the target's cache.
        /// 64 rounds up a measured 38-47 MB that under-reads (mmap'd pages count as available).
        const DRAFTER_COMPUTE_MB: u64 = 64;
        const MIN_CTX: u32 = 2048;
        const MAX_CTX: u32 = 16384;
        /// Floor unit. Not a power of two: flooring to one can discard half an affordable window,
        /// and llama.cpp's `n_ctx` needs no power of two.
        const CTX_GRANULARITY: u32 = 1024;

        let model_mb = model_bytes / (1024 * 1024);
        // A drafter's weights stay resident all session, so they come out of the budget too.
        let drafter_mb = if drafter_bytes > 0 {
            drafter_bytes / (1024 * 1024) + DRAFTER_COMPUTE_MB
        } else {
            0
        };
        let kv_mb = budget_mb
            .saturating_sub(model_mb)
            .saturating_sub(COMPUTE_BUFFER_MB)
            .saturating_sub(drafter_mb);
        // The header's cost when known, else the conservative measured constant.
        let slope = kv_kib_per_token
            .filter(|k| *k > 0)
            .unwrap_or(KV_KIB_PER_TOKEN);
        let tokens = (kv_mb * 1024) / slope;

        // Clamp in u64 before the cast so a huge allowance can't wrap u32.
        let granularity = CTX_GRANULARITY as u64;
        let floored = (tokens / granularity) * granularity;
        floored.min(MAX_CTX as u64).max(MIN_CTX as u64) as u32
    }

    /// Stamp Jetson settings into the registry; failures leave defaults. Not fail-closed on fit:
    /// past the budget `n_gpu_layers = 99` silently spills to CPU.
    fn apply_jetson_settings(model_id: &str) {
        use goose::providers::local_inference::local_model_registry::{
            get_registry, ModelSettings, ToolCallingMode,
        };

        // No row or file yet: assume the largest shipped model so the first load is conservative.
        const ASSUMED_LARGEST_MODEL_BYTES: u64 = 5 * 1024 * 1024 * 1024;
        let model_bytes = get_registry()
            .lock()
            .ok()
            .and_then(|reg| {
                reg.get_model(model_id)
                    .and_then(|e| std::fs::metadata(&e.local_path).ok())
                    .map(|m| m.len())
            })
            .unwrap_or(ASSUMED_LARGEST_MODEL_BYTES);
        let kv_kib = get_registry()
            .lock()
            .ok()
            .and_then(|reg| reg.get_model(model_id).map(|e| e.local_path.clone()))
            .and_then(|p| Self::kv_cost_from_header(&p));
        // Before sizing the window: the drafter's resident weights must come out of the budget.
        let draft_model = Self::registered_drafter(model_id);
        let drafter_bytes = draft_model
            .as_deref()
            .and_then(|id| {
                get_registry()
                    .lock()
                    .ok()?
                    .get_model(id)
                    .and_then(|e| std::fs::metadata(&e.local_path).ok())
                    .map(|m| m.len())
            })
            .unwrap_or(0);
        let context_size = Self::jetson_context_size(model_bytes, drafter_bytes, kv_kib);
        tracing::info!(
            model = model_id,
            model_mb = model_bytes / (1024 * 1024),
            kv_kib_per_token = kv_kib.map_or("fallback".to_string(), |k| k.to_string()),
            context_size,
            drafter_mb = drafter_bytes / (1024 * 1024),
            "Jetson context sized to fit this model's KV cache in the LLM budget"
        );

        let probe = get_registry()
            .lock()
            .ok()
            .and_then(|reg| reg.get_model(model_id).map(|e| e.local_path.clone()))
            .and_then(|p| Self::probe_model(&p));
        let (tools, thinking) = match &probe {
            Some(p) => Self::tool_and_thinking_for(p),
            None => (ToolCallingMode::Auto, true),
        };
        if let Some(p) = &probe {
            tracing::info!(
                model = model_id,
                tools = ?p.tools,
                thinking = ?p.thinking,
                "model capabilities read from its chat template"
            );
        }

        let jetson_settings = ModelSettings {
            // Full offload: unified memory, one 8 GB pool for CPU and GPU.
            n_gpu_layers: Some(99),
            // Turn 1 alone is ~3,250 tokens; KV buffers must stay below NvMap's ~586 MiB wall.
            context_size: Some(context_size),
            // 512 saturates the Ampere SMs in prefill within the 68 GB/s memory bandwidth.
            n_batch: Some(512),
            // 4 of the A78AE's 6 cores; 2 stay free for the OS, audio and GIAP services.
            n_threads: Some(4),
            // Halves KV memory on Ampere, and llama.cpp requires it for a quantised `type_v`.
            flash_attention: Some(true),
            // q8_0 KV: ~half of f16's memory, quality-neutral on this board; q4_0 is 6.4x noisier.
            type_k: Some("q8_0".to_string()),
            type_v: Some("q8_0".to_string()),
            // Compute buffer 522 -> 129 MiB versus the 512 default, at no measurable speed cost.
            n_ubatch: Some(128),
            // On unified memory mlock makes every GPU access page-fault.
            use_mlock: false,
            // From the model's own template; see `tool_and_thinking_for`.
            tool_calling: tools,
            enable_thinking: thinking,
            // Re-decided from disk on every build (the block is replaced whole), so a deleted
            // drafter drops out and a newly downloaded one is picked up without a restart.
            draft_model,
            ..Default::default()
        };

        match get_registry().lock() {
            Ok(mut registry) => {
                // Stamp every row resolving to this GGUF: one file is registered under several
                // ids and inference reads the canonical one, not necessarily `model_id`.
                let target = registry
                    .get_model(model_id)
                    .map(|e| Self::resolved(&e.local_path));
                let ids: Vec<String> = match &target {
                    Some(path) => registry
                        .list_models()
                        .iter()
                        .filter(|e| Self::resolved(&e.local_path) == *path)
                        .map(|e| e.id.clone())
                        .collect(),
                    None => vec![model_id.to_string()],
                };
                let mut applied = Vec::new();
                for id in &ids {
                    match registry.update_model_settings(id, jetson_settings.clone()) {
                        Ok(()) => applied.push(id.as_str()),
                        Err(e) => tracing::debug!(
                            "Jetson settings not applied to '{}' (model not yet registered): {}",
                            id,
                            e
                        ),
                    }
                }
                if applied.is_empty() {
                    tracing::debug!("Jetson settings applied to no row for '{}'", model_id);
                } else {
                    tracing::info!(
                        rows = ?applied,
                        "Applied Jetson Orin Nano CUDA settings"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Could not acquire model registry lock for Jetson settings: {}",
                    e
                );
            }
        }
    }

    /// Follow symlinks so rows naming one GGUF (a `models/gguf/` link or its blob) compare equal.
    fn resolved(p: &std::path::Path) -> std::path::PathBuf {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Strip reasoning preambles: everything up to Gemma 4's last `<channel|>`, else `<thought>`
/// or `<think>` blocks (Qwen3, DeepSeek-R1, QwQ).
fn strip_thinking_tokens(text: &str) -> String {
    const CHANNEL_CLOSE: &str = "<channel|>";
    if let Some(pos) = text.rfind(CHANNEL_CLOSE) {
        return text[pos + CHANNEL_CLOSE.len()..].trim().to_string();
    }

    if text.contains("<thought>") {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        loop {
            if let Some(start) = rest.find("<thought>") {
                out.push_str(&rest[..start]);
                if let Some(end) = rest[start..].find("</thought>") {
                    rest = &rest[start + end + "</thought>".len()..];
                } else {
                    break; // unclosed — discard tail
                }
            } else {
                out.push_str(rest);
                break;
            }
        }
        let trimmed = out.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
        return String::new();
    }

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
    fn capabilities(&self) -> pond_core::models::domain::model_capabilities::ModelCapabilities {
        let name = self.inner.model_name();
        pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(&name)
    }

    async fn complete(&self, system: &str, messages: Vec<ChatMessage>) -> Result<ChatMessage> {
        let mut msg = self.inner.complete(system, messages).await?;
        msg.content = strip_thinking_tokens(&msg.content);
        Ok(msg)
    }

    fn model_name(&self) -> String {
        self.inner.model_name()
    }
}

impl LocalInferenceLlmAdapter {
    /// `complete()` without `strip_thinking_tokens()`, for diagnostics.
    pub async fn raw_complete(
        &self,
        system: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<ChatMessage> {
        self.inner.complete(system, messages).await
    }
}

// ── Unit tests (no model weights required) ────────────────────────────────────

#[cfg(test)]
mod tests {

    /// Deliberately not `cfg(feature = "cuda")`: CI is the only place this arithmetic runs.
    #[test]
    fn jetson_context_fits_each_model_in_the_budget() {
        // Exact on-device sizes (`stat -Lc %s`): the result is a step function of weight size.
        let e2b = LocalInferenceLlmAdapter::jetson_context_size(3_106_738_272, 0, None);
        let e4b = LocalInferenceLlmAdapter::jetson_context_size(4_977_171_584, 0, None);
        assert_eq!(e2b, 16384, "E2B should keep the full window");
        assert_eq!(
            e4b, 8192,
            "E4B should get half the window. It briefly got 16384, on a budget that claimed the \
             marketing 8192 MB of RAM; the kernel reports 7620, and at the real figure E4B's \
             16384 needs 896 MiB of KV it does not have -- it was running out of swap."
        );
        assert!(
            e4b <= e2b,
            "a bigger model must never get a bigger context, got E4B {e4b} vs E2B {e2b}"
        );
    }

    /// Guards `scripts/jetson-emu.sh`, which would otherwise silently test the Mac's budget.
    #[test]
    fn a_different_device_budget_produces_a_different_window() {
        /// E4B Q4_K_M, `stat -Lc %s` on the device.
        const E4B: u64 = 4_977_171_584;
        // As the scheduler computes it: total RAM less the OS/STT/TTS reservation.
        let nano = 7620 - (1500 + 200 + 100);
        let nx = 15564 - (1500 + 200 + 100);

        let on_nano = LocalInferenceLlmAdapter::context_size_for_budget(nano, E4B, 0, None);
        let on_nx = LocalInferenceLlmAdapter::context_size_for_budget(nx, E4B, 0, None);

        assert_eq!(on_nano, 8192, "the board we actually have");
        assert!(
            on_nx > on_nano,
            "twice the RAM must buy E4B a wider window, got {on_nx} against {on_nano} -- if these \
             are equal the profile is not reaching the derivation and the emulator is theatre"
        );
    }

    #[test]
    fn the_orin_profile_reproduces_the_devices_own_windows() {
        let budget = 7620 - (1500 + 200 + 100);
        assert_eq!(
            LocalInferenceLlmAdapter::context_size_for_budget(budget, 3_106_738_272, 0, None),
            16384,
            "E2B"
        );
        assert_eq!(
            LocalInferenceLlmAdapter::context_size_for_budget(budget, 4_977_171_584, 0, None),
            8192,
            "E4B"
        );
    }

    #[test]
    fn an_impossible_budget_clamps_instead_of_wrapping() {
        assert_eq!(
            LocalInferenceLlmAdapter::context_size_for_budget(512, 4_977_171_584, 0, None),
            2048,
            "a budget the model cannot fit must land on MIN_CTX; a saturating_sub that wrapped \
             would hand llama.cpp a window of billions of tokens"
        );
    }

    /// Redoes the arithmetic independently so the test can't share the function's mistake.
    #[test]
    fn e4b_fits_its_window_and_could_not_take_another_doubling() {
        /// Orin: 128 + 320 MiB at n_ctx 8192 across both caches = 56 KiB/token.
        const MEASURED_KIB_PER_TOKEN: u64 = 56;
        let weights_mb = 4_640_000_000u64 / (1024 * 1024);
        let free_mb = crate::scheduler::LLM_BUDGET_MB - weights_mb - 600;

        let chosen = LocalInferenceLlmAdapter::jetson_context_size(4_640_000_000, 0, None) as u64;
        let needed_mb = (chosen * MEASURED_KIB_PER_TOKEN) / 1024;
        assert!(
            needed_mb < free_mb,
            "E4B was given {chosen} tokens, needing {needed_mb} MiB of KV against {free_mb} MiB \
             free. Exceeding this is what OOM-killed the board and took gnome-shell with it."
        );

        let doubled_mb = (chosen * 2 * MEASURED_KIB_PER_TOKEN) / 1024;
        assert!(
            doubled_mb > free_mb,
            "doubling E4B's window now fits ({doubled_mb} MiB vs {free_mb} MiB free), so memory \
             is no longer what caps it. If the budget really grew, raise MAX_CTX deliberately -- \
             and weigh prefill, which is 19.97 s cold at 16384 and degrades with depth."
        );
    }

    /// At ~4.5 GB the budget, not `MAX_CTX`, decides, so this pins the slope whatever ships.
    /// The size lands mid-band, 512 tokens clear of both floors.
    #[test]
    fn the_per_token_slope_is_observable_on_a_model_the_ceiling_does_not_cap() {
        let ctx = LocalInferenceLlmAdapter::jetson_context_size(4_739_563_520, 0, None);
        assert_eq!(
            ctx, 12288,
            "a 4.5 GB model got {ctx} tokens. At the device-measured cost it should get 12288; \
             16384 means the slope has been lowered towards the Mac's 16 KiB/token, which \
             describes a newer llama.cpp than the one this device ships and understates the real \
             allocation by roughly three times."
        );
    }

    #[test]
    fn rounding_does_not_discard_context_the_budget_affords() {
        // The real IQ4_XS file on the device: 4,715,416,704 bytes.
        let ctx = LocalInferenceLlmAdapter::jetson_context_size(4_715_416_704, 0, None);
        assert_eq!(
            ctx, 12288,
            "E4B IQ4_XS got {ctx}. Its budget affords 13,220 tokens, so anything at or below \
             8192 means the rounding went back to powers of two and is discarding a third of \
             the window the board can actually hold."
        );

        // And the floor still rounds DOWN, never up, at every offset.
        for bytes in [4_600_000_000u64, 4_700_000_000, 4_800_000_000] {
            let ctx = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, None) as u64;
            let model_mb = bytes / (1024 * 1024);
            let kv_mb = crate::scheduler::LLM_BUDGET_MB
                .saturating_sub(model_mb)
                .saturating_sub(600);
            let affords = (kv_mb * 1024) / 56;
            assert!(
                ctx <= affords.max(2048),
                "{bytes} bytes: handed {ctx} tokens against an affordable {affords}"
            );
            assert_eq!(
                ctx % 1024,
                0,
                "{bytes} bytes: {ctx} is not a multiple of 1024"
            );
        }
    }

    #[test]
    fn e2b_is_ceiling_bound_and_e4b_is_budget_bound() {
        const E2B_KIB_PER_TOKEN: u64 = 18;
        let e2b_weights = 2_890_000_000u64 / (1024 * 1024);
        let e2b_free = crate::scheduler::LLM_BUDGET_MB - e2b_weights - 600;
        let e2b_allows = (e2b_free * 1024) / E2B_KIB_PER_TOKEN;
        assert!(
            e2b_allows > 100_000,
            "E2B's memory should allow far more than it gets ({e2b_allows}); it is MAX_CTX that \
             stops it, and that is a latency decision rather than a memory one"
        );

        const E4B_KIB_PER_TOKEN: u64 = 56;
        let e4b_weights = 4_640_000_000u64 / (1024 * 1024);
        let e4b_free = crate::scheduler::LLM_BUDGET_MB - e4b_weights - 600;
        let e4b_allows = (e4b_free * 1024) / E4B_KIB_PER_TOKEN;
        assert!(
            (8_192..16_384).contains(&e4b_allows),
            "E4B's memory should allow its 8192 window but not a doubling of it, got \
             {e4b_allows}. Outside that range the budget is no longer what binds it and this \
             test's name is a lie."
        );
    }

    #[test]
    fn header_derived_cost_is_a_no_op_for_the_shipped_models() {
        // (weights, computed KiB/token, expected window)
        let cases = [
            (3_106_738_272u64, 18u64, 16384u32), // E2B Q4_K_M
            (4_977_171_584, 56, 8192),           // E4B Q4_K_M
            (4_715_416_704, 56, 12288),          // E4B IQ4_XS
        ];
        for (bytes, kv, want) in cases {
            let fallback = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, None);
            let derived = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, Some(kv));
            assert_eq!(
                derived, want,
                "{bytes} bytes at {kv} KiB/token should give {want}, got {derived}"
            );
            assert_eq!(
                derived, fallback,
                "{bytes} bytes: header-derived {derived} must match the fallback {fallback}                  for a model we already ship -- if this moved, the wiring changed behaviour                  on hardware nobody re-measured"
            );
        }
    }

    /// 4,500 MB, because a lighter model is `MAX_CTX`-bound at both costs and would prove nothing.
    #[test]
    fn a_cheaper_model_is_no_longer_charged_the_widest_geometry() {
        let bytes = 4_500u64 * 1024 * 1024;
        let blanket = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, None);
        let real = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, Some(28));
        assert!(
            real > blanket,
            "a 28 KiB/token model should get more than the 56 KiB/token fallback allows,              got {real} against {blanket}"
        );
    }

    #[test]
    fn a_wider_model_is_charged_for_it() {
        let bytes = 4_000_000_000u64;
        let blanket = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, None);
        let real = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, Some(168));
        assert!(
            real < blanket,
            "a 168 KiB/token model must get LESS than the 56 fallback grants, got {real}              against {blanket}; this is the direction that OOMs the board"
        );
    }

    #[test]
    fn a_useless_slope_falls_back_rather_than_dividing_by_zero() {
        let bytes = 4_977_171_584u64;
        let fallback = LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, None);
        assert_eq!(
            LocalInferenceLlmAdapter::jetson_context_size(bytes, 0, Some(0)),
            fallback
        );
    }

    /// Run with `GIAP_TEST_GGUF_DIR` set to a `models/gguf` dir. The decisions must differ across
    /// files: a probe giving one answer for everything would look like it works.
    #[test]
    #[ignore = "needs real GGUF files; set GIAP_TEST_GGUF_DIR"]
    fn probe_model_reads_real_files_and_separates_them() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;

        let Ok(dir) = std::env::var("GIAP_TEST_GGUF_DIR") else {
            eprintln!("GIAP_TEST_GGUF_DIR unset");
            return;
        };
        let mut modes = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(&dir).expect("dir") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
                continue;
            }
            let (mode, thinking) = match LocalInferenceLlmAdapter::probe_model(&path) {
                Some(p) => LocalInferenceLlmAdapter::tool_and_thinking_for(&p),
                None => (ToolCallingMode::Auto, true),
            };
            eprintln!(
                "{:<44} {:?} thinking={}",
                path.file_name().unwrap().to_string_lossy(),
                mode,
                thinking
            );
            *modes.entry(format!("{mode:?}")).or_insert(0) += 1;
        }
        assert!(!modes.is_empty(), "no GGUF files under {dir}");
        assert!(
            modes.len() > 1,
            "every model resolved to the same tool mode ({modes:?}); a probe that cannot \
             tell them apart is the blanket ForceNative with extra steps"
        );
        assert!(
            modes.contains_key("ForceEmulated"),
            "expected at least one model whose template carries no `tools` variable; \
             got {modes:?}"
        );
    }

    #[test]
    fn a_tool_using_model_keeps_native_calling() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        let gemma = ModelProbe {
            tools: ToolSupport::Native,
            thinking: Thinking::Gated {
                marker: "<|think|>".into(),
            },
            context_window_tokens: Some(131072),
            architecture: Some("gemma4".into()),
        };
        let (tools, thinking) = LocalInferenceLlmAdapter::tool_and_thinking_for(&gemma);
        assert_eq!(tools, ToolCallingMode::ForceNative);
        assert!(
            thinking,
            "a gated thinker must still get true -- this wiring must not change what \
             a currently-reasoning model does"
        );
    }

    /// DeepSeek-R1-Distill's template takes no `tools` variable.
    #[test]
    fn a_model_whose_template_cannot_carry_tools_is_not_forced_native() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        let deepseek = ModelProbe {
            tools: ToolSupport::Absent,
            thinking: Thinking::Always {
                marker: "<think>".into(),
            },
            context_window_tokens: Some(131072),
            architecture: Some("qwen2".into()),
        };
        let (tools, thinking) = LocalInferenceLlmAdapter::tool_and_thinking_for(&deepseek);
        assert_eq!(
            tools,
            ToolCallingMode::ForceEmulated,
            "forcing native on a template with no `tools` variable renders declarations \
             nowhere at all"
        );
        assert!(
            thinking,
            "it reasons unconditionally; there is no flag to clear"
        );
    }

    #[test]
    fn an_unreadable_model_defers_rather_than_forcing() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        let unknown = ModelProbe {
            tools: ToolSupport::Unknown,
            thinking: Thinking::Unknown,
            context_window_tokens: None,
            architecture: None,
        };
        let (tools, thinking) = LocalInferenceLlmAdapter::tool_and_thinking_for(&unknown);
        assert_eq!(tools, ToolCallingMode::Auto);
        assert!(!thinking, "nothing said it reasons");
    }

    #[test]
    fn a_model_with_no_reasoning_markers_does_not_get_the_flag() {
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        let plain = ModelProbe {
            tools: ToolSupport::Native,
            thinking: Thinking::Absent,
            context_window_tokens: Some(32768),
            architecture: Some("gemma3".into()),
        };
        let (_, thinking) = LocalInferenceLlmAdapter::tool_and_thinking_for(&plain);
        assert!(!thinking);
    }

    #[test]
    fn jetson_context_floors_for_an_oversized_model() {
        assert_eq!(
            LocalInferenceLlmAdapter::jetson_context_size(9_000_000_000, 0, None),
            2048
        );
    }
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

    /// Replicates `new_with_data_dir`'s filename logic: calling it would download weights.
    #[test]
    fn data_dir_filename_derivation_strips_gguf_suffix() {
        let model_id = "bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M";
        let (repo_id, quantization) = model_id.rsplit_once(':').unwrap();
        let model_name = repo_id.split('/').last().unwrap();
        let base_name = model_name.strip_suffix("-GGUF").unwrap_or(model_name);
        let filename = format!("{}-{}.gguf", base_name, quantization);

        assert_eq!(filename, "Llama-3.2-3B-Instruct-Q4_K_M.gguf");
    }

    #[test]
    fn data_dir_filename_without_gguf_suffix_kept_as_is() {
        let model_id = "bartowski/SomeModel:Q8_0";
        let (repo_id, quantization) = model_id.rsplit_once(':').unwrap();
        let model_name = repo_id.split('/').last().unwrap();
        let base_name = model_name.strip_suffix("-GGUF").unwrap_or(model_name);
        let filename = format!("{}-{}.gguf", base_name, quantization);

        assert_eq!(filename, "SomeModel-Q8_0.gguf");
    }

    #[test]
    fn rows_naming_one_file_through_a_symlink_compare_equal() {
        let tmp = tempfile::tempdir().unwrap();
        let blob = tmp.path().join("blobs").join("deadbeef");
        std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
        std::fs::write(&blob, b"GGUF").unwrap();
        let link = tmp.path().join("model.gguf");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&blob, &link).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(&blob, &link).unwrap();

        assert_eq!(
            LocalInferenceLlmAdapter::resolved(&link),
            LocalInferenceLlmAdapter::resolved(&blob),
            "a models/gguf symlink and its hf_cache blob are the same file"
        );
    }

    /// A row whose weights were deleted must still match itself to stay in stamping.
    #[test]
    fn a_missing_path_still_compares_with_itself() {
        let p = std::path::Path::new("/nowhere/at/all/model.gguf");
        assert_eq!(
            LocalInferenceLlmAdapter::resolved(p),
            LocalInferenceLlmAdapter::resolved(p)
        );
    }

    #[test]
    fn a_drafter_costs_window() {
        const E2B: u64 = 3_106_738_272;
        const DRAFTER: u64 = 59_235_648; // the real mtp-gemma-4-E2B-it.gguf
        let without = LocalInferenceLlmAdapter::jetson_context_size(E2B, 0, Some(18));
        let with = LocalInferenceLlmAdapter::jetson_context_size(E2B, DRAFTER, Some(18));
        assert!(
            with <= without,
            "attaching a drafter must not grow the window: {without} -> {with}"
        );
        assert!(
            with >= 8192,
            "E2B should still get a usable window with a drafter attached, got {with}"
        );
    }

    #[test]
    fn the_drafter_is_charged_once_not_per_token() {
        const E4B: u64 = 4_977_171_584;
        const DRAFTER: u64 = 59_678_016;
        let budget = crate::scheduler::llm_budget_mb();
        let without =
            LocalInferenceLlmAdapter::context_size_for_budget(budget, E4B, 0, Some(56)) as u64;
        let with = LocalInferenceLlmAdapter::context_size_for_budget(budget, E4B, DRAFTER, Some(56))
            as u64;
        // 57 MB of weights + a 64 MB allowance, against 56 KiB/token.
        let expected_loss = (57 + 64) * 1024 / 56;
        let actual_loss = without.saturating_sub(with);
        assert!(
            actual_loss <= expected_loss + 1024,
            "lost {actual_loss} tokens for a drafter that should cost about {expected_loss}"
        );
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
        let raw = "<|channel>thought Step 1.<channel|>intermediate<channel|>Final answer.";
        assert_eq!(strip_thinking_tokens(raw), "Final answer.");
    }

    #[test]
    fn strip_thinking_tokens_channel_close_at_end_returns_empty() {
        let raw = "<|channel>thought reasoning here<channel|>";
        assert_eq!(strip_thinking_tokens(raw), "");
    }

    #[test]
    fn strip_thinking_tokens_removes_thought_tags() {
        let raw = "<thought>internal reasoning</thought>Hello!";
        assert_eq!(strip_thinking_tokens(raw), "Hello!");
    }

    #[test]
    fn strip_thinking_tokens_thought_only_returns_empty() {
        let raw = "<thought>only reasoning</thought>";
        assert_eq!(strip_thinking_tokens(raw), "");
    }

    #[test]
    fn model_id_rsplit_fallback_uses_q4_k_m() {
        let model_id = "some-model-without-quant";
        let (_repo_id, quantization) = model_id.rsplit_once(':').unwrap_or((model_id, "Q4_K_M"));
        assert_eq!(quantization, "Q4_K_M");
    }

    /// Mirrors the Jetson `ModelSettings` literal; only hardware can check the values.
    #[test]
    fn the_jetson_settings_block_still_type_checks_off_device() {
        use goose::providers::local_inference::local_model_registry::{
            ModelSettings, ToolCallingMode,
        };

        let settings = ModelSettings {
            n_gpu_layers: Some(99),
            context_size: Some(16384),
            n_batch: Some(512),
            n_threads: Some(4),
            flash_attention: Some(true),
            type_k: Some("q8_0".to_string()),
            type_v: Some("q8_0".to_string()),
            n_ubatch: Some(128),
            use_mlock: false,
            tool_calling: ToolCallingMode::ForceNative,
            enable_thinking: true,
            ..Default::default()
        };

        assert_eq!(settings.n_ubatch, Some(128));
        assert_eq!(settings.type_k.as_deref(), Some("q8_0"));
        assert_eq!(settings.type_v.as_deref(), Some("q8_0"));
        // llama.cpp refuses a quantised V cache without flash attention.
        assert_eq!(settings.flash_attention, Some(true));
    }
}
