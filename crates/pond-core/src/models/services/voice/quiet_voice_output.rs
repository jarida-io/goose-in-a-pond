//! A [`VoiceOutput`] that keeps other audio paused while it makes a sound (see [`super::quiet`]).

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use super::quiet::{Quiet, QuietHold};
use crate::models::ports::voice_output::VoiceOutput;
use pond_voice::text::SpeechUnit;

/// How long a sound waits for other audio to pause before it plays anyway.
pub const WAIT_FOR_QUIET: Duration = Duration::from_millis(1500);

pub struct QuietVoiceOutput {
    inner: Arc<dyn VoiceOutput>,
    quiet: Arc<Quiet>,
    /// From `begin_utterance` to `end_utterance`: the whole turn, the gaps between sentences too.
    turn: Mutex<Option<QuietHold>>,
    tone: Arc<Mutex<Tone>>,
}

#[derive(Default)]
struct Tone {
    /// Moved on by every start and stop, so a start still waiting for quiet can tell it is stale.
    generation: u64,
    hold: Option<QuietHold>,
}

impl QuietVoiceOutput {
    pub fn new(inner: Arc<dyn VoiceOutput>, quiet: Arc<Quiet>) -> Self {
        Self {
            inner,
            quiet,
            turn: Mutex::new(None),
            tone: Arc::new(Mutex::new(Tone::default())),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[async_trait]
impl VoiceOutput for QuietVoiceOutput {
    async fn speak(&self, text: &str) -> Result<()> {
        let _hold = self.quiet.hold();
        self.quiet.until_quiet(WAIT_FOR_QUIET).await;
        self.inner.speak(text).await
    }

    async fn synthesize(&self, text: &str) -> Result<Option<Vec<u8>>> {
        self.inner.synthesize(text).await
    }

    async fn synthesize_unit(&self, unit: &SpeechUnit) -> Result<Option<Vec<u8>>> {
        self.inner.synthesize_unit(unit).await
    }

    async fn play_audio(&self, audio: Vec<u8>) -> Result<()> {
        let _hold = self.quiet.hold();
        self.quiet.until_quiet(WAIT_FOR_QUIET).await;
        self.inner.play_audio(audio).await
    }

    fn begin_utterance(&self) {
        self.inner.begin_utterance();
        // Taken before inference, so the pause is done by the time there is anything to say.
        *lock(&self.turn) = Some(self.quiet.hold());
    }

    fn end_utterance(&self) {
        self.inner.end_utterance();
        lock(&self.turn).take();
    }

    fn stop_speaking(&self) {
        self.inner.stop_speaking();
    }

    fn is_playing(&self) -> Option<bool> {
        self.inner.is_playing()
    }

    fn start_thinking_tone(&self) {
        let generation = {
            let mut tone = lock(&self.tone);
            tone.generation += 1;
            tone.hold = Some(self.quiet.hold());
            tone.generation
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            // Nowhere to wait: better the tone now than none.
            self.inner.start_thinking_tone();
            return;
        };
        let (inner, quiet, tone) = (self.inner.clone(), self.quiet.clone(), self.tone.clone());
        runtime.spawn(async move {
            quiet.until_quiet(WAIT_FOR_QUIET).await;
            let tone = lock(&tone);
            if tone.generation == generation && tone.hold.is_some() {
                inner.start_thinking_tone();
            }
        });
    }

    fn stop_thinking_tone(&self) {
        let mut tone = lock(&self.tone);
        tone.generation += 1;
        self.inner.stop_thinking_tone();
        tone.hold = None;
    }
}

#[cfg(test)]
mod tests {
    use super::super::quiet::GRACE;
    use super::*;
    use crate::models::ports::audio_focus::{AudioFocus, Paused};

    type Log = Arc<Mutex<Vec<&'static str>>>;

    /// Spotify playing, taking `pause_takes` to pause, writing to the same log as the voice.
    struct Service {
        log: Log,
        pause_takes: Duration,
    }

    #[async_trait]
    impl AudioFocus for Service {
        async fn pause(&self) -> Option<Paused> {
            tokio::time::sleep(self.pause_takes).await;
            lock(&self.log).push("pause");
            Some(Paused {
                device: None,
                item: None,
            })
        }
        async fn resume(&self, _paused: Paused) {
            lock(&self.log).push("resume");
        }
    }

    struct Voice(Log);

    #[async_trait]
    impl VoiceOutput for Voice {
        async fn speak(&self, _text: &str) -> Result<()> {
            lock(&self.0).push("speak");
            Ok(())
        }
        async fn synthesize(&self, _text: &str) -> Result<Option<Vec<u8>>> {
            Ok(Some(vec![0]))
        }
        async fn synthesize_unit(&self, unit: &SpeechUnit) -> Result<Option<Vec<u8>>> {
            if unit.boundary == pond_voice::text::Boundary::Paragraph {
                lock(&self.0).push("paragraph");
            }
            Ok(Some(vec![0]))
        }
        async fn play_audio(&self, _audio: Vec<u8>) -> Result<()> {
            lock(&self.0).push("play");
            Ok(())
        }
        fn start_thinking_tone(&self) {
            lock(&self.0).push("tone on");
        }
        fn stop_thinking_tone(&self) {
            lock(&self.0).push("tone off");
        }
    }

    fn voice(pause_takes: Duration) -> (QuietVoiceOutput, Log) {
        let log: Log = Arc::default();
        let quiet = Quiet::start(
            Arc::new(Service {
                log: log.clone(),
                pause_takes,
            }),
            GRACE,
        );
        (
            QuietVoiceOutput::new(Arc::new(Voice(log.clone())), quiet),
            log,
        )
    }

    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    fn said(log: &Log) -> Vec<&'static str> {
        lock(log).clone()
    }

    #[tokio::test(start_paused = true)]
    async fn a_units_boundary_passes_through() {
        let (out, log) = voice(Duration::ZERO);
        let unit = SpeechUnit::new(
            "The end of a paragraph.",
            pond_voice::text::Boundary::Paragraph,
        );
        out.synthesize_unit(&unit).await.unwrap();
        assert_eq!(said(&log), ["paragraph"]);
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_said_over_the_music() {
        let (out, log) = voice(Duration::from_millis(300));
        out.speak("Hi Jerry, ready to take your first request.")
            .await
            .unwrap();
        assert_eq!(said(&log), ["pause", "speak"]);

        tokio::time::sleep(GRACE + Duration::from_millis(100)).await;
        settle().await;
        assert_eq!(said(&log), ["pause", "speak", "resume"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_pauses_once_however_long_between_its_sentences() {
        let (out, log) = voice(Duration::ZERO);
        out.begin_utterance();
        out.speak("The first sentence.").await.unwrap();
        // A slow model: far longer than the grace before the next sentence is ready.
        tokio::time::sleep(GRACE * 3).await;
        let audio = out.synthesize("The second.").await.unwrap().unwrap();
        out.play_audio(audio).await.unwrap();
        tokio::time::sleep(GRACE * 3).await;
        settle().await;
        assert_eq!(said(&log), ["pause", "speak", "play"], "came back mid-turn");

        out.end_utterance();
        tokio::time::sleep(GRACE + Duration::from_millis(100)).await;
        settle().await;
        assert_eq!(said(&log), ["pause", "speak", "play", "resume"]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_thinking_tone_waits_for_the_pause() {
        let (out, log) = voice(Duration::from_millis(300));
        out.start_thinking_tone();
        settle().await;
        assert_eq!(
            said(&log),
            Vec::<&str>::new(),
            "the tone started over the music"
        );

        tokio::time::sleep(Duration::from_millis(400)).await;
        settle().await;
        assert_eq!(said(&log), ["pause", "tone on"]);

        out.stop_thinking_tone();
        tokio::time::sleep(GRACE + Duration::from_millis(100)).await;
        settle().await;
        assert_eq!(said(&log), ["pause", "tone on", "tone off", "resume"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_tone_stopped_while_waiting_never_starts() {
        let (out, log) = voice(Duration::from_millis(300));
        out.start_thinking_tone();
        settle().await;
        // The first sentence arrived before the pause did.
        out.stop_thinking_tone();
        tokio::time::sleep(Duration::from_secs(1)).await;
        settle().await;
        assert!(
            !said(&log).contains(&"tone on"),
            "a stale start played the tone after its stop: {:?}",
            said(&log)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn synthesising_alone_pauses_nothing() {
        let (out, log) = voice(Duration::ZERO);
        out.synthesize("Not played yet.").await.unwrap();
        tokio::time::sleep(GRACE * 2).await;
        settle().await;
        assert_eq!(said(&log), Vec::<&str>::new());
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_with_nothing_said_still_gives_the_music_back() {
        let (out, log) = voice(Duration::ZERO);
        out.begin_utterance();
        settle().await;
        out.end_utterance();
        tokio::time::sleep(GRACE + Duration::from_millis(100)).await;
        settle().await;
        assert_eq!(said(&log), ["pause", "resume"]);
    }
}
