//! One reply's speech: units are synthesised one at a time and played in order, and the pipeline
//! reports how far synthesis is ahead of the speaker, so the splitter can keep sentences whole.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::time::Instant;

use pond_voice::dsp::wav_duration_secs;
use pond_voice::text::{estimate_tokens, pack_units, Pace, SpeechUnit};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::models::ports::voice_output::VoiceOutput;

/// Hears the text of each unit as it goes to synthesis.
pub type SpokenObserver = Arc<dyn Fn(&str) + Send + Sync>;

/// Seconds of synthesis per token before any is measured: the nano's q4f16 at about real time.
const DEFAULT_SYNTH_SECS_PER_TOKEN: f32 = 0.06;
/// Seconds of synthesis per second of speech before any is measured. Taken as too slow to gain
/// on the speaker, so a slow board's first reply is not cut inside a sentence.
const DEFAULT_SYNTH_PER_AUDIO: f32 = 1.0;
/// Measured seconds of synthesis per second of speech, as `f32` bits and 0 until one is measured.
type KnownPace = Arc<AtomicU32>;
/// Kept across replies: a reply's first unit is cut before that reply has measured anything.
static PROCESS_PACE: std::sync::LazyLock<KnownPace> =
    std::sync::LazyLock::new(|| Arc::new(AtomicU32::new(0)));
/// Weight of the newest measurement in the running synthesis rate.
const RATE_SMOOTHING: f32 = 0.3;
/// How long a whole sentence waits for the text after it, which says whether a paragraph ends
/// there, before it is said anyway: a model that stalls must not hold back what it has finished.
pub const SENTENCE_HOLD: std::time::Duration = std::time::Duration::from_millis(200);

enum AudioItem {
    Wav {
        wav: Vec<u8>,
        millis: u64,
    },
    /// Synthesis gave nothing: the output speaks the text its own way.
    Fallback(String),
}

/// What the two tasks know about how far ahead the speech is.
struct Meter {
    epoch: Instant,
    /// Audio handed to playback and not yet started.
    ready_ms: AtomicU64,
    /// When the unit playing now ends, in ms since `epoch`.
    playing_until_ms: AtomicU64,
    /// Estimated tokens handed to synthesis and not yet synthesised.
    owed_tokens: AtomicU64,
    /// Measured seconds of synthesis per token, as `f32` bits.
    secs_per_token: AtomicU32,
    synth_per_audio: KnownPace,
}

impl Meter {
    fn new(synth_per_audio: KnownPace) -> Self {
        Self {
            epoch: Instant::now(),
            ready_ms: AtomicU64::new(0),
            playing_until_ms: AtomicU64::new(0),
            owed_tokens: AtomicU64::new(0),
            secs_per_token: AtomicU32::new(DEFAULT_SYNTH_SECS_PER_TOKEN.to_bits()),
            synth_per_audio,
        }
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// The pace with only the audio waiting to play: what the unit synthesised next has to beat.
    fn queued(&self) -> Pace {
        let playing = self
            .playing_until_ms
            .load(Ordering::Relaxed)
            .saturating_sub(self.now_ms());
        let measured = f32::from_bits(self.synth_per_audio.load(Ordering::Relaxed));
        Pace {
            slack_secs: (self.ready_ms.load(Ordering::Relaxed) + playing) as f32 / 1000.0,
            synth_secs_per_token: f32::from_bits(self.secs_per_token.load(Ordering::Relaxed)),
            synth_per_audio: if measured > 0.0 {
                measured
            } else {
                DEFAULT_SYNTH_PER_AUDIO
            },
        }
    }

    /// The pace for text still arriving: the units already handed over are synthesised first.
    fn pace(&self) -> Pace {
        let queued = self.queued();
        let owed = self.owed_tokens.load(Ordering::Relaxed) as f32 * queued.synth_secs_per_token;
        Pace {
            slack_secs: queued.slack_secs - owed,
            ..queued
        }
    }

    fn synthesised(&self, tokens: usize, took_secs: f32, audio_secs: f32) {
        sub(&self.owed_tokens, tokens as u64);
        if tokens > 0 {
            let old = f32::from_bits(self.secs_per_token.load(Ordering::Relaxed));
            let new = old * (1.0 - RATE_SMOOTHING) + took_secs / tokens as f32 * RATE_SMOOTHING;
            self.secs_per_token.store(new.to_bits(), Ordering::Relaxed);
        }
        if audio_secs > 0.0 {
            let now = took_secs / audio_secs;
            let old = f32::from_bits(self.synth_per_audio.load(Ordering::Relaxed));
            let new = if old > 0.0 {
                old * (1.0 - RATE_SMOOTHING) + now * RATE_SMOOTHING
            } else {
                now
            };
            self.synth_per_audio.store(new.to_bits(), Ordering::Relaxed);
        }
    }
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
        Self::start_knowing(
            PROCESS_PACE.clone(),
            voice,
            on_spoken,
            on_playback_start,
            on_playback_done,
        )
    }

