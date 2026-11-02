//! Where a catalogue row came from, what it is for and how it is obtained, all derived from
//! the row itself so none of it needs a column.

use serde::{Deserialize, Serialize};

use super::model_record::{ModelCategory, ModelRecord};

/// The description a scanned file gets when its header names nothing; never a title.
pub const ON_DISK_PLACEHOLDER: &str = "(detected on disk)";

/// Whether a GGUF is an encoder (`mmproj-*`) or a drafter (`mtp-*`, `dflash-*`, Gemma 4
/// `-assistant`). Offered as a model it would land in `models/gguf`, where every scan takes it
/// for one.
pub fn is_companion_file(file_name: &str) -> bool {
    let base = file_name
        .rsplit('/')
        .next()
        .unwrap_or(file_name)
        .to_ascii_lowercase();
    base.starts_with("mmproj")
        || base.starts_with("mtp-")
        || base.starts_with("dflash-")
        || ((base.contains("gemma-4") || base.contains("gemma4")) && base.contains("-assistant"))
}

/// The header's own word for a companion: `clip` is an encoder, `dflash` and `*-assistant` are
/// drafters.
pub fn is_companion_architecture(architecture: Option<&str>) -> bool {
    architecture.is_some_and(|a| a == "clip" || a == "dflash" || a.ends_with("-assistant"))
}

/// A GGUF architecture that is not a conversation model: a companion, speech or embeddings.
pub fn is_helper_architecture(architecture: Option<&str>) -> bool {
    if is_companion_architecture(architecture) {
        return true;
    }
    let Some(arch) = architecture.map(str::to_ascii_lowercase) else {
        return false;
    };
    [
        "asr",
        "whisper",
        "parakeet",
        "conformer",
        "bert",
        "t5encoder",
        "embed",
        "wav2vec",
    ]
    .iter()
    .any(|marker| arch.contains(marker))
}

/// Whether a model's name says it is a helper rather than something to talk to.
pub fn is_helper_name(name: &str) -> bool {
    is_companion_file(name) || names_a_non_chat_task(name)
}

/// Whether a name has a token for speech, embeddings or tool calling; no file-name rule.
fn names_a_non_chat_task(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| {
            token == "asr"
                || token.starts_with("functiongemma")
                || token.starts_with("whisper")
                || token.starts_with("embed")
        })
}

/// The name the household reads: a pick's title, the pairing table's name for a llama.cpp file it
/// lists, a scanned file's own header name, an embedding row's catalogue title, a bundled speech
/// or voice row's description, and otherwise the row's name. Never the placeholder.
pub fn title(record: &ModelRecord, pick_title: Option<&str>) -> String {
    if let Some(title) = pick_title {
        return title.to_string();
    }
    if let Some(label) = listed_label(record) {
        return label.to_string();
    }
    if record.category == ModelCategory::Embedding {
        if let Some(model) = super::embedding_models::find(&record.name) {
            return model.title.to_string();
        }
    }
    let description = record.description.trim();
    let described = !description.is_empty() && description != ON_DISK_PLACEHOLDER;
    if described && (record.is_custom || !record.category.is_llm()) {
        return description.to_string();
    }
    record.name.clone()
}

/// What the pairing table calls a llama.cpp row's file, when it lists the file.
fn listed_label(record: &ModelRecord) -> Option<&'static str> {
    if record.category != ModelCategory::Gguf {
        return None;
    }
    let file = record
        .filename
        .clone()
        .unwrap_or_else(|| super::vision_pairing::gguf_file_name(&record.name));
    super::vision_pairing::label_for_file(&file)
}

/// The catalogue name a file goes by: a LiteRT-LM file keeps its whole name (its extension is
/// how it is told from a GGUF stem); every other file drops its extension.
pub fn catalogue_name(category: &ModelCategory, filename: &str) -> String {
    let base = filename.rsplit('/').next().unwrap_or(filename);
    if *category == ModelCategory::Litert {
        return base.to_string();
    }
    [".gguf", ".llamafile", ".exe", ".onnx", ".bin"]
        .iter()
        .find_map(|ext| base.strip_suffix(ext))
        .unwrap_or(base)
        .to_string()
}

/// Where a row came from. Derived in this order: GIAP's own list, Ollama, a download URL, disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Catalogue,
    Added,
    OnDisk,
    Ollama,
}

