//! Source-specific model catalog providers.
//!
//! Three types are defined here:
//! - [`CompositeModelCatalogProvider`] — aggregates all sub-providers
//! - [`StaticModelCatalogProvider`]   — curated static lists for Whisper, Piper TTS, Llamafile, GGUF, Embedding
//! - [`OllamaCatalogProvider`]        — queries the local Ollama instance (`/api/tags`)

use anyhow::Result;
use async_trait::async_trait;
use pond_core::models::domain::model_record::{BinaryRecord, ModelCategory, ModelRecord};
use pond_core::models::ports::model_catalog_provider::ModelCatalogProvider;

// ── Composite ─────────────────────────────────────────────────────────────────

/// Aggregates multiple `ModelCatalogProvider` implementations.
///
/// Each sub-provider is called in sequence. Failures from any individual
/// provider are logged as warnings and skipped — the remaining providers
/// still contribute their records.
pub struct CompositeModelCatalogProvider {
    providers: Vec<Box<dyn ModelCatalogProvider>>,
}

impl CompositeModelCatalogProvider {
    /// Build the standard composite with static + local-Ollama sources.
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            providers: vec![
                Box::new(StaticModelCatalogProvider),
                Box::new(OllamaCatalogProvider::new(client)),
            ],
        }
    }
}

#[async_trait]
impl ModelCatalogProvider for CompositeModelCatalogProvider {
    async fn fetch(&self) -> Result<(Vec<ModelRecord>, Vec<BinaryRecord>)> {
        let mut all_models = Vec::new();
        let mut all_binaries = Vec::new();
        for provider in &self.providers {
            match provider.fetch().await {
                Ok((models, binaries)) => {
                    all_models.extend(models);
                    all_binaries.extend(binaries);
                }
                Err(e) => tracing::warn!("catalog sub-provider failed: {e}"),
            }
        }
        Ok((all_models, all_binaries))
    }
}

// ── Static curated catalog ────────────────────────────────────────────────────

/// Returns a static curated list of models for Whisper, Piper TTS, Llamafile, GGUF, and Embedding.
///
/// Ollama models are handled by [`OllamaCatalogProvider`] at runtime.
pub struct StaticModelCatalogProvider;

#[async_trait]
impl ModelCatalogProvider for StaticModelCatalogProvider {
    async fn fetch(&self) -> Result<(Vec<ModelRecord>, Vec<BinaryRecord>)> {
        Ok((static_models(), vec![]))
    }
}

fn static_models() -> Vec<ModelRecord> {
    let mut out = Vec::new();
    out.extend(whisper_models());
    out.extend(piper_tts_models());
    out.extend(llamafile_models());
    out.extend(gguf_models());
    out.extend(embedding_models());
    out
}

// ── Whisper ───────────────────────────────────────────────────────────────────

const WHISPER_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";

fn whisper_record(name: &str, filename: &str, size_mb: u64, language: &str) -> ModelRecord {
    ModelRecord {
        id: ModelRecord::id_for(&ModelCategory::Whisper, name),
        category: ModelCategory::Whisper,
        name: name.to_string(),
        filename: Some(filename.to_string()),
        description: format!("Whisper {} ({})", name, language),
        size_mb,
        url: Some(format!("{WHISPER_BASE}{filename}")),
        hf_id: None,
        ram_estimate_mb: None,
        recommended_role: Some("asr".to_string()),
        context_length: None,
        quantization: None,
        asr_language: Some(language.to_string()),
        asr_size: Some(name.to_string()),
        tts_engine: None,
        tts_voice_name: None,
        config_filename: None,
        config_url: None,
        tts_url: None,
        sample_rate: None,
        downloaded: false,
        is_custom: false,
    }
}

