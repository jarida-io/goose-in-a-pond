//! Driving Port: VoiceInput
//!
//! Abstracts text/voice input acquisition so the workflow loop does not
//! depend directly on stdin, a microphone, or any specific ASR backend.
//!
//! `listen()` blocks until a complete utterance is available, then returns
//! the transcribed text.  Returns `Ok(None)` on EOF / end-of-stream to
//! signal that the loop should terminate cleanly.

use anyhow::Result;
use async_trait::async_trait;

/// Q2-26: signal emitted by `listen_with_speculative` before the final
/// transcript is confirmed.
///
/// Implementations that overlap ASR with the silence-confirmation wait
/// (e.g. `WhisperRsInput`) fire `Ready` the moment a provisional transcript
/// is available, and `Invalidated` if speech resumes afterward (the
/// provisional transcript covered a too-short clip). Callers may use
/// `Ready` to speculatively start downstream work (e.g. the LLM call) and
/// must cancel/discard that work on `Invalidated`.
#[derive(Clone)]
pub enum SpeculativeSignal {
    Ready(String),
    Invalidated,
}

/// Driving Port: VoiceInput
#[async_trait]
pub trait VoiceInput: Send + Sync {
    /// Capture one utterance and return its text.
    ///
    /// Returns `Ok(None)` when the input stream is exhausted (EOF / device
    /// closed) — the caller should exit its loop cleanly.
    async fn listen(&self) -> Result<Option<String>>;

    /// Like `listen()`, but invokes `on_speculative` with provisional
    /// transcripts as they become available, before the final transcript is
    /// confirmed (Q2-26). Implementations that don't support the overlap
    /// just call `listen()` and never invoke the callback.
    async fn listen_with_speculative(
        &self,
        on_speculative: Box<dyn Fn(SpeculativeSignal) + Send + Sync>,
    ) -> Result<Option<String>> {
        let _ = on_speculative;
        self.listen().await
    }

    /// Short label shown in the terminal prompt before each capture.
    ///
    /// Stdin implementations return `"> "`.
    /// Voice implementations may return `"🎤 "` or similar.
    fn prompt(&self) -> &str {
        "> "
    }

    /// Pre-load captured audio that `listen()` should transcribe instead of
    /// recording a fresh microphone clip.
    ///
    /// Call this before `listen()` when the wake-word detector has already
    /// recorded the user's command in the same breath as the wake word.
    /// The default implementation is a no-op — implementors that support
    /// audio hand-off (e.g. `WhisperInput`) override this method.
    fn prime_with_captured(&self, _wav: Vec<u8>) {}
}
