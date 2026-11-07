use anyhow::Result;
use async_trait::async_trait;

/// Early transcript (`Ready`), or `Invalidated` when speech resumed: drop work begun on it.
#[derive(Clone)]
pub enum SpeculativeSignal {
    Ready(String),
    Invalidated,
}

#[async_trait]
pub trait VoiceInput: Send + Sync {
    /// Capture one utterance; `Ok(None)` at EOF or device close, when the caller exits its loop.
    async fn listen(&self) -> Result<Option<String>>;

    /// `listen()` that also reports provisional transcripts; the default never calls back.
    async fn listen_with_speculative(
        &self,
        on_speculative: Box<dyn Fn(SpeculativeSignal) + Send + Sync>,
    ) -> Result<Option<String>> {
        let _ = on_speculative;
        self.listen().await
    }

    /// Terminal prompt label shown before each capture.
    fn prompt(&self) -> &str {
        "> "
    }

    /// Make the next `listen()` transcribe this wake-word capture instead of recording afresh.
    fn prime_with_captured(&self, _wav: Vec<u8>) {}

    /// Make the next `listen()` capture speech already under way since `onset` (a barge-in),
    /// from then rather than from now, and without waiting for speech to start again.
    fn prime_with_onset(&self, _onset: std::time::Instant) {}

    /// The recording behind the last `listen()` that returned words, as 16 kHz mono WAV, handed
    /// over once. `None` from an input that keeps no recording.
    fn take_heard_audio(&self) -> Option<Vec<u8>> {
        None
    }
}

/// Whether the active chat model hears a recording itself; asked as each voice turn starts, since
/// the household can switch models or finish downloading the encoder while the room runs.
#[async_trait]
pub trait ModelHears: Send + Sync {
    async fn model_hears(&self) -> bool;
}