fn whisper_models() -> Vec<ModelRecord> {
    vec![
        whisper_record("tiny", "ggml-tiny.bin", 75, "multilingual"),
        whisper_record("tiny.en", "ggml-tiny.en.bin", 75, "en"),
        whisper_record("base", "ggml-base.bin", 142, "multilingual"),
        whisper_record("base.en", "ggml-base.en.bin", 142, "en"),
        whisper_record("small", "ggml-small.bin", 466, "multilingual"),
        whisper_record("small.en", "ggml-small.en.bin", 466, "en"),
        whisper_record("medium", "ggml-medium.en.bin", 1457, "en"),
        whisper_record("large-v3", "ggml-large-v3.bin", 2948, "multilingual"),
        whisper_record(
            "large-v3-turbo",
            "ggml-large-v3-turbo.bin",
            809,
            "multilingual",
        ),
    ]
}

// ── Piper TTS ─────────────────────────────────────────────────────────────────

const PIPER_BASE: &str = "https://huggingface.co/rhasspy/piper-voices/resolve/main/";

struct PiperVoice {
    name: &'static str,
    model_filename: &'static str,
    config_filename: &'static str,
    hf_path: &'static str, // path inside the HF repo (lang/lang_full/voice/quality/)
    size_mb: u64,
    description: &'static str,
}

fn piper_record(v: &PiperVoice) -> ModelRecord {
    let model_url = format!("{PIPER_BASE}{}/{}", v.hf_path, v.model_filename);
    let config_url = format!("{PIPER_BASE}{}/{}", v.hf_path, v.config_filename);
    ModelRecord {
        id: ModelRecord::id_for(&ModelCategory::TtsPiper, v.name),
        category: ModelCategory::TtsPiper,
        name: v.name.to_string(),
        filename: Some(v.model_filename.to_string()),
        description: v.description.to_string(),
        size_mb: v.size_mb,
        url: Some(model_url),
        hf_id: None,
        ram_estimate_mb: None,
        recommended_role: Some("tts".to_string()),
        context_length: None,
        quantization: None,
        asr_language: None,
        asr_size: None,
        tts_engine: Some("piper".to_string()),
        tts_voice_name: None,
        config_filename: Some(v.config_filename.to_string()),
        config_url: Some(config_url),
        tts_url: None,
        sample_rate: Some(22050),
        downloaded: false,
        is_custom: false,
    }
}

fn piper_tts_models() -> Vec<ModelRecord> {
    let voices = vec![
        PiperVoice {
            name: "en-lessac-medium",
            model_filename: "en_US-lessac-medium.onnx",
            config_filename: "en_US-lessac-medium.onnx.json",
            hf_path: "en/en_US/lessac/medium",
            size_mb: 63,
            description: "Piper en_US-lessac medium female (~63 MB) — recommended",
        },
        PiperVoice {
            name: "en-lessac-high",
            model_filename: "en_US-lessac-high.onnx",
            config_filename: "en_US-lessac-high.onnx.json",
            hf_path: "en/en_US/lessac/high",
            size_mb: 254,
            description: "Piper en_US-lessac high quality female (~254 MB)",
        },
        PiperVoice {
            name: "en-ryan-medium",
            model_filename: "en_US-ryan-medium.onnx",
            config_filename: "en_US-ryan-medium.onnx.json",
            hf_path: "en/en_US/ryan/medium",
            size_mb: 63,
            description: "Piper en_US-ryan medium male (~63 MB)",
        },
        PiperVoice {
            name: "en-ryan-high",
            model_filename: "en_US-ryan-high.onnx",
            config_filename: "en_US-ryan-high.onnx.json",
            hf_path: "en/en_US/ryan/high",
            size_mb: 254,
            description: "Piper en_US-ryan high quality male (~254 MB)",
        },
        PiperVoice {
            name: "en-jenny-dioco-medium",
            model_filename: "en_GB-jenny_dioco-medium.onnx",
            config_filename: "en_GB-jenny_dioco-medium.onnx.json",
            hf_path: "en/en_GB/jenny_dioco/medium",
            size_mb: 63,
            description: "Piper en_GB-jenny_dioco medium British female (~63 MB)",
        },
        PiperVoice {
            name: "fr-siwis-medium",
            model_filename: "fr_FR-siwis-medium.onnx",
            config_filename: "fr_FR-siwis-medium.onnx.json",
            hf_path: "fr/fr_FR/siwis/medium",
            size_mb: 63,
            description: "Piper fr_FR-siwis medium female (~63 MB)",
        },
        PiperVoice {
            name: "de-thorsten-medium",
            model_filename: "de_DE-thorsten-medium.onnx",
            config_filename: "de_DE-thorsten-medium.onnx.json",
            hf_path: "de/de_DE/thorsten/medium",
            size_mb: 63,
            description: "Piper de_DE-thorsten medium male (~63 MB)",
        },
        PiperVoice {
            name: "sw-biblia-medium",
            model_filename: "sw_CD-biblia_takatifu-medium.onnx",
            config_filename: "sw_CD-biblia_takatifu-medium.onnx.json",
            hf_path: "sw/sw_CD/biblia_takatifu/medium",
            size_mb: 63,
            description: "Piper sw_CD-biblia_takatifu medium Swahili (~63 MB)",
        },
    ];
    voices.iter().map(piper_record).collect()
}