    fn start_knowing(
        known: KnownPace,
        voice: Arc<dyn VoiceOutput>,
        on_spoken: Option<SpokenObserver>,
        on_playback_start: Option<Box<dyn FnOnce() + Send>>,
        on_playback_done: Option<Box<dyn FnOnce() + Send>>,
    ) -> Self {
        let meter = Arc::new(Meter::new(known));
        let (units_tx, mut units_rx) = mpsc::channel::<SpeechUnit>(16);
        let (audio_tx, mut audio_rx) = mpsc::channel::<AudioItem>(16);

        let synth_voice = voice.clone();
        let synth_meter = meter.clone();
        let synth = tokio::spawn(async move {
            let mut queue: VecDeque<SpeechUnit> = VecDeque::new();
            loop {
                while let Ok(unit) = units_rx.try_recv() {
                    queue.push_back(unit);
                }
                if queue.is_empty() {
                    match units_rx.recv().await {
                        Some(unit) => queue.push_back(unit),
                        None => break,
                    }
                    continue;
                }
                let before: usize = queue.iter().map(|u| estimate_tokens(&u.text)).sum();
                let Some(unit) = pack_units(&mut queue, synth_meter.queued()) else {
                    continue;
                };
                let after: usize = queue.iter().map(|u| estimate_tokens(&u.text)).sum();
                let tokens = before - after;
                let boundary = unit.boundary;
                let chars = unit.text.chars().count();
                if let Some(observer) = &on_spoken {
                    observer(&unit.text);
                }
                let started = Instant::now();
                let item = match synth_voice.synthesize_unit(&unit).await {
                    Ok(Some(wav)) if !wav.is_empty() => {
                        let millis = wav_duration_secs(&wav)
                            .map(|s| (s * 1000.0) as u64)
                            .unwrap_or(0);
                        synth_meter.ready_ms.fetch_add(millis, Ordering::Relaxed);
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
                    boundary = ?boundary,
                    synth_ms = started.elapsed().as_millis() as u64,
                    audio_ms,
                    "speech unit synthesised"
                );
                synth_meter.synthesised(
                    tokens,
                    started.elapsed().as_secs_f32(),
                    audio_ms as f32 / 1000.0,
                );
                if audio_tx.send(item).await.is_err() {
                    break;
                }
            }
        });

        let play_meter = meter.clone();
        let playback = tokio::spawn(async move {
            let mut on_playback_start = on_playback_start;
            while let Some(item) = audio_rx.recv().await {
                if let Some(start) = on_playback_start.take() {
                    start();
                }
                match item {
                    AudioItem::Wav { wav, millis } => {
                        sub(&play_meter.ready_ms, millis);
                        play_meter
                            .playing_until_ms
                            .store(play_meter.now_ms() + millis, Ordering::Relaxed);
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

    /// How far ahead of the speaker synthesis is now.
    pub fn pace(&self) -> Pace {
        self.meter.pace()
    }

    /// Queue a unit; `false` once the pipeline has stopped taking them.
    pub async fn say(&self, unit: SpeechUnit) -> bool {
        let Some(units) = &self.units else {
            return false;
        };
        let tokens = estimate_tokens(&unit.text) as u64;
        self.meter.owed_tokens.fetch_add(tokens, Ordering::Relaxed);
        if units.send(unit).await.is_err() {
            sub(&self.meter.owed_tokens, tokens);
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
    use pond_voice::text::Boundary;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Synthesises `play` worth of audio per unit, taking `synth` to do it.
    struct Timed {
        synth: Duration,
        play: Duration,
        log: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl VoiceOutput for Timed {
        async fn speak(&self, text: &str) -> Result<()> {
            self.log.lock().unwrap().push(format!("speak {text}"));
            Ok(())
        }
        async fn synthesize_unit(&self, unit: &SpeechUnit) -> Result<Option<Vec<u8>>> {
            tokio::time::sleep(self.synth).await;
            self.log
                .lock()
                .unwrap()
                .push(format!("synth {} {:?}", unit.text, unit.boundary));
            let samples = vec![0.0; (self.play.as_secs_f32() * 24_000.0) as usize];
            Ok(Some(encode_wav_pcm16(&f32_to_pcm16(&samples), 24_000)))
        }
        async fn play_audio(&self, _audio: Vec<u8>) -> Result<()> {
            tokio::time::sleep(self.play).await;
            self.log.lock().unwrap().push("play".into());
            Ok(())
        }
    }

    fn voice(synth_ms: u64, play_ms: u64) -> Arc<Timed> {
        Arc::new(Timed {
            synth: Duration::from_millis(synth_ms),
            play: Duration::from_millis(play_ms),
            log: Mutex::default(),
        })
    }

    #[tokio::test(start_paused = true)]
    async fn units_play_in_order_with_their_boundaries() {
        let v = voice(10, 1);
        let mut p = SpeechPipeline::start(v.clone(), None, None, None);
        p.say(SpeechUnit::new("One.", Boundary::Paragraph)).await;
        p.say(SpeechUnit::new("Two.", Boundary::End)).await;
        p.finish().await;
        assert_eq!(
            *v.log.lock().unwrap(),
            ["synth One. Paragraph", "play", "synth Two. End", "play"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn queued_audio_shows_as_slack() {
        let v = voice(100, 5_000);
        let p = SpeechPipeline::start(v, None, None, None);
        assert!(p.pace().slack_secs <= 0.0, "nothing is queued at the start");
        p.say(SpeechUnit::new(
            "The first sentence of the reply.",
            Boundary::Sentence,
        ))
        .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let ahead = p.pace().slack_secs;
        assert!(
            (4.5..=5.0).contains(&ahead),
            "five seconds of audio are playing: {ahead}"
        );
        tokio::time::sleep(Duration::from_millis(2_000)).await;
        assert!(
            p.pace().slack_secs < ahead,
            "playing audio consumes slack over time"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn synthesis_still_owed_comes_off_the_slack() {
        let v = voice(2_000, 5_000);
        let p = SpeechPipeline::start(v, None, None, None);
        p.say(SpeechUnit::new("The first sentence.", Boundary::Sentence))
            .await;
        tokio::time::sleep(Duration::from_millis(2_300)).await;
        let before = p.pace().slack_secs;
        let next = "A second sentence that will take the engine a while to make into sound.";
        p.say(SpeechUnit::new(next, Boundary::Sentence)).await;
        let owed = before - p.pace().slack_secs;
        assert!(
            owed >= estimate_tokens(next) as f32 * 0.04,
            "the unit handed over is synthesised before anything cut now: {owed}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_engine_slower_than_its_speech_is_known_to_the_next_reply() {
        let known = KnownPace::default();
        let start =
            |v: Arc<Timed>| SpeechPipeline::start_knowing(known.clone(), v, None, None, None);

        let mut p = start(voice(1_340, 1_000));
        assert!(
            !p.pace().gains_on_the_speaker(),
            "unmeasured, an engine is taken as unable to gain"
        );
        p.say(SpeechUnit::new("One sentence.", Boundary::End)).await;
        p.finish().await;
        let slow = p.pace().synth_per_audio;
        assert!((1.3..=1.4).contains(&slow), "{slow}");
        assert_eq!(start(voice(1, 1)).pace().synth_per_audio, slow);

        let fast = KnownPace::default();
        let mut p = SpeechPipeline::start_knowing(fast, voice(200, 1_000), None, None, None);
        p.say(SpeechUnit::new("One sentence.", Boundary::End)).await;
        p.finish().await;
        assert!(p.pace().gains_on_the_speaker(), "{:?}", p.pace());
    }

    #[tokio::test(start_paused = true)]
    async fn units_waiting_behind_a_slow_synthesis_are_packed_when_there_is_time() {
        let v = voice(200, 20_000);
        let p = SpeechPipeline::start(v.clone(), None, None, None);
        p.say(SpeechUnit::new("First.", Boundary::Sentence)).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        // While the first plays for twenty seconds, three short sentences queue up.
        for s in ["Two.", "Three.", "Four."] {
            p.say(SpeechUnit::new(s, Boundary::Sentence)).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let log = v.log.lock().unwrap().clone();
        assert!(
            log.iter().any(|l| l == "synth Two. Three. Four. Sentence"),
            "{log:?}"
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
        p.say(SpeechUnit::new("Hello there.", Boundary::End)).await;
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
        p.say(SpeechUnit::new("Only one.", Boundary::End)).await;
        p.finish().await;
        assert_eq!(*heard.lock().unwrap(), ["Only one."]);
        assert!(done.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn playback_start_runs_when_the_first_unit_reaches_the_speaker() {
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = started.clone();
        let mut p = SpeechPipeline::start(
            voice(10, 1),
            None,
            Some(Box::new(move || s.store(true, Ordering::SeqCst))),
            None,
        );
        assert!(!started.load(Ordering::SeqCst));
        p.say(SpeechUnit::new("First.", Boundary::Sentence)).await;
        p.finish().await;
        assert!(started.load(Ordering::SeqCst));
    }
}
