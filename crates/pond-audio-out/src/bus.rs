//! Unified audio bus for speech output.

use anyhow::Result;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::models::services::voice::speech_pipeline::SpeechPipeline;
use pond_voice::text::split_text;
use std::sync::Arc;

/// Unified audio bus coordinating speech synthesis and hardware playback.
#[derive(Clone)]
pub struct AudioBus {
    voice: Arc<dyn VoiceOutput>,
}

impl AudioBus {
    pub fn new(voice: Arc<dyn VoiceOutput>) -> Self {
        Self { voice }
    }

    pub fn voice(&self) -> &Arc<dyn VoiceOutput> {
        &self.voice
    }

    pub fn stop_speaking(&self) {
        self.voice.stop_speaking();
    }

    pub fn begin_utterance(&self) {
        self.voice.begin_utterance();
    }

    pub fn end_utterance(&self) {
        self.voice.end_utterance();
    }

    pub fn is_playing(&self) -> Option<bool> {
        self.voice.is_playing()
    }

    fn pipeline(&self) -> SpeechPipeline {
        let voice = self.voice.clone();
        SpeechPipeline::start(
            self.voice.clone(),
            Some(Arc::new(|text: &str| crate::tap::report_spoken(text))),
            Some(Box::new(move || voice.end_utterance())),
        )
    }

    /// Deliver text speech units, optionally waiting for speaker playback to finish.
    pub async fn speak_text(&self, text: &str, wait_for_playback: bool) -> Result<()> {
        let units = split_text(text);
        if units.is_empty() {
            return Ok(());
        }

        self.voice.begin_utterance();
        let mut pipeline = self.pipeline();
        for unit in units {
            if !pipeline.say(unit).await {
                break;
            }
        }

        if wait_for_playback {
            pipeline.finish().await;
        } else {
            tokio::spawn(async move { pipeline.finish().await });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct MockVoice {
        synthesized: Arc<AtomicUsize>,
        played: Arc<AtomicUsize>,
        spoken: Arc<AtomicUsize>,
        utterance_active: Arc<AtomicBool>,
    }

    #[async_trait]
    impl VoiceOutput for MockVoice {
        async fn speak(&self, _text: &str) -> Result<()> {
            self.spoken.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn synthesize(&self, _text: &str) -> Result<Option<Vec<u8>>> {
            self.synthesized.fetch_add(1, Ordering::SeqCst);
            Ok(Some(pond_voice::dsp::encode_wav_pcm16(&[], 24_000)))
        }

        async fn play_audio(&self, _audio: Vec<u8>) -> Result<()> {
            self.played.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn begin_utterance(&self) {
            self.utterance_active.store(true, Ordering::SeqCst);
        }

        fn end_utterance(&self) {
            self.utterance_active.store(false, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn speak_text_splits_clauses_and_plays() {
        let voice = Arc::new(MockVoice {
            synthesized: Arc::new(AtomicUsize::new(0)),
            played: Arc::new(AtomicUsize::new(0)),
            spoken: Arc::new(AtomicUsize::new(0)),
            utterance_active: Arc::new(AtomicBool::new(false)),
        });

        let bus = AudioBus::new(voice.clone());
        let text =
            "First clause of speech, followed by the second clause here. And a third sentence.";
        bus.speak_text(text, true).await.unwrap();

        assert!(voice.synthesized.load(Ordering::SeqCst) >= 2);
        assert_eq!(
            voice.synthesized.load(Ordering::SeqCst),
            voice.played.load(Ordering::SeqCst)
        );
        assert!(!voice.utterance_active.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn what_the_bus_says_reaches_the_spoken_tap() {
        // The only test that sets the spoken tap, so it needs no lock.
        let said = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        crate::set_spoken_tap(Some({
            let said = said.clone();
            Arc::new(move |text: &str| said.lock().unwrap().push(text.to_string()))
        }));
        let voice = Arc::new(MockVoice {
            synthesized: Arc::new(AtomicUsize::new(0)),
            played: Arc::new(AtomicUsize::new(0)),
            spoken: Arc::new(AtomicUsize::new(0)),
            utterance_active: Arc::new(AtomicBool::new(false)),
        });
        AudioBus::new(voice)
            .speak_text("Your timer is done. The tea is ready.", true)
            .await
            .unwrap();
        crate::set_spoken_tap(None);
        let said = said.lock().unwrap().join(" ");
        assert!(
            said.contains("Your timer is done") && said.contains("The tea is ready"),
            "{said}"
        );
    }
}
