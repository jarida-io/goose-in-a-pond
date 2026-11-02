//! Model catalog providers: static curated lists, local Ollama, and a composite of both.

use anyhow::Result;
use async_trait::async_trait;
use pond_core::models::domain::curated::{self, CuratedModel};
use pond_core::models::domain::embedding_models::{EmbeddingModel, EMBEDDING_MODELS};
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

/// The bundled catalogue: Whisper, Kokoro voices, GIAP's picks and embedding models.
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
    out.extend(curated_models());
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

// ── GIAP's picks ──────────────────────────────────────────────────────────────

/// One row per pick, downloading the pinned revision; anything else is added from Hugging Face.
fn curated_models() -> Vec<ModelRecord> {
    curated::CURATED.iter().map(curated_record).collect()
}

fn curated_record(pick: &CuratedModel) -> ModelRecord {
    let size_mb = pick.size_mb();
    ModelRecord {
        id: pick.id(),
        category: pick.category(),
        name: pick.name().to_string(),
        filename: Some(pick.filename.to_string()),
        description: pick.summary.to_string(),
        size_mb,
        url: Some(pick.url()),
        hf_id: pick.quantization.map(|q| format!("{}:{q}", pick.repo)),
        // Weights + 25% for the run, the catalogue's own rule of thumb.
        ram_estimate_mb: Some(size_mb + size_mb / 4),
        recommended_role: Some("chat".to_string()),
        context_length: Some(pick.context_length),
        quantization: pick.quantization.map(str::to_string),
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

// ── Embedding ────────────────────────────────────────────────────────────────

/// GGUF rows download their pinned file into `models/embedding`; fastembed rows name no file.
fn embedding_models() -> Vec<ModelRecord> {
    EMBEDDING_MODELS
        .iter()
        .map(EmbeddingModel::record)
        .collect()
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

    fn conversation_rows() -> Vec<ModelRecord> {
        static_models()
            .into_iter()
            .filter(|m| m.category.is_llm())
            .collect()
    }

    /// Rung 3 of the context governor reads `context_length`; every pick must declare one.
    #[test]
    fn every_chat_capable_entry_declares_a_context_window() {
        let rows = conversation_rows();
        assert_eq!(
            rows.len(),
            curated::CURATED.len(),
            "the bundled conversation rows are exactly GIAP's picks"
        );
        for pick in curated::CURATED {
            let row = rows
                .iter()
                .find(|m| m.id == pick.id())
                .unwrap_or_else(|| panic!("{} has no catalogue row", pick.id()));
            let declared = row.context_length.unwrap_or(0);
            assert!(
                declared >= 2048,
                "catalogue entry {} declares no usable context window ({declared})",
                row.name
            );
            assert_eq!(declared, pick.context_length, "{}", row.name);
        }
    }

    /// The download pins what the URL names; a `main` URL would fetch whatever is there now.
    #[test]
    fn every_pick_downloads_its_pinned_revision() {
        for m in conversation_rows() {
            let url = m.url.as_deref().expect("a download URL");
            let (repo, revision, file) = pond_hf_cache::parse_hf_url(url).expect("an HF URL");
            let pin = curated::pinned(&repo, &revision, &file)
                .unwrap_or_else(|| panic!("{} downloads an unpinned file: {url}", m.name));
            assert_eq!(m.name, pin.name());
            assert_eq!(m.filename.as_deref(), Some(pin.filename));
            assert_eq!(m.id, pin.id());
            assert_eq!(m.size_mb, pin.size_mb(), "{}", m.name);
            assert_eq!(m.category, pin.category());
        }
    }

    #[test]
    fn the_embedding_rows_are_the_table() {
        let rows: Vec<ModelRecord> = static_models()
            .into_iter()
            .filter(|m| m.category == ModelCategory::Embedding)
            .collect();
        assert_eq!(rows.len(), EMBEDDING_MODELS.len());
        for (row, model) in rows.iter().zip(EMBEDDING_MODELS) {
            assert_eq!(row.id, model.id());
            match model.gguf() {
                Some(file) => {
                    let url = row.url.as_deref().expect("a GGUF row downloads");
                    let (repo, revision, name) =
                        pond_hf_cache::parse_hf_url(url).expect("an HF URL");
                    assert_eq!(curated::file_pin(&repo, &revision, &name), Some(file.pin()));
                    assert_eq!(row.filename.as_deref(), Some(file.filename));
                }
                None => assert!(row.filename.is_none() && row.url.is_none(), "{}", row.name),
            }
        }
    }

    #[test]
    fn the_household_pick_keeps_its_name() {
        let rows = conversation_rows();
        let e4b = rows
            .iter()
            .find(|m| m.id == "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL")
            .expect("the household's chat model is a pick");
        assert_eq!(
            e4b.filename.as_deref(),
            Some("gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf")
        );
        assert_eq!(e4b.quantization.as_deref(), Some("UD-Q4_K_XL"));
        assert!(rows.iter().all(|m| m.category != ModelCategory::Llamafile));
    }
}
