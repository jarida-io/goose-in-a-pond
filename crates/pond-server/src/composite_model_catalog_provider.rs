//! Model catalog providers: static curated lists, local Ollama, and a composite of both.

use anyhow::Result;
use async_trait::async_trait;
use pond_core::models::domain::model_record::{BinaryRecord, ModelCategory, ModelRecord};
use pond_core::models::ports::model_catalog_provider::ModelCatalogProvider;

// ── Composite ─────────────────────────────────────────────────────────────────

/// Aggregates `ModelCatalogProvider`s; one failing is logged and skipped, not fatal.
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
                // Debug, not warn: usually Ollama is just not running, which is normal.
                Err(e) => tracing::debug!("catalog sub-provider failed: {e}"),
            }
        }
        Ok((all_models, all_binaries))
    }
}

// ── Static curated catalog ────────────────────────────────────────────────────

/// Curated static catalog: Whisper, Kokoro TTS, Llamafile, GGUF and embedding models.
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
    out.extend(kokoro_tts_voices());
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

// ── Kokoro TTS ────────────────────────────────────────────────────────────────

const KOKORO_BASE: &str =
    "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main/voices/";

/// Every voice Kokoro publishes; a `.bin` is fetched only when chosen. Ids are Kokoro's own
/// `<lang><gender>_<name>`, which the desktop parses for display name and grouping.
const KOKORO_VOICES: &[(&str, &str)] = &[
    ("af_heart", "American female — Heart"),
    ("af_alloy", "American female — Alloy"),
    ("af_aoede", "American female — Aoede"),
    ("af_bella", "American female — Bella"),
    ("af_jessica", "American female — Jessica"),
    ("af_kore", "American female — Kore"),
    ("af_nicole", "American female — Nicole"),
    ("af_nova", "American female — Nova"),
    ("af_river", "American female — River"),
    ("af_sarah", "American female — Sarah"),
    ("af_sky", "American female — Sky"),
    ("am_adam", "American male — Adam"),
    ("am_echo", "American male — Echo"),
    ("am_eric", "American male — Eric"),
    ("am_fenrir", "American male — Fenrir"),
    ("am_liam", "American male — Liam"),
    ("am_michael", "American male — Michael"),
    ("am_onyx", "American male — Onyx"),
    ("am_puck", "American male — Puck"),
    ("am_santa", "American male — Santa"),
    ("bf_alice", "British female — Alice"),
    ("bf_emma", "British female — Emma"),
    ("bf_isabella", "British female — Isabella"),
    ("bf_lily", "British female — Lily"),
    ("bm_daniel", "British male — Daniel"),
    ("bm_fable", "British male — Fable"),
    ("bm_george", "British male — George"),
    ("bm_lewis", "British male — Lewis"),
    ("jf_alpha", "Japanese female — Alpha"),
    ("jf_gongitsune", "Japanese female — Gongitsune"),
    ("jf_nezumi", "Japanese female — Nezumi"),
    ("jf_tebukuro", "Japanese female — Tebukuro"),
    ("jm_kumo", "Japanese male — Kumo"),
    ("zf_xiaobei", "Mandarin female — Xiaobei"),
    ("zf_xiaoni", "Mandarin female — Xiaoni"),
    ("zf_xiaoxiao", "Mandarin female — Xiaoxiao"),
    ("zf_xiaoyi", "Mandarin female — Xiaoyi"),
    ("zm_yunjian", "Mandarin male — Yunjian"),
    ("zm_yunxi", "Mandarin male — Yunxi"),
    ("zm_yunxia", "Mandarin male — Yunxia"),
    ("zm_yunyang", "Mandarin male — Yunyang"),
    ("ef_dora", "Spanish female — Dora"),
    ("em_alex", "Spanish male — Alex"),
    ("em_santa", "Spanish male — Santa"),
    ("ff_siwis", "French female — Siwis"),
    ("hf_alpha", "Hindi female — Alpha"),
    ("hf_beta", "Hindi female — Beta"),
    ("hm_omega", "Hindi male — Omega"),
    ("hm_psi", "Hindi male — Psi"),
    ("if_sara", "Italian female — Sara"),
    ("im_nicola", "Italian male — Nicola"),
    ("pf_dora", "Portuguese female — Dora"),
    ("pm_alex", "Portuguese male — Alex"),
    ("pm_santa", "Portuguese male — Santa"),
];

/// One row per Kokoro voice: a 522 KB style table over shared weights, hence `size_mb: 1`.
fn kokoro_tts_voices() -> Vec<ModelRecord> {
    KOKORO_VOICES
        .iter()
        .map(|(id, description)| ModelRecord {
            id: ModelRecord::id_for(&ModelCategory::TtsKokoro, id),
            category: ModelCategory::TtsKokoro,
            name: id.to_string(),
            filename: Some(format!("{id}.bin")),
            description: description.to_string(),
            size_mb: 1,
            url: Some(format!("{KOKORO_BASE}{id}.bin")),
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: Some("tts".to_string()),
            context_length: None,
            quantization: None,
            asr_language: None,
            asr_size: None,
            tts_engine: Some("kokoro".to_string()),
            tts_voice_name: Some(id.to_string()),
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: Some(24_000),
            downloaded: false,
            is_custom: false,
        })
        .collect()
}

// ── Llamafile ─────────────────────────────────────────────────────────────────

