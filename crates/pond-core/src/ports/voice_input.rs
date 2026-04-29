//! Driving Port: VoiceInput
//!
//! Abstracts text/voice input acquisition so the workflow loop does not
//! depend directly on stdin, a microphone, or any specific ASR backend.
//!
//! `listen()` blocks until a complete utterance is available, then returns
//! the transcribed text.  Returns `Ok(None)` on EOF / end-of-stream to
//! signal that the loop should terminate cleanly.
//!
//! `listen_with_audio()` returns both the transcript and the raw WAV bytes.
//! The default implementation delegates to `listen()` and returns an empty
//! byte vector.  Microphone-backed implementations override this so callers
//! can run speaker identification on the captured audio without re-recording.

use anyhow::Result;
use async_trait::async_trait;

/// Driving Port: VoiceInput
#[async_trait]
pub trait VoiceInput: Send + Sync {
    /// Capture one utterance and return its text.
    ///
    /// Returns `Ok(None)` when the input stream is exhausted (EOF / device
    /// closed) — the caller should exit its loop cleanly.
    async fn listen(&self) -> Result<Option<String>>;

    /// Capture one utterance and return `(transcript, wav_bytes)`.
    ///
    /// The default implementation calls `listen()` and returns an empty
    /// `Vec<u8>` for the audio.  Microphone-backed implementations should
    /// override this to return the actual WAV bytes.
    async fn listen_with_audio(&self) -> Result<Option<(String, Vec<u8>)>> {
        match self.listen().await? {
            Some(text) => Ok(Some((text, Vec::new()))),
            None => Ok(None),
        }
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
