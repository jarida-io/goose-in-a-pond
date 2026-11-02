//! TTS with a fallback engine, so a primary TTS error never aborts a turn.

use crate::models::ports::voice_output::VoiceOutput;
use anyhow::Result;
use async_trait::async_trait;
use pond_voice::text::SpeechUnit;
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

    async fn synthesize_unit(&self, unit: &SpeechUnit) -> Result<Option<Vec<u8>>> {
        match self.primary.synthesize_unit(unit).await {
            Ok(v) => Ok(v),
            Err(_) => self.fallback.synthesize_unit(unit).await,
        }
    }

    async fn play_audio(&self, audio: Vec<u8>) -> Result<()> {
        match self.primary.play_audio(audio.clone()).await {
            Ok(()) => Ok(()),
            Err(_) => self.fallback.play_audio(audio).await,
        }
    }

    fn begin_utterance(&self) {
        self.primary.begin_utterance();
        self.fallback.begin_utterance();
    }

    fn end_utterance(&self) {
        self.primary.end_utterance();
        self.fallback.end_utterance();
    }

    fn stop_speaking(&self) {
        self.primary.stop_speaking();
    }

    fn is_playing(&self) -> Option<bool> {
        self.primary.is_playing()
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

    /// Records the boundary each unit arrived with.
    struct UnitTts(std::sync::Mutex<Vec<pond_voice::text::Boundary>>);

    #[async_trait]
    impl VoiceOutput for UnitTts {
        async fn speak(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn synthesize_unit(&self, unit: &SpeechUnit) -> Result<Option<Vec<u8>>> {
            self.0.lock().unwrap().push(unit.boundary);
            Ok(Some(vec![1]))
        }
    }

    #[tokio::test]
    async fn the_unit_boundary_reaches_the_engine() {
        use pond_voice::text::Boundary;
        let engine = Arc::new(UnitTts(Default::default()));
        let tts = FallbackVoiceOutput::new(engine.clone(), Arc::new(FailingTts));
        let unit = SpeechUnit::new("First paragraph ends here.", Boundary::Paragraph);
        tts.synthesize_unit(&unit).await.unwrap();
        assert_eq!(*engine.0.lock().unwrap(), vec![Boundary::Paragraph]);
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
