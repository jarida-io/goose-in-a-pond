//! One reply's speech: units are synthesised one at a time and played in order. An engine slower
//! than its own speech would stop between units, so a sound that starts from silence is held
//! until the units still owed can follow it without a gap.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

use pond_voice::dsp::wav_duration_secs;
use pond_voice::text::{estimate_tokens, SpeechUnit};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::models::ports::voice_output::VoiceOutput;

/// Hears the text of each unit as it goes to synthesis.
pub type SpokenObserver = Arc<dyn Fn(&str) + Send + Sync>;

/// The longest a sound is held: past this a rate measured on one odd pass is not believed.
const LONGEST_HOLD: Duration = Duration::from_secs(20);

enum AudioItem {
    Wav {
        wav: Vec<u8>,
        millis: u64,
    },
    /// Synthesis gave nothing: the output speaks the text its own way.
    Fallback(String),
}

/// What the two tasks know about the speech still to come.
#[derive(Default)]
struct Meter {
    /// Estimated tokens of each unit handed over and not yet synthesised, oldest first.
    owed: Mutex<VecDeque<usize>>,
    /// When the oldest of `owed` went to the engine.
    synth_began: Mutex<Option<Instant>>,
    /// Audio synthesised and not yet started, in ms.
    ready_ms: AtomicU64,
    /// Measured seconds of synthesis and of speech per token, as `f32` bits; 0 until measured.
    synth_per_token: AtomicU32,
    speech_per_token: AtomicU32,
}

impl Meter {
    fn synthesised(&self, tokens: usize, took_secs: f32, audio_ms: u64) {
        self.ready_ms.fetch_add(audio_ms, Ordering::Relaxed);
        self.owed.lock().unwrap().pop_front();
        *self.synth_began.lock().unwrap() = None;
        if tokens > 0 && audio_ms > 0 {
            let per_token = |secs: f32| (secs / tokens as f32).to_bits();
            self.synth_per_token
                .store(per_token(took_secs), Ordering::Relaxed);
            self.speech_per_token
                .store(per_token(audio_ms as f32 / 1000.0), Ordering::Relaxed);
        }
    }

    /// How long a sound starting now should wait: see [`hold_secs`].
    fn hold(&self) -> Duration {
        let synth = f32::from_bits(self.synth_per_token.load(Ordering::Relaxed));
        let speech = f32::from_bits(self.speech_per_token.load(Ordering::Relaxed));
        if synth <= 0.0 || speech <= 0.0 {
            return Duration::ZERO;
        }
        let spent = self
            .synth_began
            .lock()
            .unwrap()
            .map_or(0.0, |began| began.elapsed().as_secs_f32());
        let owed: Vec<(f32, f32)> = self
            .owed
            .lock()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(k, &tokens)| {
                let work = tokens as f32 * synth;
                let left = if k == 0 {
                    (work - spent).max(0.0)
                } else {
                    work
                };
                (left, tokens as f32 * speech)
            })
            .collect();
        let ready = self.ready_ms.load(Ordering::Relaxed) as f32 / 1000.0;
        Duration::from_secs_f32(hold_secs(ready, &owed)).min(LONGEST_HOLD)
    }
}

/// How long to hold `ready_secs` of audio so each unit still owed, given as the seconds its
/// synthesis and its speech take, is ready when the audio before it ends.
fn hold_secs(ready_secs: f32, owed: &[(f32, f32)]) -> f32 {
    let (mut work, mut audio, mut hold) = (0.0f32, ready_secs, 0.0f32);
    for &(synth, speech) in owed {
        work += synth;
        hold = hold.max(work - audio);
        audio += speech;
    }
    hold
}

fn sub(counter: &AtomicU64, by: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
        Some(v.saturating_sub(by))
    });
}

/// A synthesis task feeding a playback task for one reply. Dropping it stops both.
pub struct SpeechPipeline {
    units: Option<mpsc::Sender<SpeechUnit>>,
    meter: Arc<Meter>,
    synth: Option<JoinHandle<()>>,
    playback: Option<JoinHandle<()>>,
}