// ── Llamafile ─────────────────────────────────────────────────────────────────

struct LlamafileEntry {
    name: &'static str,
    filename: &'static str,
    mozilla_repo: &'static str,
    size_mb: u64,
    ram_estimate_mb: u64,
    recommended_role: &'static str,
    description: &'static str,
}

fn llamafile_record(e: &LlamafileEntry) -> ModelRecord {
    let url = format!(
        "https://huggingface.co/Mozilla/{}/resolve/main/{}",
        e.mozilla_repo, e.filename
    );
    ModelRecord {
        id: ModelRecord::id_for(&ModelCategory::Llamafile, e.name),
        category: ModelCategory::Llamafile,
        name: e.name.to_string(),
        filename: Some(e.filename.to_string()),
        description: e.description.to_string(),
        size_mb: e.size_mb,
        url: Some(url),
        hf_id: None,
        ram_estimate_mb: Some(e.ram_estimate_mb),
        recommended_role: Some(e.recommended_role.to_string()),
        context_length: None,
        quantization: None,
        asr_language: None,
        asr_size: None,
        tts_engine: None,
        tts_voice_name: None,
        config_filename: None,
        config_url: None,
        tts_url: None,
        sample_rate: None,
        downloaded: false,
        is_custom: false,
    }
}

fn llamafile_models() -> Vec<ModelRecord> {
    let entries = vec![
        LlamafileEntry {
            name: "llama-1b",
            filename: "Llama-3.2-1B-Instruct-Q4_K_M.llamafile",
            mozilla_repo: "Llama-3.2-1B-Instruct-llamafile",
            size_mb: 1120,
            ram_estimate_mb: 950,
            recommended_role: "chat",
            description: "Llama 3.2 1B Instruct Q4_K_M (~1.1 GB, fastest)",
        },
        LlamafileEntry {
            name: "gemma-2b",
            filename: "gemma-2-2b-it.Q4_K_M.llamafile",
            mozilla_repo: "gemma-2-2b-it-llamafile",
            size_mb: 1950,
            ram_estimate_mb: 1800,
            recommended_role: "chat",
            description: "Gemma 2 2B IT Q4_K_M (~2.0 GB, smarter) — default",
        },
        LlamafileEntry {
            name: "llama-3b",
            filename: "Llama-3.2-3B-Instruct-Q4_K_M.llamafile",
            mozilla_repo: "Llama-3.2-3B-Instruct-llamafile",
            size_mb: 2020,
            ram_estimate_mb: 2500,
            recommended_role: "chat",
            description: "Llama 3.2 3B Instruct Q4_K_M (~2.0 GB, balanced)",
        },
        LlamafileEntry {
            name: "phi-3.5-mini",
            filename: "Phi-3.5-mini-instruct.Q4_K_M.llamafile",
            mozilla_repo: "Phi-3.5-mini-instruct-llamafile",
            size_mb: 2390,
            ram_estimate_mb: 2600,
            recommended_role: "think",
            description: "Phi-3.5 Mini Instruct Q4_K_M (~2.4 GB, efficient reasoning)",
        },
        LlamafileEntry {
            name: "mistral-7b",
            filename: "Mistral-7B-Instruct-v0.2.Q4_K_M.llamafile",
            mozilla_repo: "Mistral-7B-Instruct-v0.2-llamafile",
            size_mb: 4370,
            ram_estimate_mb: 5200,
            recommended_role: "think",
            description: "Mistral 7B Instruct v0.2 Q4_K_M (~4.4 GB, most capable)",
        },
    ];
    entries.iter().map(llamafile_record).collect()
}