struct LlamafileEntry {
    name: &'static str,
    filename: &'static str,
    mozilla_repo: &'static str,
    size_mb: u64,
    ram_estimate_mb: u64,
    /// The base model's declared maximum; must match the GGUF row for the same weights.
    context_length: u32,
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
        context_length: Some(e.context_length),
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
            context_length: 131072,
            recommended_role: "chat",
            description: "Llama 3.2 1B Instruct Q4_K_M (~1.1 GB, fastest)",
        },
        LlamafileEntry {
            name: "gemma-2b",
            filename: "gemma-2-2b-it.Q4_K_M.llamafile",
            mozilla_repo: "gemma-2-2b-it-llamafile",
            size_mb: 1950,
            ram_estimate_mb: 1800,
            context_length: 8192,
            recommended_role: "chat",
            description: "Gemma 2 2B IT Q4_K_M (~2.0 GB, smarter) — default",
        },
        LlamafileEntry {
            name: "llama-3b",
            filename: "Llama-3.2-3B-Instruct-Q4_K_M.llamafile",
            mozilla_repo: "Llama-3.2-3B-Instruct-llamafile",
            size_mb: 2020,
            ram_estimate_mb: 2500,
            context_length: 131072,
            recommended_role: "chat",
            description: "Llama 3.2 3B Instruct Q4_K_M (~2.0 GB, balanced)",
        },
        LlamafileEntry {
            name: "phi-3.5-mini",
            filename: "Phi-3.5-mini-instruct.Q4_K_M.llamafile",
            mozilla_repo: "Phi-3.5-mini-instruct-llamafile",
            size_mb: 2390,
            ram_estimate_mb: 2600,
            context_length: 131072,
            recommended_role: "think",
            description: "Phi-3.5 Mini Instruct Q4_K_M (~2.4 GB, efficient reasoning)",
        },
        LlamafileEntry {
            name: "mistral-7b",
            filename: "Mistral-7B-Instruct-v0.2.Q4_K_M.llamafile",
            mozilla_repo: "Mistral-7B-Instruct-v0.2-llamafile",
            size_mb: 4370,
            ram_estimate_mb: 5200,
            context_length: 32768,
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
            context_length: 131072,
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
            context_length: 131072,
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
            context_length: 131072,
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
            context_length: 131072,
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
            context_length: 131072,
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
            context_length: 131072,
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
            context_length: 131072,
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

/// Models installed in local Ollama; errs when it is not running (the composite skips it).
pub struct OllamaCatalogProvider {
    client: reqwest::Client,
}

impl OllamaCatalogProvider {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// Declared context window from `POST /api/show` (`/api/tags` has none). Keys are per
    /// architecture, so match the `.context_length` suffix; any failure is `None`, not an error.
    async fn declared_context_length(&self, model: &str) -> Option<u32> {
        let resp = self
            .client
            .post("http://localhost:11434/api/show")
            .timeout(std::time::Duration::from_secs(3))
            .json(&serde_json::json!({ "model": model }))
            .send()
            .await
            .ok()?
            .json::<serde_json::Value>()
            .await
            .ok()?;

        resp["model_info"]
            .as_object()?
            .iter()
            .find(|(k, _)| k.ends_with(".context_length"))
            .and_then(|(_, v)| v.as_u64())
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| *v > 0)
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

        let mut models: Vec<ModelRecord> = resp["models"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(ollama_entry_to_record)
            .collect();

        // Catalog refresh only, never per turn; sequential so N models don't hammer Ollama.
        for m in &mut models {
            m.context_length = self.declared_context_length(&m.name).await;
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chat_capable_entry_declares_a_context_window() {
        let chat_capable: Vec<ModelRecord> = static_models()
            .into_iter()
            .filter(|m| matches!(m.category, ModelCategory::Gguf | ModelCategory::Llamafile))
            .collect();

        // A floor, so a broken filter cannot pass by matching nothing.
        assert!(
            chat_capable.len() >= 20,
            "only {} chat-capable catalog entries — the filter has broken, \
             not the catalog shrunk",
            chat_capable.len()
        );

        for m in &chat_capable {
            let declared = m.context_length.unwrap_or(0);
            assert!(
                declared >= 2048,
                "catalog entry {} declares no usable context window ({declared}); \
                 rung 3 of the context governor reads this field",
                m.name
            );
        }
    }

    #[test]
    fn the_same_base_model_declares_the_same_window_in_both_tables() {
        let by_base = |needle: &str| -> Vec<(String, u32)> {
            static_models()
                .iter()
                .filter(|m| {
                    matches!(m.category, ModelCategory::Gguf | ModelCategory::Llamafile)
                        && m.filename
                            .as_deref()
                            .is_some_and(|f| f.to_ascii_lowercase().contains(needle))
                })
                .map(|m| (m.name.clone(), m.context_length.unwrap_or(0)))
                .collect()
        };

        for needle in ["llama-3.2-1b", "llama-3.2-3b", "gemma-2-2b"] {
            let found = by_base(needle);
            assert!(
                found.len() >= 2,
                "expected {needle} in both the GGUF and llamafile tables, found {found:?}"
            );
            let first = found[0].1;
            assert!(
                found.iter().all(|(_, c)| *c == first),
                "{needle} declares different windows across tables: {found:?}"
            );
        }
    }
}
