//! Changing the speech model while the pond runs: the model the ASR role names is fetched when it
//! is not on disk, and replaces the running one only once it has loaded, so transcription never
//! stops for a choice.

use async_trait::async_trait;

/// How a switch ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechSwitch {
    /// It is already the running model.
    Unchanged,
    /// Now transcribing with it; `fetched` when it had to be downloaded first.
    Switched { model: String, fetched: bool },
    /// The running model stays, and why: the name matched nothing, the fetch or the load failed.
    Kept { reason: String },
    /// A newer choice arrived while this one was being fetched.
    Superseded,
}

#[async_trait]
pub trait SpeechModelSwitch: Send + Sync {
    /// `model` as the ASR role names it: a catalogue name or id, or a file in `models/`.
    async fn switch_to(&self, model: &str) -> SpeechSwitch;
}
