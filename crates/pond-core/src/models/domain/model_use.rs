//! What each model on the pond is used for, and the section a household finds it under.
//!
//! Settings decide who holds a job, since they are what the live paths read: the chat provider
//! is rebuilt from `chat_provider`/`chat_model`, Kokoro speaks `voice_tts_voice`, and the
//! embedder is chosen from `embedding_provider`/`active_embedding_model`. The
//! `model_role_assignments` table is only consulted for a job whose setting is empty, because a
//! row there can outlive a settings change made elsewhere.

use serde::{Deserialize, Serialize};

use super::embedding_models::EmbedderChoice;
use super::model_record::{ModelCategory, ModelRecord, ModelRoleAssignment};
use super::taxonomy::ModelKind;
use crate::user_data::domain::settings::Settings;

/// A job a model on the pond is doing right now.
///
/// The first five are held by a catalogue row; the rest by a file that works for one of them
/// (the picture encoder and the drafter for the conversation model) or by a camera model that
/// has no catalogue row at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelUse {
    /// The model Goose talks and thinks with.
    Chat,
    /// The small model that re-generates empty tool-call arguments.
    Tool,
    /// Speech to text.
    Asr,
    /// The voice Goose speaks in.
    Tts,
    /// Embeddings for memory and tool selection.
    Embedding,
    /// The vision encoder (mmproj) that lets the conversation model read pictures.
    Pictures,
    /// The speculative-decoding drafter attached to the conversation model.
    Drafter,
    /// The detector that names what moved in front of a camera.
    Camera,
    /// Face recognition on camera events.
    Faces,
}

impl ModelUse {
    /// The wire name, as serde writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Tool => "tool",
            Self::Asr => "asr",
            Self::Tts => "tts",
            Self::Embedding => "embedding",
            Self::Pictures => "pictures",
            Self::Drafter => "drafter",
            Self::Camera => "camera",
            Self::Faces => "faces",
        }
    }

    /// The section a model doing this job is listed under.
    pub fn section(self) -> ModelSection {
        match self {
            Self::Chat => ModelSection::Conversation,
            Self::Asr => ModelSection::Listening,
            Self::Tts => ModelSection::Speaking,
            Self::Embedding => ModelSection::Memory,
            Self::Pictures | Self::Camera | Self::Faces => ModelSection::Pictures,
            Self::Tool | Self::Drafter => ModelSection::Other,
        }
    }
}

/// Where a household finds a model, in the order both clients list the sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSection {
    Conversation,
    Listening,
    Speaking,
    Memory,
    Pictures,
    Other,
}

impl ModelSection {
    /// The section for a catalogue row: its first job's, else what the row is for.
    pub fn of(record: &ModelRecord, uses: &[ModelUse]) -> Self {
        if let Some(first) = uses.first() {
            return first.section();
        }
        match record.category {
            ModelCategory::Whisper => Self::Listening,
            ModelCategory::TtsPiper | ModelCategory::TtsKokoro | ModelCategory::TtsHttp => {
                Self::Speaking
            }
            ModelCategory::Embedding => Self::Memory,
            ModelCategory::Gguf
            | ModelCategory::Litert
            | ModelCategory::Llamafile
            | ModelCategory::Ollama => {
                if ModelKind::of(record).is_conversation() {
                    Self::Conversation
                } else {
                    Self::Other
                }
            }
        }
    }
}

/// The catalogue row holding each job, by id; `None` where no row does.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RoleHolders {
    pub chat: Option<String>,
    pub tool: Option<String>,
    pub asr: Option<String>,
    pub tts: Option<String>,
    pub embedding: Option<String>,
}

