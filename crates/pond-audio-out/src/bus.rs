//! Unified audio bus for speech output.

use anyhow::Result;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_voice::text::split_speech_units;
use std::sync::Arc;
use tokio::sync::mpsc;

enum AudioItem {
    Wav(Vec<u8>),
    Fallback(String),
}

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

    /// Deliver text speech units, optionally waiting for speaker playback to finish.
    pub async fn speak_text(&self, text: &str, wait_for_playback: bool) -> Result<()> {
        let (units, remainder) = split_speech_units(text, true);
        let mut all_units = units;
        let remainder = remainder.trim();
        if !remainder.is_empty() {
            all_units.push(remainder.to_string());
        }
        if all_units.is_empty() {
            return Ok(());
        }

        self.voice.begin_utterance();

        let (speech_tx, mut speech_rx) = mpsc::channel::<String>(8);
        let (audio_tx, mut audio_rx) = mpsc::channel::<AudioItem>(8);

        let voice_synth = self.voice.clone();
        let synth_handle = tokio::spawn(async move {
            while let Some(clause) = speech_rx.recv().await {
                if clause.is_empty() {
                    continue;
                }
                match voice_synth.synthesize(&clause).await {
                    Ok(Some(wav)) if !wav.is_empty() => {
                        if audio_tx.send(AudioItem::Wav(wav)).await.is_err() {
                            break;
                        }
                    }
                    _ => {
                        if audio_tx.send(AudioItem::Fallback(clause)).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let voice_play = self.voice.clone();
        let playback_handle = tokio::spawn(async move {
            while let Some(item) = audio_rx.recv().await {
                match item {
                    AudioItem::Wav(wav) => {
                        if let Err(e) = voice_play.play_audio(wav).await {
                            tracing::warn!("AudioBus playback failed: {e}");
                        }
                    }
                    AudioItem::Fallback(text) => {
                        if let Err(e) = voice_play.speak(&text).await {
                            tracing::warn!("AudioBus fallback speak failed: {e}");
                        }
                    }
                }
            }
            voice_play.end_utterance();
        });

        for unit in all_units {
            let clean = unit.trim().to_string();
            if !clean.is_empty() && speech_tx.send(clean).await.is_err() {
                break;
            }
        }
        drop(speech_tx);

        if wait_for_playback {
            let _ = synth_handle.await;
            let _ = playback_handle.await;
        } else {
            tokio::spawn(async move {
                let _ = synth_handle.await;
                let _ = playback_handle.await;
            });
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
}
