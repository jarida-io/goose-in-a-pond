//! Where the pond keeps the ear's models, mirroring pond-server's `default_data_dir`, for tools and
//! for tests that use a model only when the pond has already fetched it.

use std::path::{Path, PathBuf};

/// `POND_DATA_DIR`, else the platform data directory's `goose-in-a-pond`.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("POND_DATA_DIR") {
        return Some(PathBuf::from(d));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.map(|h| h.join(".local/share")))
    };
    base.map(|b| b.join("goose-in-a-pond"))
}

/// Silero VAD v5, where the voice child already downloads it.
pub fn silero(data_dir: &Path) -> PathBuf {
    data_dir
        .join("models")
        .join("silero")
        .join("silero_vad.onnx")
}

/// WeSpeaker ResNet34-LM, where the pond will fetch it for Voice Match.
pub fn speaker(data_dir: &Path) -> PathBuf {
    data_dir
        .join("models")
        .join("voice-match")
        .join("resnet34_LM.onnx")
}
