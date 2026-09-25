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
}
