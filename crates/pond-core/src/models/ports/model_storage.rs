//! Implementations are the single owner of the on-disk layout under `data_dir`.

use std::path::PathBuf;

use crate::models::domain::model_record::{BinaryRecord, ModelRecord};

pub trait ModelStorage: Send + Sync {
    /// `None` for file-less categories (`ModelCategory::Ollama`, `ModelCategory::TtsHttp`).
    fn path_for(&self, record: &ModelRecord) -> Option<PathBuf>;

    fn is_present(&self, record: &ModelRecord) -> bool {
        self.path_for(record).map(|p| p.exists()).unwrap_or(false)
    }

    /// On-disk path for a tool binary (e.g. `whisper-server`, `piper`).
    fn binary_path(&self, record: &BinaryRecord) -> PathBuf;

    fn binary_present(&self, record: &BinaryRecord) -> bool {
        self.binary_path(record).exists()
    }
}