impl SpeechPipeline {
    /// `on_playback_start` runs when the first audio item begins playback.
    /// `on_playback_done` runs once the last unit has played, however the reply ends early.
    pub fn start(
        voice: Arc<dyn VoiceOutput>,
        on_spoken: Option<SpokenObserver>,
        on_playback_start: Option<Box<dyn FnOnce() + Send>>,
        on_playback_done: Option<Box<dyn FnOnce() + Send>>,
    ) -> Self {
        let meter = Arc::new(Meter::default());
        let (units_tx, mut units_rx) = mpsc::channel::<SpeechUnit>(16);
        let (audio_tx, mut audio_rx) = mpsc::channel::<AudioItem>(16);

        let synth_voice = voice.clone();
        let synth_meter = meter.clone();
        let synth = tokio::spawn(async move {
            while let Some(unit) = units_rx.recv().await {
                let tokens = estimate_tokens(&unit.text);
                let chars = unit.text.chars().count();
                let last = unit.last;
                if let Some(observer) = &on_spoken {
                    observer(&unit.text);
                }
                let started = Instant::now();
                *synth_meter.synth_began.lock().unwrap() = Some(started);
                let item = match synth_voice.synthesize_unit(&unit).await {
                    Ok(Some(wav)) if !wav.is_empty() => {
                        let millis = wav_duration_secs(&wav)
                            .map(|s| (s * 1000.0) as u64)
                            .unwrap_or(0);
                        AudioItem::Wav { wav, millis }
                    }
                    _ => AudioItem::Fallback(unit.text),
                };
                let audio_ms = match &item {
                    AudioItem::Wav { millis, .. } => *millis,
                    _ => 0,
                };
                tracing::info!(
                    kind = "speech_unit",
                    chars,
                    tokens,
                    last,
                    synth_ms = started.elapsed().as_millis() as u64,
                    audio_ms,
                    "speech unit synthesised"
                );
                synth_meter.synthesised(tokens, started.elapsed().as_secs_f32(), audio_ms);
                if audio_tx.send(item).await.is_err() {
                    break;
                }
            }
        });

        let play_meter = meter.clone();
        let playback = tokio::spawn(async move {
            let mut on_playback_start = on_playback_start;
            loop {
                // An item that had to be waited for starts from silence; one already here does not.
                let (item, from_silence) = match audio_rx.try_recv() {
                    Ok(item) => (item, on_playback_start.is_some()),
                    Err(mpsc::error::TryRecvError::Empty) => match audio_rx.recv().await {
                        Some(item) => (item, true),
                        None => break,
                    },
                    Err(mpsc::error::TryRecvError::Disconnected) => break,
                };
                if from_silence {
                    let hold = play_meter.hold();
                    if hold > Duration::from_millis(50) {
                        tracing::info!(
                            kind = "speech_hold",
                            hold_ms = hold.as_millis() as u64,
                            owed_units = play_meter.owed.lock().unwrap().len(),
                            "speech held so the units still owed follow without a gap"
                        );
                        tokio::time::sleep(hold).await;
                    }
                }
                if let Some(start) = on_playback_start.take() {
                    start();
                }
                match item {
                    AudioItem::Wav { wav, millis } => {
                        sub(&play_meter.ready_ms, millis);
                        if let Err(e) = voice.play_audio(wav).await {
                            tracing::warn!("TTS playback failed: {e}");
                        }
                    }
                    AudioItem::Fallback(text) => {
                        if let Err(e) = voice.speak(&text).await {
                            tracing::warn!("TTS failed: {e}");
                        }
                    }
                }
            }
            if let Some(done) = on_playback_done {
                done();
            }
        });

        Self {
            units: Some(units_tx),
            meter,
            synth: Some(synth),
            playback: Some(playback),
        }
    }