// ── GGUF ──────────────────────────────────────────────────────────────────────

struct GgufEntry {
    name: &'static str,
    filename: &'static str,
    /// HuggingFace repo in `owner/repo` form (without `:quant` suffix)
    hf_repo: &'static str,
    size_mb: u64,
    ram_estimate_mb: u64,
    context_length: u32,
    quantization: &'static str,
    recommended_role: &'static str,
    description: &'static str,
}

fn gguf_record(e: &GgufEntry) -> ModelRecord {
    let url = format!(
        "https://huggingface.co/{}/resolve/main/{}",
        e.hf_repo, e.filename
    );
    ModelRecord {
        id: ModelRecord::id_for(&ModelCategory::Gguf, e.name),
        category: ModelCategory::Gguf,
        name: e.name.to_string(),
        filename: Some(e.filename.to_string()),
        description: e.description.to_string(),
        size_mb: e.size_mb,
        url: Some(url),
        hf_id: Some(format!("{}:{}", e.hf_repo, e.quantization)),
        ram_estimate_mb: Some(e.ram_estimate_mb),
        recommended_role: Some(e.recommended_role.to_string()),
        context_length: Some(e.context_length),
        quantization: Some(e.quantization.to_string()),
        asr_language: None,
        asr_size: None,
        tts_engine: None,
        tts_voice_name: None,
        config_filename: None,
        config_url: None,
        tts_url: None,
        sample_rate: None,
        downloaded: false,
        is_custom: false,
    }
}

