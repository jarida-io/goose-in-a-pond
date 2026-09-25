//! Live TTS reconfiguration, no restart needed; implementations do any downloads it implies.
//! Kept out of `VoiceOutput` so the speaking path never depends on it.

use anyhow::Result;
use async_trait::async_trait;

/// What applying settings actually did; a quality change can fetch up to 326 MB and reload.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TtsApplied {
    pub voice: String,
    /// Pace multiplier × 1000.
    pub speed_milli: u32,
    pub quality: String,
    pub downloaded_voice: bool,
    pub downloaded_weights: bool,
    /// The session was dropped and will reload on the next utterance.
    pub engine_reloaded: bool,
}

#[async_trait]
pub trait TtsControl: Send + Sync {
    /// Must be safe mid-speech: the settings screen calls it on every slider release.
    async fn apply(&self, voice: &str, speed: f32, quality: &str) -> Result<TtsApplied>;

    /// Voices usable without a download (the catalogue also lists ones that need one).
    async fn installed_voices(&self) -> Vec<String>;
}
