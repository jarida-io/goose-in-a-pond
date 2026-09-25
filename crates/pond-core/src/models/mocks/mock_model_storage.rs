use std::path::PathBuf;

use crate::models::domain::model_record::{BinaryRecord, ModelCategory, ModelRecord};
use crate::models::ports::model_storage::ModelStorage;

/// Mock storage with three configurable presence modes.
pub enum MockModelStorage {
    /// Files are always considered present — `is_present` always returns `true`.
    AlwaysPresent,
    /// Files are never present — `is_present` always returns `false`.
    NeverPresent,
    /// Paths are resolved under `base`; `is_present` does a real filesystem check.
    FileSystemBacked { base: PathBuf },
}

impl MockModelStorage {
    pub fn always_present() -> Self {
        Self::AlwaysPresent
    }

    pub fn never_present() -> Self {
        Self::NeverPresent
    }

    pub fn file_system_backed(base: PathBuf) -> Self {
        Self::FileSystemBacked { base }
    }
}

impl ModelStorage for MockModelStorage {
    fn path_for(&self, record: &ModelRecord) -> Option<PathBuf> {
        let filename = record.filename.as_deref()?;
        match self {
            Self::AlwaysPresent => {
                // Return a fake path — `is_present` is overridden to return true anyway.
                Some(PathBuf::from(format!("/mock/always/{filename}")))
            }
            Self::NeverPresent => {
                // Return a fake path that won't exist.
                Some(PathBuf::from(format!("/mock/never/{filename}")))
            }
            Self::FileSystemBacked { base } => {
                let subdir = match record.category {
                    ModelCategory::Whisper => "models",
                    ModelCategory::Llamafile => "models/llm",
                    ModelCategory::Gguf => "models/gguf",
                    ModelCategory::TtsPiper => "models/tts",
                    ModelCategory::TtsKokoro => "models/kokoro/voices",
                    ModelCategory::Ollama | ModelCategory::TtsHttp | ModelCategory::Embedding => {
                        return None
                    }
                };
                Some(base.join(subdir).join(filename))
            }
        }
    }

    fn is_present(&self, record: &ModelRecord) -> bool {
        match self {
            Self::AlwaysPresent => true,
            Self::NeverPresent => false,
            Self::FileSystemBacked { .. } => {
                self.path_for(record).map(|p| p.exists()).unwrap_or(false)
            }
        }
    }

    fn binary_path(&self, record: &BinaryRecord) -> PathBuf {
        match self {
            Self::AlwaysPresent | Self::NeverPresent => {
                PathBuf::from(format!("/mock/bin/{}", record.name))
            }
            Self::FileSystemBacked { base } => base.join("bin").join(&record.name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::model_record::ModelCategory;
    use tempfile::tempdir;

    fn gguf_record(name: &str) -> ModelRecord {
        ModelRecord {
            id: format!("gguf/{name}"),
            category: ModelCategory::Gguf,
            name: name.to_string(),
            filename: Some(format!("{name}.gguf")),
            description: "test".to_string(),
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
    fn always_present_is_present() {
        let storage = MockModelStorage::AlwaysPresent;
        assert!(storage.is_present(&gguf_record("llama3")));
    }

    #[test]
    fn never_present_is_not_present() {
        let storage = MockModelStorage::NeverPresent;
        assert!(!storage.is_present(&gguf_record("llama3")));
    }

    #[test]
    fn file_system_backed_checks_disk() {
        let tmp = tempdir().unwrap();
        let storage = MockModelStorage::FileSystemBacked {
            base: tmp.path().to_path_buf(),
        };

        let record = gguf_record("llama3");
        assert!(!storage.is_present(&record), "file not yet created");

        let path = storage.path_for(&record).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"model").unwrap();

        assert!(storage.is_present(&record), "file now exists on disk");
    }

    #[test]
    fn ollama_has_no_path() {
        let storage = MockModelStorage::NeverPresent;
        let mut record = gguf_record("llama3");
        record.category = ModelCategory::Ollama;
        record.filename = None;
        assert!(storage.path_for(&record).is_none());
    }
}