    /// Queue a unit; `false` once the pipeline has stopped taking them.
    pub async fn say(&self, unit: SpeechUnit) -> bool {
        let Some(units) = &self.units else {
            return false;
        };
        let tokens = estimate_tokens(&unit.text);
        self.meter.owed.lock().unwrap().push_back(tokens);
        if units.send(unit).await.is_err() {
            self.meter.owed.lock().unwrap().pop_back();
            return false;
        }
        true
    }

    /// No more units: returns once everything queued has played.
    pub async fn finish(&mut self) {
        self.units.take();
        if let Some(synth) = self.synth.take() {
            let _ = synth.await;
        }
        if let Some(playback) = self.playback.take() {
            let _ = playback.await;
        }
    }
}

impl Drop for SpeechPipeline {
    fn drop(&mut self) {
        for task in [self.synth.take(), self.playback.take()]
            .into_iter()
            .flatten()
        {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use pond_voice::dsp::{encode_wav_pcm16, f32_to_pcm16};

    /// Synthesises `play` worth of audio per unit, taking `synth` to do it.
    struct Timed {
        synth: Duration,
        play: Duration,
        born: Instant,
        log: Mutex<Vec<String>>,
        /// When each playback began and ended, in ms since `born`.
        played: Mutex<Vec<(u64, u64)>>,
    }

    impl Timed {
        fn ms(&self) -> u64 {
            self.born.elapsed().as_millis() as u64
        }
    }

    #[async_trait]
    impl VoiceOutput for Timed {
        async fn speak(&self, text: &str) -> Result<()> {
            self.log.lock().unwrap().push(format!("speak {text}"));
            Ok(())
        }
        async fn synthesize_unit(&self, unit: &SpeechUnit) -> Result<Option<Vec<u8>>> {
            tokio::time::sleep(self.synth).await;
            let end = if unit.last { "last" } else { "more" };
            self.log
                .lock()
                .unwrap()
                .push(format!("synth {} {end}", unit.text));
            let samples = vec![0.0; (self.play.as_secs_f32() * 24_000.0) as usize];
            Ok(Some(encode_wav_pcm16(&f32_to_pcm16(&samples), 24_000)))
        }
        async fn play_audio(&self, _audio: Vec<u8>) -> Result<()> {
            let began = self.ms();
            tokio::time::sleep(self.play).await;
            self.played.lock().unwrap().push((began, self.ms()));
            self.log.lock().unwrap().push("play".into());
            Ok(())
        }
    }

    fn voice(synth_ms: u64, play_ms: u64) -> Arc<Timed> {
        Arc::new(Timed {
            synth: Duration::from_millis(synth_ms),
            play: Duration::from_millis(play_ms),
            born: Instant::now(),
            log: Mutex::default(),
            played: Mutex::default(),
        })
    }

    const A_UNIT: &str = "A unit of a dozen words or so, said in one pass.";

    #[test]
    fn a_hold_covers_the_unit_that_would_be_latest() {
        assert_eq!(hold_secs(10.0, &[]), 0.0);
        assert_eq!(
            hold_secs(10.0, &[(4.0, 10.0)]),
            0.0,
            "ready before it is due"
        );
        assert_eq!(hold_secs(10.0, &[(14.0, 10.0)]), 4.0);
        assert_eq!(hold_secs(10.0, &[(14.0, 10.0), (14.0, 10.0)]), 8.0);
        assert_eq!(
            hold_secs(10.0, &[(14.0, 10.0), (2.0, 1.0)]),
            4.0,
            "a short unit behind a long one is ready in time by itself"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn units_play_in_order_as_they_were_handed_over() {
        let v = voice(1, 10);
        let mut p = SpeechPipeline::start(v.clone(), None, None, None);
        p.say(SpeechUnit::from_text("One.")).await;
        p.say(SpeechUnit::closing("Two.")).await;
        p.finish().await;
        assert_eq!(
            *v.log.lock().unwrap(),
            ["synth One. more", "synth Two. last", "play", "play"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_engine_slower_than_its_speech_is_held_so_it_never_stops_between_units() {
        let v = voice(1_400, 1_000);
        let mut p = SpeechPipeline::start(v.clone(), None, None, None);
        for _ in 0..3 {
            p.say(SpeechUnit::from_text(A_UNIT)).await;
        }
        p.finish().await;
        let played = v.played.lock().unwrap().clone();
        assert_eq!(played.len(), 3);
        assert!(
            (2_150..=2_300).contains(&played[0].0),
            "the first sound waits out what the engine still owes: {played:?}"
        );
        for pair in played.windows(2) {
            assert!(
                pair[1].0 - pair[0].1 <= 60,
                "a unit starts as the one before it ends: {played:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_engine_that_keeps_ahead_is_not_held() {
        let v = voice(100, 1_000);
        let mut p = SpeechPipeline::start(v.clone(), None, None, None);
        p.say(SpeechUnit::from_text(A_UNIT)).await;
        p.say(SpeechUnit::closing(A_UNIT)).await;
        p.finish().await;
        let played = v.played.lock().unwrap().clone();
        assert!(played[0].0 <= 110, "{played:?}");
        assert!(played[1].0 - played[0].1 <= 10, "{played:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_unit_said_alone_is_not_held() {
        let v = voice(1_400, 1_000);
        let mut p = SpeechPipeline::start(v.clone(), None, None, None);
        p.say(SpeechUnit::closing(A_UNIT)).await;
        p.finish().await;
        assert!(v.played.lock().unwrap()[0].0 <= 1_410);
    }

    #[tokio::test(start_paused = true)]
    async fn speech_resuming_after_a_tool_is_held_the_same_way() {
        let v = voice(1_400, 1_000);
        let mut p = SpeechPipeline::start(v.clone(), None, None, None);
        p.say(SpeechUnit::from_text(A_UNIT)).await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        p.say(SpeechUnit::from_text(A_UNIT)).await;
        p.say(SpeechUnit::closing(A_UNIT)).await;
        p.finish().await;
        let played = v.played.lock().unwrap().clone();
        assert!(
            played[0].0 <= 1_410,
            "nothing was owed behind it: {played:?}"
        );
        assert!(
            played[2].0 - played[1].1 <= 60,
            "what follows the tool is said without a gap: {played:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_unit_with_no_audio_is_spoken_by_the_fallback() {
        struct Silent(Mutex<Vec<String>>);
        #[async_trait]
        impl VoiceOutput for Silent {
            async fn speak(&self, text: &str) -> Result<()> {
                self.0.lock().unwrap().push(text.to_string());
                Ok(())
            }
        }
        let v = Arc::new(Silent(Mutex::default()));
        let mut p = SpeechPipeline::start(v.clone(), None, None, None);
        p.say(SpeechUnit::closing("Hello there.")).await;
        p.finish().await;
        assert_eq!(*v.0.lock().unwrap(), ["Hello there."]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_observer_hears_each_unit_and_done_runs_after_the_last() {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (h, d) = (heard.clone(), done.clone());
        let mut p = SpeechPipeline::start(
            voice(1, 1),
            Some(Arc::new(move |t: &str| {
                h.lock().unwrap().push(t.to_string())
            })),
            None,
            Some(Box::new(move || d.store(true, Ordering::SeqCst))),
        );
        p.say(SpeechUnit::closing("Only one.")).await;
        p.finish().await;
        assert_eq!(*heard.lock().unwrap(), ["Only one."]);
        assert!(done.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn playback_start_runs_when_the_first_unit_reaches_the_speaker() {
        let v = voice(1_400, 1_000);
        let started = Arc::new(Mutex::new(None));
        let (s, clock) = (started.clone(), v.clone());
        let mut p = SpeechPipeline::start(
            v.clone(),
            None,
            Some(Box::new(move || *s.lock().unwrap() = Some(clock.ms()))),
            None,
        );
        assert!(started.lock().unwrap().is_none());
        p.say(SpeechUnit::from_text(A_UNIT)).await;
        p.say(SpeechUnit::closing(A_UNIT)).await;
        p.finish().await;
        assert_eq!(
            *started.lock().unwrap(),
            Some(v.played.lock().unwrap()[0].0),
            "the working tone plays through the hold"
        );
    }
}