fn gguf_models() -> Vec<ModelRecord> {
    let entries = vec![
        GgufEntry {
            name: "llama-3.2-1b",
            filename: "Llama-3.2-1B-Instruct-Q4_K_M.gguf",
            hf_repo: "bartowski/Llama-3.2-1B-Instruct-GGUF",
            size_mb: 800,
            ram_estimate_mb: 1500,
            context_length: 131072,
            quantization: "Q4_K_M",
            recommended_role: "task",
            description: "Llama 3.2 1B Instruct Q4_K_M (~800 MB, fastest — ideal for Jetson Nano)",
        },
        GgufEntry {
            name: "llama-3.2-3b",
            filename: "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
            hf_repo: "bartowski/Llama-3.2-3B-Instruct-GGUF",
            size_mb: 2000,
            ram_estimate_mb: 3000,
            context_length: 131072,
            quantization: "Q4_K_M",
            recommended_role: "chat",
            description:
                "Llama 3.2 3B Instruct Q4_K_M (~2 GB, balanced) — default for local inference",
        },
        GgufEntry {
            name: "llama-3.1-8b",
            filename: "Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf",
            hf_repo: "bartowski/Meta-Llama-3.1-8B-Instruct-GGUF",
            size_mb: 4700,
            ram_estimate_mb: 7000,
            context_length: 131072,
            quantization: "Q4_K_M",
            recommended_role: "chat",
            description: "Llama 3.1 8B Instruct Q4_K_M (~4.7 GB, best quality for 8 GB RAM)",
        },
        GgufEntry {
            name: "qwen2.5-0.5b",
            filename: "Qwen2.5-0.5B-Instruct-Q6_K.gguf",
            hf_repo: "Qwen/Qwen2.5-0.5B-Instruct-GGUF",
            size_mb: 500,
            ram_estimate_mb: 900,
            context_length: 32768,
            quantization: "Q6_K",
            recommended_role: "task",
            description: "Qwen2.5 0.5B Instruct Q6_K (~500 MB, ultra-low RAM)",
        },
        GgufEntry {
            name: "qwen2.5-1.5b",
            filename: "Qwen2.5-1.5B-Instruct-Q6_K.gguf",
            hf_repo: "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
            size_mb: 1100,
            ram_estimate_mb: 2000,
            context_length: 32768,
            quantization: "Q6_K",
            recommended_role: "task",
            description: "Qwen2.5 1.5B Instruct Q6_K (~1.1 GB)",
        },
        GgufEntry {
            name: "qwen2.5-3b",
            filename: "Qwen2.5-3B-Instruct-Q5_K_M.gguf",
            hf_repo: "Qwen/Qwen2.5-3B-Instruct-GGUF",
            size_mb: 2100,
            ram_estimate_mb: 3200,
            context_length: 32768,
            quantization: "Q5_K_M",
            recommended_role: "chat",
            description: "Qwen2.5 3B Instruct Q5_K_M (~2.1 GB)",
        },
        GgufEntry {
            name: "qwen2.5-7b",
            filename: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
            hf_repo: "Qwen/Qwen2.5-7B-Instruct-GGUF",
            size_mb: 4700,
            ram_estimate_mb: 7000,
            context_length: 32768,
            quantization: "Q4_K_M",
            recommended_role: "chat",
            description: "Qwen2.5 7B Instruct Q4_K_M (~4.7 GB, excellent instruction following)",
        },
        GgufEntry {
            name: "gemma-2-2b-it",
            filename: "gemma-2-2b-it-Q6_K.gguf",
            hf_repo: "bartowski/gemma-2-2b-it-GGUF",
            size_mb: 2100,
            ram_estimate_mb: 3200,
            context_length: 8192,
            quantization: "Q6_K",
            recommended_role: "chat",
            description: "Gemma 2 2B Instruct Q6_K (~2.1 GB)",
        },
        GgufEntry {
            name: "gemma-2-9b-it",
            filename: "gemma-2-9b-it-Q4_K_M.gguf",
            hf_repo: "bartowski/gemma-2-9b-it-GGUF",
            size_mb: 5500,
            ram_estimate_mb: 8000,
            context_length: 8192,
            quantization: "Q4_K_M",
            recommended_role: "think",
            description: "Gemma 2 9B Instruct Q4_K_M (~5.5 GB, strong reasoning)",
        },
        GgufEntry {
            name: "phi-4-mini",
            filename: "Phi-4-mini-instruct-Q4_K_M.gguf",
            hf_repo: "bartowski/Phi-4-mini-instruct-GGUF",
            size_mb: 2400,
            ram_estimate_mb: 3800,
            context_length: 131072,
            quantization: "Q4_K_M",
            recommended_role: "chat",
            description: "Phi-4 Mini Instruct Q4_K_M (~2.4 GB, strong reasoning in small package)",
        },
        GgufEntry {
            name: "mistral-7b-v0.3",
            filename: "Mistral-7B-Instruct-v0.3-Q4_K_M.gguf",
            hf_repo: "bartowski/Mistral-7B-Instruct-v0.3-GGUF",
            size_mb: 4400,
            ram_estimate_mb: 6500,
            context_length: 32768,
            quantization: "Q4_K_M",
            recommended_role: "chat",
            description: "Mistral 7B Instruct v0.3 Q4_K_M (~4.4 GB)",
        },
        GgufEntry {
            name: "deepseek-r1-1.5b",
            filename: "DeepSeek-R1-Distill-Qwen-1.5B-Q6_K.gguf",
            hf_repo: "bartowski/DeepSeek-R1-Distill-Qwen-1.5B-GGUF",
            size_mb: 1100,
            ram_estimate_mb: 2000,
            context_length: 32768,
            quantization: "Q6_K",
            recommended_role: "think",
            description: "DeepSeek R1 Distill 1.5B Q6_K (~1.1 GB, reasoning specialist)",
        },
        GgufEntry {
            name: "deepseek-r1-7b",
            filename: "DeepSeek-R1-Distill-Qwen-7B-Q4_K_M.gguf",
            hf_repo: "bartowski/DeepSeek-R1-Distill-Qwen-7B-GGUF",
            size_mb: 4700,
            ram_estimate_mb: 7000,
            context_length: 32768,
            quantization: "Q4_K_M",
            recommended_role: "think",
            description: "DeepSeek R1 Distill 7B Q4_K_M (~4.7 GB, reasoning specialist)",
        },
        // ── Gemma 4 family ────────────────────────────────────────────
        GgufEntry {
            name: "gemma-4-E1B-it",
            filename: "gemma-4-E1B-it-Q4_K_M.gguf",
            hf_repo: "unsloth/gemma-4-E1B-it-GGUF",
            size_mb: 700,
            ram_estimate_mb: 1200,
            context_length: 8192,
            quantization: "Q4_K_M",
            recommended_role: "tool",
            description: "Gemma 4 E1B Instruct Q4_K_M (~700 MB, ultra-fast tool-call specialist)",
        },
        GgufEntry {
            name: "gemma-4-E2B-it",
            filename: "gemma-4-E2B-it-Q4_K_M.gguf",
            hf_repo: "unsloth/gemma-4-E2B-it-GGUF",
            size_mb: 3100,
            ram_estimate_mb: 4500,
            context_length: 8192,
            quantization: "Q4_K_M",
            recommended_role: "chat",
            description: "Gemma 4 E2B Instruct Q4_K_M (~3.1 GB, vision + tool calling + thinking)",
        },
        GgufEntry {
            name: "gemma-4-E4B-it-Q4_K_S",
            filename: "gemma-4-E4B-it-Q4_K_S.gguf",
            hf_repo: "google/gemma-4-E4B-it-GGUF",
            size_mb: 2500,
            ram_estimate_mb: 3500,
            context_length: 8192,
            quantization: "Q4_K_S",
            recommended_role: "chat",
            description: "Gemma 4 E4B Instruct Q4_K_S (~2.5 GB)",
        },
        GgufEntry {
            name: "gemma-4-E4B-it",
            filename: "gemma-4-E4B-it-Q4_K_M.gguf",
            hf_repo: "unsloth/gemma-4-E4B-it-GGUF",
            size_mb: 3000,
            ram_estimate_mb: 4200,
            context_length: 8192,
            quantization: "Q4_K_M",
            recommended_role: "chat",
            description:
                "Gemma 4 E4B Instruct Q4_K_M (~3 GB, vision + native tool calling + thinking)",
        },
        GgufEntry {
            name: "gemma-4-12B-A4B-it",
            filename: "gemma-4-12B-A4B-it-Q4_K_M.gguf",
            hf_repo: "unsloth/gemma-4-12B-A4B-it-GGUF",
            size_mb: 7500,
            ram_estimate_mb: 10000,
            context_length: 8192,
            quantization: "Q4_K_M",
            recommended_role: "think",
            description: "Gemma 4 12B MoE (4B active) Q4_K_M (~7.5 GB, strong reasoning + vision)",
        },
        GgufEntry {
            name: "gemma-4-26B-A4B-it",
            filename: "gemma-4-26B-A4B-it-Q4_K_M.gguf",
            hf_repo: "unsloth/gemma-4-26B-A4B-it-GGUF",
            size_mb: 16000,
            ram_estimate_mb: 20000,
            context_length: 8192,
            quantization: "Q4_K_M",
            recommended_role: "think",
            description: "Gemma 4 26B MoE (4B active) Q4_K_M (~16 GB, best MoE quality + vision)",
        },
        GgufEntry {
            name: "gemma-4-27B-it",
            filename: "gemma-4-27B-it-Q4_K_M.gguf",
            hf_repo: "unsloth/gemma-4-27B-it-GGUF",
            size_mb: 16500,
            ram_estimate_mb: 21000,
            context_length: 8192,
            quantization: "Q4_K_M",
            recommended_role: "think",
            description: "Gemma 4 27B Dense Instruct Q4_K_M (~16.5 GB, highest quality + vision)",
        },
    ];
    entries.iter().map(gguf_record).collect()
}