impl RoleHolders {
    /// Who holds each job among `records`, from `settings`, with `assignments` filling a job
    /// whose setting is empty. A setting that names no row leaves its job unheld rather than
    /// falling back: the live path runs what the setting says, not what the table remembers.
    pub fn resolve(
        records: &[ModelRecord],
        assignments: &[ModelRoleAssignment],
        settings: &Settings,
    ) -> Self {
        let has = |id: &str| records.iter().any(|r| r.id == id);
        let assigned = |role: &str| {
            assignments
                .iter()
                .find(|a| a.role == role && has(&a.model_id))
                .map(|a| a.model_id.clone())
        };
        let named = |pick: &dyn Fn(&ModelRecord) -> bool| {
            records.iter().find(|r| pick(r)).map(|r| r.id.clone())
        };

        let chat_model = settings.chat_model.trim();
        let chat = if chat_model.is_empty() {
            assigned("chat")
        } else {
            let category = ModelCategory::for_chat_model(&settings.chat_provider, chat_model);
            let id = ModelRecord::id_for(&category, chat_model);
            has(&id).then_some(id)
        };

        let tool = match settings.tool_model.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => named(&|r| {
                r.category == ModelCategory::Gguf
                    && (r.name == t || r.filename.as_deref() == Some(t))
            }),
            _ => assigned("tool"),
        };

        let whisper = settings.active_whisper_model.trim();
        let asr = if whisper.is_empty() {
            assigned("asr")
        } else {
            named(&|r| r.category == ModelCategory::Whisper && r.name == whisper)
        };

        let voice = settings.voice_tts_voice.trim();
        let tts_model = settings.active_tts_model.trim();
        let tts = if !voice.is_empty() {
            // A legacy Piper value is the voice's `.onnx` file name.
            named(&|r| {
                r.category.is_tts() && (r.name == voice || r.filename.as_deref() == Some(voice))
            })
        } else if !tts_model.is_empty() {
            named(&|r| r.category.is_tts() && r.name == tts_model)
        } else {
            assigned("tts")
        };

        let embedding = EmbedderChoice::resolve(settings)
            .model()
            .map(|m| m.id())
            .filter(|id| has(id));

        Self {
            chat,
            tool,
            asr,
            tts,
            embedding,
        }
    }

    /// The jobs the row `model_id` holds, in [`ModelUse`] order.
    pub fn uses_of(&self, model_id: &str) -> Vec<ModelUse> {
        [
            (ModelUse::Chat, &self.chat),
            (ModelUse::Tool, &self.tool),
            (ModelUse::Asr, &self.asr),
            (ModelUse::Tts, &self.tts),
            (ModelUse::Embedding, &self.embedding),
        ]
        .into_iter()
        .filter(|(_, holder)| holder.as_deref() == Some(model_id))
        .map(|(use_, _)| use_)
        .collect()
    }
}

/// A model the pond has in use that is not a catalogue row of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelperModel {
    /// Stable within one pond: a file's own id where it has one, else the job's name.
    pub id: String,
    /// What the household reads; `None` when the pond has no name for it but the job's.
    pub title: Option<String>,
    pub uses: Vec<ModelUse>,
    pub section: ModelSection,
    /// The catalogue row this works for, when it works for one.
    pub serves: Option<String>,
}

