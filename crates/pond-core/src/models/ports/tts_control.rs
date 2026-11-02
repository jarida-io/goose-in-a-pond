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

/// Where the speech engine runs, for the TTS status.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TtsAcceleration {
    /// `voice_tts_accelerator` as applied: `auto`, `cpu` or `gpu`.
    pub setting: String,
    /// `cpu` or `cuda` once the engine has loaded.
    pub device: Option<String>,
    /// Why the engine runs there.
    pub reason: Option<String>,
    /// How long the last load took, and the GPU warm-up within it.
    pub load_ms: Option<u64>,
    pub warm_ms: Option<u64>,
}

#[async_trait]
pub trait TtsControl: Send + Sync {
    /// Must be safe mid-speech: the settings screen calls it on every slider release.
    async fn apply(&self, voice: &str, speed: f32, quality: &str) -> Result<TtsApplied>;

    /// Voices usable without a download (the catalogue also lists ones that need one).
    async fn installed_voices(&self) -> Vec<String>;

    /// `text` as a WAV in `voice`, at the current pace and quality, leaving the selected voice
    /// as it is: a sample must not change how a running turn sounds. An uninstalled voice is an
    /// error, never a download.
    async fn synthesize_in_voice(&self, voice: &str, text: &str) -> Result<Vec<u8>>;

    /// Drop the speech engine from RAM to reclaim memory.
    async fn unload(&self) -> Result<()> {
        Ok(())
    }

    /// Whether the speech engine is currently resident in RAM.
    async fn is_loaded(&self) -> bool {
        false
    }

    /// Prewarm the speech engine into RAM.
    async fn prewarm(&self) -> Result<()> {
        Ok(())
    }
    /// Choose where the engine runs (`auto`, `cpu`, `gpu`); `true` when that drops the loaded
    /// session, so the next utterance reloads it there.
    async fn set_accelerator(&self, _setting: &str) -> Result<bool> {
        Ok(false)
    }
    /// Where the engine runs and why; `None` for an engine with one place to run.
    async fn acceleration(&self) -> Option<TtsAcceleration> {
        None
    }
}
