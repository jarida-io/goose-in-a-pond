use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait VoiceOutput: Send + Sync {
    /// Synthesise and deliver `text` (speak aloud or print).
    async fn speak(&self, text: &str) -> Result<()>;

    /// Synthesize without playing (to overlap with playback); `None` = unsupported, use `speak()`.
    async fn synthesize(&self, _text: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Play `synthesize()` output; returns when playback finishes.
    async fn play_audio(&self, _audio: Vec<u8>) -> Result<()> {
        Ok(())
    }

    /// Clear any prior interrupt; call once per TURN, before its first `speak()`/`play_audio()`.
    /// Clearing per utterance would forget a barge-in during sentence one by sentence two.
    fn begin_utterance(&self) {}

    /// Stop playback now (wake-word barge-in); must be safe when nothing is playing.
    fn stop_speaking(&self) {}

    /// Loop a soft tone during inference until `stop_thinking_tone()`.
    fn start_thinking_tone(&self) {}

    /// Must be safe when no tone is playing.
    fn stop_thinking_tone(&self) {}
}