// ── Embedding ────────────────────────────────────────────────────────────────

fn embedding_models() -> Vec<ModelRecord> {
    vec![
        ModelRecord {
            id: ModelRecord::id_for(&ModelCategory::Embedding, "all-MiniLM-L6-v2"),
            category: ModelCategory::Embedding,
            name: "all-MiniLM-L6-v2".to_string(),
            filename: None, // fastembed downloads automatically
            description: "all-MiniLM-L6-v2 — fast, lightweight sentence embeddings. 23 MB, 384 dimensions. Best for domain classification and semantic search.".to_string(),
            size_mb: 23,
            url: None,
            hf_id: None,
            ram_estimate_mb: Some(50),
            recommended_role: Some("embedding".to_string()),
            context_length: None,
            quantization: None,
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: false,
            is_custom: false,
        },
        ModelRecord {
            id: ModelRecord::id_for(&ModelCategory::Embedding, "bge-small-en-v1.5"),
            category: ModelCategory::Embedding,
            name: "bge-small-en-v1.5".to_string(),
            filename: None,
            description: "BGE Small EN v1.5 — high-quality English embedding model. 33 MB, 384 dimensions.".to_string(),
            size_mb: 33,
            url: None,
            hf_id: None,
            ram_estimate_mb: Some(70),
            recommended_role: Some("embedding".to_string()),
            context_length: None,
            quantization: None,
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: false,
            is_custom: false,
        },
        ModelRecord {
            id: ModelRecord::id_for(&ModelCategory::Embedding, "nomic-embed-text-v1.5"),
            category: ModelCategory::Embedding,
            name: "nomic-embed-text-v1.5".to_string(),
            filename: None,
            description: "Nomic Embed Text v1.5 — 768-dimension model with Matryoshka support (truncate to 256/384 dims). 274 MB.".to_string(),
            size_mb: 274,
            url: None,
            hf_id: None,
            ram_estimate_mb: Some(350),
            recommended_role: Some("embedding".to_string()),
            context_length: None,
            quantization: None,
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: false,
            is_custom: false,
        },
    ]
}