impl Provenance {
    /// `listed`: the row is one of GIAP's picks, or for speech, voice and embedding rows, a
    /// bundled one. Conversation rows ignore `is_custom`: a pruned pick kept for its file is Added.
    pub fn derive(record: &ModelRecord, listed: bool) -> Self {
        if listed {
            return Self::Catalogue;
        }
        if record.category == ModelCategory::Ollama {
            return Self::Ollama;
        }
        if !record.category.is_llm() && !record.is_custom {
            return Self::Catalogue;
        }
        if record.url.is_some() {
            Self::Added
        } else {
            Self::OnDisk
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Catalogue => "catalogue",
            Self::Added => "added",
            Self::OnDisk => "on_disk",
            Self::Ollama => "ollama",
        }
    }
}

/// What a row is for: something to talk to, or a helper never offered as conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Conversation,
    Helper,
}

impl ModelKind {
    pub fn of(record: &ModelRecord) -> Self {
        if !record.category.is_llm() {
            return Self::Helper;
        }
        if matches!(record.recommended_role.as_deref(), Some("tool" | "draft")) {
            return Self::Helper;
        }
        let file = record.filename.as_deref().unwrap_or_default();
        // An Ollama tag names no file: `gemma4-assistant` there is a persona, not a drafter.
        let helper = if record.category == ModelCategory::Ollama {
            names_a_non_chat_task
        } else {
            is_helper_name
        };
        if helper(&record.name) || helper(file) {
            return Self::Helper;
        }
        Self::Conversation
    }

    pub fn is_conversation(self) -> bool {
        self == Self::Conversation
    }
}

/// How a row is obtained: a download the pond runs, another program's store, or not at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Acquisition {
    Download,
    External,
    Unavailable,
}

