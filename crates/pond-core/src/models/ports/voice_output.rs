use anyhow::Result;
use async_trait::async_trait;
use pond_voice::text::SpeechUnit;

#[async_trait]
pub trait VoiceOutput: Send + Sync {
    /// Synthesise and deliver `text` (speak aloud or print).
    async fn speak(&self, text: &str) -> Result<()>;

    /// Synthesize without playing (to overlap with playback); `None` = unsupported, use `speak()`.
    async fn synthesize(&self, _text: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    /// [`Self::synthesize`] for one unit of a longer text, which says whether more follows it so
    /// an output can end the unit in a gap or at rest. Wrappers must forward it.
    async fn synthesize_unit(&self, unit: &SpeechUnit) -> Result<Option<Vec<u8>>> {
        self.synthesize(&unit.text).await
    }

    /// Play `synthesize()` output; returns when playback finishes.
    async fn play_audio(&self, _audio: Vec<u8>) -> Result<()> {
        Ok(())
    }

    /// Clear any prior interrupt; call once per TURN, before its first `speak()`/`play_audio()`.
    /// Clearing per utterance would forget a barge-in during sentence one by sentence two.
    fn begin_utterance(&self) {}

    /// The turn `begin_utterance` began is over, however it ended.
    fn end_utterance(&self) {}

    /// Stop playback now (wake-word barge-in); must be safe when nothing is playing.
    fn stop_speaking(&self) {}

    /// Whether audio is coming out right now; `None` when this output cannot tell.
    /// Barge-in listens only while something plays, not across a tool call between sentences.
    fn is_playing(&self) -> Option<bool> {
        None
    }

    /// Loop a soft tone during inference until `stop_thinking_tone()`.
    fn start_thinking_tone(&self) {}

    /// Must be safe when no tone is playing.
    fn stop_thinking_tone(&self) {}
}
