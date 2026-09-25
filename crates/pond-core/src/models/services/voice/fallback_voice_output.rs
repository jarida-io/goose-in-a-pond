//! TTS with a fallback engine, so a primary TTS error never aborts a turn.

use crate::models::ports::voice_output::VoiceOutput;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

pub struct FallbackVoiceOutput {
    primary: Arc<dyn VoiceOutput>,
    fallback: Arc<dyn VoiceOutput>,
}

impl FallbackVoiceOutput {
    pub fn new(primary: Arc<dyn VoiceOutput>, fallback: Arc<dyn VoiceOutput>) -> Self {
        Self { primary, fallback }
    }
}

#[async_trait]
impl VoiceOutput for FallbackVoiceOutput {
    async fn speak(&self, text: &str) -> Result<()> {
        match self.primary.speak(text).await {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::info!("Primary TTS unavailable ({}), using fallback.", e);
                self.fallback.speak(text).await
            }
        }
    }

    async fn synthesize(&self, text: &str) -> Result<Option<Vec<u8>>> {
        match self.primary.synthesize(text).await {
            Ok(v) => Ok(v),
            Err(_) => self.fallback.synthesize(text).await,
        }
    }

    async fn play_audio(&self, audio: Vec<u8>) -> Result<()> {
        match self.primary.play_audio(audio.clone()).await {
            Ok(()) => Ok(()),
            Err(_) => self.fallback.play_audio(audio).await,
        }
    }

    fn stop_speaking(&self) {
        self.primary.stop_speaking();
    }

    fn start_thinking_tone(&self) {
        self.primary.start_thinking_tone();
    }

    fn stop_thinking_tone(&self) {
        self.primary.stop_thinking_tone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct SucceedingTts(Arc<AtomicBool>);
    struct FailingTts;

    #[async_trait]
    impl VoiceOutput for SucceedingTts {
        async fn speak(&self, _: &str) -> Result<()> {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[async_trait]
    impl VoiceOutput for FailingTts {
        async fn speak(&self, _: &str) -> Result<()> {
            Err(anyhow::anyhow!("TTS server offline"))
        }
    }

    #[tokio::test]
    async fn uses_primary_when_it_succeeds() {
        let primary_called = Arc::new(AtomicBool::new(false));
        let fallback_called = Arc::new(AtomicBool::new(false));

        let tts = FallbackVoiceOutput::new(
            Arc::new(SucceedingTts(primary_called.clone())),
            Arc::new(SucceedingTts(fallback_called.clone())),
        );

        tts.speak("hello").await.unwrap();
        assert!(
            primary_called.load(Ordering::SeqCst),
            "primary should have been called"
        );
        assert!(
            !fallback_called.load(Ordering::SeqCst),
            "fallback should NOT have been called"
        );
    }

    #[tokio::test]
    async fn falls_back_when_primary_fails() {
        let fallback_called = Arc::new(AtomicBool::new(false));

        let tts = FallbackVoiceOutput::new(
            Arc::new(FailingTts),
            Arc::new(SucceedingTts(fallback_called.clone())),
        );

        tts.speak("hello").await.unwrap();
        assert!(
            fallback_called.load(Ordering::SeqCst),
            "fallback should have been called"
        );
    }

    #[tokio::test]
    async fn returns_error_when_both_fail() {
        let tts = FallbackVoiceOutput::new(Arc::new(FailingTts), Arc::new(FailingTts));
        let err = tts.speak("hello").await.unwrap_err();
        assert!(err.to_string().contains("TTS server offline"));
    }
}