impl Acquisition {
    /// `pinned`: GIAP knows where the file comes from even when the row lost its URL.
    pub fn of(record: &ModelRecord, pinned: bool) -> Self {
        match record.category {
            ModelCategory::Ollama | ModelCategory::TtsHttp => Self::External,
            // fastembed fetches its own weights; such rows carry no file.
            ModelCategory::Embedding if record.filename.is_none() => Self::External,
            _ if pinned || record.url.is_some() => Self::Download,
            _ => Self::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(category: ModelCategory, name: &str) -> ModelRecord {
        ModelRecord {
            id: ModelRecord::id_for(&category, name),
            category,
            name: name.to_string(),
            filename: Some(format!("{name}.gguf")),
            description: String::new(),
            size_mb: 0,
            url: None,
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: None,
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

    #[test]
    fn encoders_and_drafters_are_companions_and_chat_models_are_not() {
        for companion in [
            "mmproj-BF16.gguf",
            "subdir/mmproj-F16.gguf",
            "mtp-gemma-4-E2B-it.gguf",
            "gemma-4-E2B-it-assistant-F16.gguf",
            "gemma-4-E4B-it-assistant-Q8_0.gguf",
            "dflash-gemma-4-26B-A4B-it-BF16.gguf",
            "sub/DFlash-gemma-4-31B-it-Q8_0.gguf",
        ] {
            assert!(is_companion_file(companion), "{companion}");
        }
        for chat in [
            "gemma-4-E2B-it-Q4_K_M.gguf",
            "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf",
            "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
            "Nemotron3-Nano-4B.gguf",
        ] {
            assert!(!is_companion_file(chat), "{chat}");
        }
        assert!(is_companion_architecture(Some("clip")));
        assert!(is_companion_architecture(Some("gemma4-assistant")));
        assert!(is_companion_architecture(Some("dflash")));
        assert!(is_helper_architecture(Some("dflash")));
        assert!(!is_companion_architecture(Some("gemma4")));
        assert!(!is_companion_architecture(None));
    }

    #[test]
    fn speech_and_embedding_headers_are_helpers_and_chat_headers_are_not() {
        for helper in [
            "clip",
            "gemma4-assistant",
            "nemotron_asr",
            "parakeet",
            "fastconformer",
            "nomic-bert",
            "gemma-embedding",
            "whisper",
        ] {
            assert!(is_helper_architecture(Some(helper)), "{helper}");
        }
        for chat in ["gemma4", "llama", "qwen3", "nemotron_h", "granite", "phi3"] {
            assert!(!is_helper_architecture(Some(chat)), "{chat}");
        }
        assert!(!is_helper_architecture(None));
    }

    #[test]
    fn functiongemma_tool_rows_and_speech_files_are_never_conversation() {
        let mut tool = row(ModelCategory::Gguf, "gemma-4-E1B-it");
        tool.recommended_role = Some("tool".into());
        assert_eq!(ModelKind::of(&tool), ModelKind::Helper);

        for name in [
            "functiongemma-270m-it-Q8_0",
            "old_functiongemma-270m-it-Q4_K_M",
            "nemotron-3.5-asr-q4_k",
            "mtp-gemma-4-E2B-it",
            "gemma-4-E2B-it-assistant-F16",
        ] {
            assert_eq!(
                ModelKind::of(&row(ModelCategory::Gguf, name)),
                ModelKind::Helper,
                "{name}"
            );
        }
        for name in [
            "gemma-4-E4B-it-qat-UD-Q4_K_XL",
            "Llama-3.2-3B-Instruct-Q4_K_M",
            "NVIDIA-Nemotron3-Nano-4B-Q4_K_M",
        ] {
            assert_eq!(
                ModelKind::of(&row(ModelCategory::Gguf, name)),
                ModelKind::Conversation,
                "{name}"
            );
        }
        assert_eq!(
            ModelKind::of(&row(ModelCategory::Whisper, "base")),
            ModelKind::Helper
        );
    }

    /// An Ollama tag names no file, so a file-name rule never makes it a helper; its words do.
    #[test]
    fn an_ollama_tag_is_a_helper_by_its_words_and_never_by_a_file_name_rule() {
        let kind = |name: &str| ModelKind::of(&row(ModelCategory::Ollama, name));
        for persona in [
            "gemma4-assistant:latest",
            "gemma-4-assistant",
            "mtp-friend:7b",
            "dflash-notes:1b",
            "llama3.2",
        ] {
            assert_eq!(kind(persona), ModelKind::Conversation, "{persona}");
        }
        for helper in [
            "nomic-embed-text:latest",
            "mxbai-embed-large",
            "dimavz/whisper-tiny",
        ] {
            assert_eq!(kind(helper), ModelKind::Helper, "{helper}");
        }
        assert_eq!(
            ModelKind::of(&row(ModelCategory::Gguf, "dflash-gemma-4-26B-A4B-it-Q8_0")),
            ModelKind::Helper,
            "the same words as a file are a drafter"
        );
    }

    #[test]
    fn provenance_follows_the_list_then_ollama_then_a_url_then_disk() {
        let pick = row(ModelCategory::Gguf, "gemma-4-E4B-it-qat-UD-Q4_K_XL");
        assert_eq!(Provenance::derive(&pick, true), Provenance::Catalogue);

        let ollama = row(ModelCategory::Ollama, "llama3.2");
        assert_eq!(Provenance::derive(&ollama, false), Provenance::Ollama);

        let mut added = row(ModelCategory::Gguf, "Qwen3-4B-Q4_K_M");
        added.url = Some("https://huggingface.co/x/y/resolve/main/Qwen3-4B-Q4_K_M.gguf".into());
        assert_eq!(Provenance::derive(&added, false), Provenance::Added);

        let mut scanned = row(ModelCategory::Gguf, "my-model");
        scanned.is_custom = true;
        assert_eq!(Provenance::derive(&scanned, false), Provenance::OnDisk);

        // A pick that left the list keeps its file, and its URL makes it Added, not Catalogue.
        let mut former = row(ModelCategory::Gguf, "llama-3.2-3b");
        former.url = Some("https://huggingface.co/a/b/resolve/main/c.gguf".into());
        assert_eq!(Provenance::derive(&former, false), Provenance::Added);

        let whisper = row(ModelCategory::Whisper, "base");
        assert_eq!(Provenance::derive(&whisper, false), Provenance::Catalogue);
        let mut hand_copied = row(ModelCategory::Whisper, "ggml-custom");
        hand_copied.is_custom = true;
        assert_eq!(Provenance::derive(&hand_copied, false), Provenance::OnDisk);
    }

    #[test]
    fn ollama_and_http_voices_are_external_and_a_sourceless_file_cannot_be_fetched() {
        assert_eq!(
            Acquisition::of(&row(ModelCategory::Ollama, "llama3.2"), false),
            Acquisition::External
        );
        assert_eq!(
            Acquisition::of(&row(ModelCategory::TtsHttp, "vivian"), false),
            Acquisition::External
        );
        let mut fastembed = row(ModelCategory::Embedding, "all-MiniLM-L6-v2");
        fastembed.filename = None;
        assert_eq!(Acquisition::of(&fastembed, false), Acquisition::External);

        let scanned = row(ModelCategory::Gguf, "my-model");
        assert_eq!(Acquisition::of(&scanned, false), Acquisition::Unavailable);
        assert_eq!(Acquisition::of(&scanned, true), Acquisition::Download);
        let mut with_url = scanned.clone();
        with_url.url = Some("https://example.com/m.gguf".into());
        assert_eq!(Acquisition::of(&with_url, false), Acquisition::Download);
    }

    #[test]
    fn the_title_is_never_the_placeholder_or_a_catalogue_sentence() {
        let mut scanned = row(ModelCategory::Gguf, "NVIDIA-Nemotron3-Nano-4B-Q4_K_M");
        scanned.is_custom = true;
        scanned.description = ON_DISK_PLACEHOLDER.to_string();
        assert_eq!(title(&scanned, None), "NVIDIA-Nemotron3-Nano-4B-Q4_K_M");
        scanned.description = "Nemotron 3 Nano 4B".to_string();
        assert_eq!(title(&scanned, None), "Nemotron 3 Nano 4B");

        let mut former = row(ModelCategory::Gguf, "llama-3.2-3b");
        former.description = "Llama 3.2 3B Instruct Q4_K_M (~2 GB, balanced)".to_string();
        assert_eq!(title(&former, None), "llama-3.2-3b");
        assert_eq!(title(&former, Some("Gemma 4 E4B")), "Gemma 4 E4B");

        let embedder = crate::models::domain::embedding_models::EMBEDDINGGEMMA_2.record();
        assert_eq!(title(&embedder, None), "EmbeddingGemma 2");

        let mut voice = row(ModelCategory::TtsKokoro, "af_heart");
        voice.description = "American female — Heart".to_string();
        assert_eq!(title(&voice, None), "American female — Heart");
    }

    /// An added or found file the pairing table lists reads as the table names it; any other
    /// falls back as before, ending at its stem.
    #[test]
    fn a_listed_file_reads_as_the_pairing_table_names_it() {
        let mut added = row(ModelCategory::Gguf, "SmolVLM-256M-Instruct-Q8_0");
        added.is_custom = true;
        added.filename = Some("SmolVLM-256M-Instruct-Q8_0.gguf".to_string());
        added.url = Some("https://huggingface.co/x/resolve/main/SmolVLM.gguf".to_string());
        assert_eq!(title(&added, None), "SmolVLM 256M");

        let mut found = added.clone();
        found.url = None;
        found.description = "Smolvlm 256M Instruct".to_string();
        assert_eq!(title(&found, None), "SmolVLM 256M", "the table's name wins");

        let mut fileless = row(ModelCategory::Gguf, "SmolVLM-256M-Instruct-Q8_0");
        fileless.filename = None;
        assert_eq!(title(&fileless, None), "SmolVLM 256M", "by the name's file");

        let mut unlisted = row(ModelCategory::Gguf, "my-own-finetune-Q4_K_M");
        unlisted.is_custom = true;
        assert_eq!(title(&unlisted, None), "my-own-finetune-Q4_K_M");

        let litert = row(ModelCategory::Litert, "SmolVLM-256M-Instruct-Q8_0.gguf");
        assert_eq!(title(&litert, None), "SmolVLM-256M-Instruct-Q8_0.gguf");
        assert_eq!(title(&added, Some("A pick")), "A pick");
    }

    #[test]
    fn a_file_keeps_the_name_the_scan_would_give_it() {
        assert_eq!(
            catalogue_name(&ModelCategory::Gguf, "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf"),
            "gemma-4-E4B-it-qat-UD-Q4_K_XL"
        );
        assert_eq!(
            catalogue_name(&ModelCategory::Litert, "gemma-4-E2B-it.litertlm"),
            "gemma-4-E2B-it.litertlm"
        );
        assert_eq!(
            catalogue_name(&ModelCategory::Llamafile, "a/b/llama.llamafile"),
            "llama"
        );
        assert_eq!(catalogue_name(&ModelCategory::Gguf, "odd-name"), "odd-name");
    }

    #[test]
    fn every_value_serialises_as_its_wire_name() {
        assert_eq!(
            serde_json::to_value(Provenance::OnDisk).unwrap(),
            serde_json::json!("on_disk")
        );
        assert_eq!(Provenance::OnDisk.as_str(), "on_disk");
        assert_eq!(
            serde_json::to_value(ModelKind::Conversation).unwrap(),
            serde_json::json!("conversation")
        );
        assert_eq!(
            serde_json::to_value(Acquisition::Unavailable).unwrap(),
            serde_json::json!("unavailable")
        );
    }
}