impl HelperModel {
    /// A helper doing `use_`, listed under that job's section.
    pub fn new(
        id: impl Into<String>,
        title: Option<String>,
        use_: ModelUse,
        serves: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title,
            uses: vec![use_],
            section: use_.section(),
            serves,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::embedding_models::{ALL_MINILM_L6_V2, EMBEDDINGGEMMA_2};

    fn row(category: ModelCategory, name: &str) -> ModelRecord {
        ModelRecord {
            id: ModelRecord::id_for(&category, name),
            category,
            name: name.to_string(),
            filename: Some(format!("{name}.bin")),
            description: String::new(),
            size_mb: 1,
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
            downloaded: true,
            is_custom: false,
        }
    }

    fn catalogue() -> Vec<ModelRecord> {
        vec![
            row(ModelCategory::Gguf, "gemma-4-E4B-it-Q4_K_M"),
            row(ModelCategory::Gguf, "gemma-4-E2B-it-Q4_K_M"),
            row(ModelCategory::Gguf, "qwen-tool-0.5b"),
            row(ModelCategory::Whisper, "base"),
            row(ModelCategory::Whisper, "base.en"),
            row(ModelCategory::TtsKokoro, "af_heart"),
            row(ModelCategory::TtsKokoro, "bf_emma"),
            ALL_MINILM_L6_V2.record(),
            EMBEDDINGGEMMA_2.record(),
        ]
    }

    fn assignment(role: &str, model_id: &str) -> ModelRoleAssignment {
        ModelRoleAssignment {
            role: role.to_string(),
            model_id: model_id.to_string(),
        }
    }

    fn settings() -> Settings {
        Settings {
            chat_provider: "local".into(),
            chat_model: "gemma-4-E4B-it-Q4_K_M".into(),
            tool_model: Some("qwen-tool-0.5b".into()),
            active_whisper_model: "base".into(),
            voice_tts_voice: "bf_emma".into(),
            embedding_provider: "fastembed".into(),
            active_embedding_model: String::new(),
            ..Settings::default()
        }
    }

    #[test]
    fn every_job_is_read_from_settings() {
        let holders = RoleHolders::resolve(&catalogue(), &[], &settings());
        assert_eq!(
            holders,
            RoleHolders {
                chat: Some("gguf/gemma-4-E4B-it-Q4_K_M".into()),
                tool: Some("gguf/qwen-tool-0.5b".into()),
                asr: Some("whisper/base".into()),
                tts: Some("tts_kokoro/bf_emma".into()),
                // No model named: the pond runs MiniLM, so MiniLM holds Memory.
                embedding: Some("embedding/all-MiniLM-L6-v2".into()),
            }
        );
    }

    #[test]
    fn settings_win_over_a_stale_assignment() {
        let stale = [
            assignment("chat", "gguf/gemma-4-E2B-it-Q4_K_M"),
            assignment("asr", "whisper/base.en"),
            assignment("tts", "tts_kokoro/af_heart"),
        ];
        let holders = RoleHolders::resolve(&catalogue(), &stale, &settings());
        assert_eq!(holders.chat.as_deref(), Some("gguf/gemma-4-E4B-it-Q4_K_M"));
        assert_eq!(holders.asr.as_deref(), Some("whisper/base"));
        assert_eq!(holders.tts.as_deref(), Some("tts_kokoro/bf_emma"));
    }

    #[test]
    fn an_empty_setting_falls_back_to_its_assignment() {
        let s = Settings {
            chat_model: String::new(),
            tool_model: None,
            active_whisper_model: String::new(),
            voice_tts_voice: String::new(),
            active_tts_model: String::new(),
            ..settings()
        };
        let assigned = [
            assignment("chat", "gguf/gemma-4-E2B-it-Q4_K_M"),
            assignment("tool", "gguf/qwen-tool-0.5b"),
            assignment("asr", "whisper/base.en"),
            assignment("tts", "tts_kokoro/af_heart"),
        ];
        let holders = RoleHolders::resolve(&catalogue(), &assigned, &s);
        assert_eq!(holders.chat.as_deref(), Some("gguf/gemma-4-E2B-it-Q4_K_M"));
        assert_eq!(holders.tool.as_deref(), Some("gguf/qwen-tool-0.5b"));
        assert_eq!(holders.asr.as_deref(), Some("whisper/base.en"));
        assert_eq!(holders.tts.as_deref(), Some("tts_kokoro/af_heart"));
    }

    #[test]
    fn a_setting_naming_no_row_leaves_the_job_unheld() {
        let s = Settings {
            chat_provider: "ollama".into(),
            chat_model: "llama3".into(),
            active_whisper_model: "large-v3".into(),
            ..settings()
        };
        let assigned = [assignment("chat", "gguf/gemma-4-E4B-it-Q4_K_M")];
        let holders = RoleHolders::resolve(&catalogue(), &assigned, &s);
        assert_eq!(
            holders.chat, None,
            "Ollama runs llama3, not the remembered row"
        );
        assert_eq!(holders.asr, None);
    }

    #[test]
    fn an_assignment_to_a_row_that_is_gone_holds_nothing() {
        let s = Settings {
            chat_model: String::new(),
            ..settings()
        };
        let holders = RoleHolders::resolve(&catalogue(), &[assignment("chat", "gguf/deleted")], &s);
        assert_eq!(holders.chat, None);
    }

    #[test]
    fn memory_follows_the_chosen_embedder_and_is_empty_when_switched_off() {
        let chosen = Settings {
            active_embedding_model: "embeddinggemma-2".into(),
            ..settings()
        };
        assert_eq!(
            RoleHolders::resolve(&catalogue(), &[], &chosen)
                .embedding
                .as_deref(),
            Some("embedding/embeddinggemma-2")
        );
        let off = Settings {
            embedding_provider: "none".into(),
            ..chosen
        };
        assert_eq!(
            RoleHolders::resolve(&catalogue(), &[], &off).embedding,
            None
        );
    }

    #[test]
    fn a_legacy_piper_file_name_still_finds_its_voice() {
        let mut piper = row(ModelCategory::TtsPiper, "lessac");
        piper.filename = Some("en_US-lessac-medium.onnx".into());
        let s = Settings {
            voice_tts_voice: "en_US-lessac-medium.onnx".into(),
            ..settings()
        };
        let holders = RoleHolders::resolve(&[piper], &[], &s);
        assert_eq!(holders.tts.as_deref(), Some("tts_piper/lessac"));
    }

    #[test]
    fn a_row_holding_two_jobs_lists_both_in_order() {
        let holders = RoleHolders {
            chat: Some("gguf/a".into()),
            tool: Some("gguf/a".into()),
            ..RoleHolders::default()
        };
        assert_eq!(holders.uses_of("gguf/a"), [ModelUse::Chat, ModelUse::Tool]);
        assert!(holders.uses_of("gguf/b").is_empty());
    }

    #[test]
    fn a_row_with_no_job_is_listed_by_what_it_is() {
        let section = |category, name: &str| ModelSection::of(&row(category, name), &[]);
        assert_eq!(
            section(ModelCategory::Gguf, "gemma-4-E2B-it"),
            ModelSection::Conversation
        );
        assert_eq!(
            section(ModelCategory::Whisper, "base"),
            ModelSection::Listening
        );
        assert_eq!(
            section(ModelCategory::TtsKokoro, "af_heart"),
            ModelSection::Speaking
        );
        assert_eq!(
            section(ModelCategory::Embedding, "bge-small-en-v1.5"),
            ModelSection::Memory
        );
        assert_eq!(
            section(ModelCategory::Gguf, "mtp-gemma-4-E4B-it"),
            ModelSection::Other,
            "a drafter file is no conversation model"
        );
    }

    #[test]
    fn a_row_with_a_job_is_listed_under_that_job() {
        let tool = row(ModelCategory::Gguf, "gemma-4-E2B-it");
        assert_eq!(
            ModelSection::of(&tool, &[ModelUse::Tool]),
            ModelSection::Other
        );
    }

    #[test]
    fn uses_are_written_as_their_wire_names() {
        for use_ in [
            ModelUse::Chat,
            ModelUse::Tool,
            ModelUse::Asr,
            ModelUse::Tts,
            ModelUse::Embedding,
            ModelUse::Pictures,
            ModelUse::Drafter,
            ModelUse::Camera,
            ModelUse::Faces,
        ] {
            assert_eq!(
                serde_json::to_value(use_).unwrap(),
                serde_json::Value::String(use_.as_str().to_string())
            );
        }
        assert_eq!(
            serde_json::to_value(ModelSection::Pictures).unwrap(),
            "pictures"
        );
    }

    #[test]
    fn a_helper_is_listed_under_its_job() {
        let encoder = HelperModel::new(
            "pictures",
            Some("Picture support".into()),
            ModelUse::Pictures,
            Some("gguf/a".into()),
        );
        assert_eq!(encoder.section, ModelSection::Pictures);
        assert_eq!(encoder.uses, [ModelUse::Pictures]);
        let drafter = HelperModel::new("mtp", None, ModelUse::Drafter, None);
        assert_eq!(drafter.section, ModelSection::Other);
    }
}
