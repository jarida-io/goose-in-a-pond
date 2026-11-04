//! Unified audio bus for streaming and batch speech output.

use anyhow::Result;
use futures::stream::BoxStream;
use futures::StreamExt;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::models::services::voice::speech_pipeline::{SpeechPipeline, SENTENCE_HOLD};
use pond_core::shared::domain::agent::AgentStreamEvent;
use pond_voice::text::{split_text, ReplySplitter};
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
            None,
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

    /// Stream LLM tokens directly into pipelined clause synthesis and playback.
    ///
    /// When `wait_for_playback` is false, returns `Ok(full_text)` as soon as token generation
    /// completes, while speech playback finishes in the background.
    pub async fn speak_stream(
        &self,
        mut stream: BoxStream<'static, Result<AgentStreamEvent>>,
        wait_for_playback: bool,
    ) -> Result<String> {
        self.voice.begin_utterance();
        let mut pipeline = self.pipeline();

        let mut full_text = String::new();
        let mut splitter = ReplySplitter::new();

        loop {
            let event = if splitter.pending().trim().is_empty() {
                stream.next().await
            } else {
                match tokio::time::timeout(SENTENCE_HOLD, stream.next()).await {
                    Ok(event) => event,
                    Err(_) => {
                        for unit in splitter.pause(pipeline.pace()) {
                            pipeline.say(unit).await;
                        }
                        continue;
                    }
                }
            };
            let Some(event_result) = event else {
                break;
            };
            let units = match event_result? {
                AgentStreamEvent::Text { content } => {
                    full_text.push_str(&content);
                    splitter.push(&content, pipeline.pace())
                }
                AgentStreamEvent::ToolCall { .. } => splitter.pause(pipeline.pace()),
                AgentStreamEvent::Done { .. } => break,
                AgentStreamEvent::Error { content } => {
                    return Err(anyhow::anyhow!("agent stream error: {content}"));
                }
                _ => Vec::new(),
            };
            for unit in units {
                if !pipeline.say(unit).await {
                    break;
                }
            }
        }

        for unit in splitter.finish() {
            pipeline.say(unit).await;
        }

        if wait_for_playback {
            pipeline.finish().await;
        } else {
            tokio::spawn(async move { pipeline.finish().await });
        }

        Ok(full_text)
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

    #[tokio::test]
    async fn speak_stream_pipelines_tokens_incrementally() {
        let voice = Arc::new(MockVoice {
            synthesized: Arc::new(AtomicUsize::new(0)),
            played: Arc::new(AtomicUsize::new(0)),
            spoken: Arc::new(AtomicUsize::new(0)),
            utterance_active: Arc::new(AtomicBool::new(false)),
        });

        let bus = AudioBus::new(voice.clone());
        let tokens = vec![
            Ok(AgentStreamEvent::Text {
                content: "Hello there Jerry, ".to_string(),
            }),
            Ok(AgentStreamEvent::Text {
                content: "the weather today is sunny. ".to_string(),
            }),
            Ok(AgentStreamEvent::Text {
                content: "Have a great day.".to_string(),
            }),
            Ok(AgentStreamEvent::Done {
                session_id: "s1".to_string(),
                model_role: "task".to_string(),
                usage: None,
                stats: None,
            }),
        ];
        let stream = futures::stream::iter(tokens).boxed();

        let result = bus.speak_stream(stream, true).await.unwrap();
        assert_eq!(
            result,
            "Hello there Jerry, the weather today is sunny. Have a great day."
        );
        assert!(voice.synthesized.load(Ordering::SeqCst) >= 2);
    }
}