// ── Ollama (local) ────────────────────────────────────────────────────────────

/// Lists models installed in the local Ollama instance.
///
/// Queries `http://localhost:11434/api/tags`. Returns an empty vec (not an error)
/// if Ollama is not running or the request fails — the composite provider
/// continues with the static list.
pub struct OllamaCatalogProvider {
    client: reqwest::Client,
}

impl OllamaCatalogProvider {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl ModelCatalogProvider for OllamaCatalogProvider {
    async fn fetch(&self) -> Result<(Vec<ModelRecord>, Vec<BinaryRecord>)> {
        let resp = self
            .client
            .get("http://localhost:11434/api/tags")
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        let models = resp["models"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(ollama_entry_to_record)
            .collect();

        Ok((models, vec![]))
    }
}

fn ollama_entry_to_record(m: &serde_json::Value) -> Option<ModelRecord> {
    let name = m["name"].as_str()?.to_string();
    let size_bytes = m["size"].as_u64().unwrap_or(0);
    let size_mb = size_bytes / 1_000_000;
    let ram_mb = if size_mb > 0 { Some(size_mb * 2) } else { None };
    let quant = m["details"]["quantization_level"]
        .as_str()
        .map(|s| s.to_string());
    let description = format!(
        "Ollama: {} ({})",
        name,
        m["details"]["parameter_size"].as_str().unwrap_or("?")
    );

    Some(ModelRecord {
        id: ModelRecord::id_for(&ModelCategory::Ollama, &name),
        category: ModelCategory::Ollama,
        name,
        filename: None,
        description,
        size_mb,
        url: None,
        hf_id: None,
        ram_estimate_mb: ram_mb,
        recommended_role: Some("chat".to_string()),
        context_length: None,
        quantization: quant,
        asr_language: None,
        asr_size: None,
        tts_engine: None,
        tts_voice_name: None,
        config_filename: None,
        config_url: None,
        tts_url: None,
        sample_rate: None,
        downloaded: true, // Ollama models are always locally present
        is_custom: false,
    })
}
