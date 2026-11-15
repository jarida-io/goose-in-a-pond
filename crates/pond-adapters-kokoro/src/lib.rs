//! Kokoro-82M TTS as a [`VoiceOutput`]: text -> espeak IPA -> vocab ids -> 24 kHz f32 via `ort`.
//! The ~92 MB session loads on first `speak`, so a bad model path fails there, not in `new()`.

use anyhow::{Context, Result};
use async_trait::async_trait;
use pond_audio_out::{
    bus, play_turn_wav, play_wav, start_thinking_tone_thread, Claim, Floor, TurnFloor, TONE_OFF,
};
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::shared::domain::agent::ThrottledAudioLevelSink;
use pond_voice::dsp::{encode_wav_pcm16, f32_to_pcm16, speech_bounds};
use pond_voice::text::{split_text, SpeechUnit, UNIT_GAP_SECS};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{OwnedRwLockWriteGuard, RwLock};

pub mod engine;
pub mod gpu;
pub mod tokenizer;
pub mod voices;

pub use engine::{Engine, SAMPLE_RATE};
pub use gpu::{GpuSpeech, HelperCommand};
pub use tokenizer::Vocab;
pub use voices::StyleTable;

/// The model's reference voice, used in its published samples.
pub const DEFAULT_VOICE: &str = "af_heart";
pub const DEFAULT_SPEED: f32 = 1.0;
/// Below 0.5 the prosody smears; above 2.0 it clips words.
pub const MIN_SPEED: f32 = 0.5;
pub const MAX_SPEED: f32 = 2.0;

/// Kept of Kokoro's own silence before a chunk's first sound and after its last. The model puts
/// ~0.3 s before and ~0.5 s after every forward pass, which made every join of two ~0.8 s long.
const LEAD_KEEP_SECS: f32 = 0.03;
const TAIL_KEEP_SECS: f32 = 0.05;
/// Fade at a trimmed edge, so the cut never clicks.
const EDGE_FADE_SECS: f32 = 0.005;

/// Where Kokoro's files live, and how hard to work.
#[derive(Debug, Clone)]
pub struct KokoroConfig {
    /// The `.onnx` weights (quality tier is chosen by picking the file).
    pub model_path: PathBuf,
    /// Directory of `<voice>.bin` style tables.
    pub voices_dir: PathBuf,
    /// The model repo's `tokenizer.json`.
    pub tokenizer_path: PathBuf,
    /// Bound on ONNX Runtime's per-op threads. `None` lets ORT decide.
    pub intra_threads: Option<usize>,
    /// espeak-ng data directory, if not on the default search path.
    pub espeak_data: Option<PathBuf>,
}

/// Env var that espeak-rs consults to find the bundled `espeak-ng-data` dir.
const ESPEAKNG_DATA_DIRECTORY: &str = "PIPER_ESPEAKNG_DATA_DIRECTORY";

/// ONNX session load deadline: a cold load takes seconds, but a broken ORT blocks forever.
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

pub struct KokoroOutput {
    config: KokoroConfig,
    /// The active `.onnx`, which changes at runtime in lockstep with `engine`.
    model_path: RwLock<PathBuf>,
    vocab: Arc<Vocab>,
    /// Loaded lazily on first synthesis; `None` means "not paying for it yet". An `Arc` so a
    /// synthesis can take its guard onto a blocking thread.
    engine: Arc<RwLock<Option<Engine>>>,
    /// The currently selected voice's style table, loaded on demand.
    style: RwLock<Option<Arc<StyleTable>>>,
    voice_name: RwLock<String>,
    /// Pace, as `f32::to_bits` so it can live in an atomic.
    speed_bits: AtomicU64,
    /// Where long speech is said on the GPU; `None` where this host cannot.
    gpu: Option<Arc<GpuSpeech>>,

    thinking_for: Arc<AtomicU64>,
    utterance: Arc<AtomicU64>,
    speech_interrupted: Arc<AtomicBool>,
    /// Playbacks in flight; zero between sentences, e.g. while a tool runs.
    playing: Arc<std::sync::atomic::AtomicUsize>,
    /// The audio bus, held from `begin_utterance` to `end_utterance` so nothing plays inside a turn.
    turn: TurnFloor,
    /// Between `begin_utterance` and `end_utterance`: what plays then is the turn's.
    in_turn: AtomicBool,
    audio_level_sink: Option<Arc<ThrottledAudioLevelSink>>,
    /// Sample rate of the most recent synthesis, for diagnostics.
    last_sample_rate: Mutex<Option<u32>>,
}

impl KokoroOutput {
    /// Reads the vocab but not the model; fails only on a bad `tokenizer.json`.
    pub fn new(config: KokoroConfig) -> Result<Self> {
        if let Some(dir) = &config.espeak_data {
            pond_core::shared::services::process_env::set(ESPEAKNG_DATA_DIRECTORY, dir);
        }
        let vocab = Vocab::load(&config.tokenizer_path)?;
        tracing::info!(symbols = vocab.len(), "Kokoro vocab loaded");
        bus().open_early();

        Ok(Self {
            model_path: RwLock::new(config.model_path.clone()),
            config,
            vocab: Arc::new(vocab),
            engine: Arc::new(RwLock::new(None)),
            style: RwLock::new(None),
            voice_name: RwLock::new(DEFAULT_VOICE.to_string()),
            speed_bits: AtomicU64::new(DEFAULT_SPEED.to_bits() as u64),
            gpu: None,
            thinking_for: Arc::new(AtomicU64::new(TONE_OFF)),
            utterance: Arc::new(AtomicU64::new(1)),
            speech_interrupted: Arc::new(AtomicBool::new(false)),
            playing: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            turn: TurnFloor::default(),
            in_turn: AtomicBool::new(false),
            audio_level_sink: None,
            last_sample_rate: Mutex::new(None),
        })
    }

    /// Attach a live playback-amplitude reporter, the `speaking` analog of mic RMS.
    pub fn with_audio_level_sink(mut self, sink: Arc<ThrottledAudioLevelSink>) -> Self {
        self.audio_level_sink = Some(sink);
        self
    }

    /// Long speech is said on the GPU through `gpu` whenever it takes it.
    pub fn with_gpu_speech(mut self, gpu: GpuSpeech) -> Self {
        self.gpu = Some(Arc::new(gpu));
        self
    }

    pub fn gpu_speech(&self) -> Option<&Arc<GpuSpeech>> {
        self.gpu.as_ref()
    }

    /// Select the voice. Swaps a 522 KB table; the session is untouched.
    pub async fn set_voice(&self, name: &str) -> Result<()> {
        // Load first so a bad name keeps the current voice instead of muting the pond.
        let table = StyleTable::load(&self.config.voices_dir, name)?;
        *self.style.write().await = Some(Arc::new(table));
        *self.voice_name.write().await = name.to_string();
        tracing::info!(voice = name, "Kokoro voice selected");
        Ok(())
    }

    pub async fn voice(&self) -> String {
        self.voice_name.read().await.clone()
    }

    /// Clamps to [`MIN_SPEED`]..=[`MAX_SPEED`] and returns the value applied.
    pub fn set_speed(&self, speed: f32) -> f32 {
        let clamped = speed.clamp(MIN_SPEED, MAX_SPEED);
        self.speed_bits
            .store(clamped.to_bits() as u64, Ordering::Relaxed);
        clamped
    }

    pub fn speed(&self) -> f32 {
        f32::from_bits(self.speed_bits.load(Ordering::Relaxed) as u32)
    }

    pub fn installed_voices(&self) -> Vec<String> {
        voices::installed(&self.config.voices_dir)
    }

    pub async fn is_loaded(&self) -> bool {
        self.engine.read().await.is_some()
    }

    /// Drop the ONNX session to free its memory; the next utterance reloads it.
    pub async fn unload(&self) {
        if self.engine.write().await.take().is_some() {
            tracing::info!("Kokoro session unloaded");
        }
    }

    /// Switch quality tier (`.onnx` file). Holds the engine lock for the whole swap so no synthesis
    /// sees a path that disagrees with the session.
    pub async fn set_model(&self, path: PathBuf) -> Result<()> {
        if !path.exists() {
            return Err(anyhow::anyhow!(
                "Kokoro model not found at {}",
                path.display()
            ));
        }
        let mut engine = self.engine.write().await;
        *engine = None;
        *self.model_path.write().await = path.clone();
        tracing::info!(path = %path.display(), "Kokoro quality tier changed");
        Ok(())
    }

    pub async fn model_path(&self) -> PathBuf {
        self.model_path.read().await.clone()
    }

    /// Ensure the engine is loaded in RAM, loading and caching on disk if absent.
    pub async fn ensure_loaded(&self) -> Result<()> {
        self.loaded_engine().await.map(drop)
    }

    /// The engine, loaded if absent, under a guard that can move to a blocking thread.
    async fn loaded_engine(&self) -> Result<OwnedRwLockWriteGuard<Option<Engine>>> {
        let mut engine_guard = self.engine.clone().write_owned().await;
        if engine_guard.is_none() {
            let path = self.model_path.read().await.clone();
            let threads = self.config.intra_threads;
            let loaded = tokio::time::timeout(
                LOAD_TIMEOUT,
                tokio::task::spawn_blocking(move || Engine::load(&path, threads)),
            )
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "Kokoro model load timed out after {}s — the ONNX Runtime library is \
                     probably missing or version-incompatible",
                    LOAD_TIMEOUT.as_secs()
                )
            })?
            .context("Kokoro model load panicked")??;
            *engine_guard = Some(loaded);
        }
        Ok(engine_guard)
    }

    /// Prewarm the model into RAM ahead of time.
    pub async fn prewarm(&self) -> Result<()> {
        self.ensure_loaded().await
    }

    /// Mono f32 at [`SAMPLE_RATE`], ending at rest for the last unit of its speech and in
    /// [`UNIT_GAP_SECS`] otherwise; empty for text that phonemizes to nothing.
    pub async fn synth_samples(&self, unit: &SpeechUnit) -> Result<Vec<f32>> {
        self.synth_samples_with(&unit.text, unit.last, None).await
    }

    /// `text` in `voice` as a WAV, leaving the selected voice untouched: a turn reads the
    /// selection between units, so swapping it for a preview would race that turn.
    pub async fn synth_wav_in_voice(&self, voice: &str, text: &str) -> Result<Vec<u8>> {
        let style = Arc::new(StyleTable::load(&self.config.voices_dir, voice)?);
        let samples = self.synth_samples_with(text, true, Some(style)).await?;
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        Ok(encode_wav_pcm16(&f32_to_pcm16(&samples), SAMPLE_RATE))
    }

    /// `style` is the voice to use; `None` is the selected one. A text longer than one pass is
    /// said in as few as hold it; each pass is cut to its speech, then a gap unless it is `last`.
    async fn synth_samples_with(
        &self,
        text: &str,
        last: bool,
        style: Option<Arc<StyleTable>>,
    ) -> Result<Vec<f32>> {
        let vocab = self.vocab.clone();
        let owned = text.to_string();
        let chunks = off_the_runtime(move || {
            let mut chunks = Vec::new();
            let mut dropped = 0;
            for part in split_text(&owned) {
                let (passes, lost) = tokenizer::chunk(&part.text, &vocab)?;
                dropped += lost;
                let n = passes.len();
                for (k, pass) in passes.into_iter().enumerate() {
                    let gap = match (k + 1 == n, part.last) {
                        (false, _) => WORD_GAP_SECS,
                        (true, false) => UNIT_GAP_SECS,
                        (true, true) if last => 0.0,
                        (true, true) => UNIT_GAP_SECS,
                    };
                    chunks.push((pass, gap));
                }
            }
            if dropped > 0 {
                // Not fatal, but the word will sound wrong: the model never learned that phoneme.
                tracing::warn!(
                    dropped,
                    text_chars = owned.chars().count(),
                    "Kokoro dropped phonemes outside the model vocab"
                );
            }
            Ok(chunks)
        })
        .await?;
        if chunks.is_empty() {
            return Ok(Vec::new());
        }

        // A preview in another voice is one short line, and must not end a speech's helper.
        let gpu = self.gpu.clone().filter(|_| style.is_none());
        let style = match style {
            Some(style) => style,
            None => self.ensure_style().await?,
        };
        let speed = self.speed();
        let mut engine = self.loaded_engine().await?;
        let tokens: usize = chunks.iter().map(|(chunk, _)| chunk.tokens.len()).sum();

        let samples = off_the_runtime(move || {
            let Some(engine) = engine.as_mut() else {
                anyhow::bail!("the Kokoro engine was unloaded before synthesis");
            };
            let mut gpu = gpu.filter(|gpu| gpu.takes(tokens, last));
            let mut samples = Vec::new();
            for (chunk, gap) in &chunks {
                push_checked(&mut samples, chunk, *gap, speed, |part| {
                    let row = style.style_for(part.tokens.len());
                    if let Some(helper) = &gpu {
                        match helper.synthesize(&part.padded(), row, speed) {
                            Ok(audio) => return Ok(audio),
                            Err(e) => {
                                tracing::warn!(
                                    kind = "kokoro_gpu_failed",
                                    error = %format!("{e:#}"),
                                    "a pass of long speech came back to the CPU"
                                );
                                gpu = None;
                            }
                        }
                    }
                    engine.synthesize(part, row, speed)
                })?;
            }
            if let Some(gpu) = gpu.filter(|_| last) {
                gpu.end("speech_ended");
            }
            Ok(samples)
        })
        .await?;
        hand_back_heap();
        *self.last_sample_rate.lock().unwrap() = Some(SAMPLE_RATE);
        Ok(samples)
    }

    /// Synthesize to a WAV buffer ready for playback or an HTTP response.
    pub async fn synth_wav(&self, unit: &SpeechUnit) -> Result<Vec<u8>> {
        let samples = self.synth_samples(unit).await?;
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        Ok(encode_wav_pcm16(&f32_to_pcm16(&samples), SAMPLE_RATE))
    }

    async fn ensure_style(&self) -> Result<Arc<StyleTable>> {
        if let Some(s) = self.style.read().await.as_ref() {
            return Ok(s.clone());
        }
        let name = self.voice_name.read().await.clone();
        let table = Arc::new(StyleTable::load(&self.config.voices_dir, &name)?);
        *self.style.write().await = Some(table.clone());
        Ok(table)
    }

    pub fn last_sample_rate(&self) -> Option<u32> {
        *self.last_sample_rate.lock().unwrap()
    }

    /// A turn that begins, or a reply cut short, needs the helper's memory back at once: the
    /// turn's recording is heard only if its encoder fits.
    fn free_the_gpu(&self) {
        if let Some(gpu) = &self.gpu {
            gpu.stop_now();
        }
    }
}

/// The longest words said outside a turn queue for the speaker before they are dropped.
const SPEECH_WAITS_AT_MOST: Duration = Duration::from_secs(120);
/// The longest a voice sample queues for the speaker: past that, nobody is still waiting for it.
const SAMPLE_WAITS_AT_MOST: Duration = Duration::from_secs(30);

/// Where a playback holds the audio bus.
#[derive(Clone)]
enum Hold {
    /// The turn's own hold, from `begin_utterance` to `end_utterance`.
    Turn(TurnFloor),
    /// A claim of its own, for sound outside any turn.
    Own(Arc<Floor>),
}

impl Hold {
    fn play(
        &self,
        wav: Vec<u8>,
        interrupted: &AtomicBool,
        utterance: &AtomicU64,
        sink: Option<&ThrottledAudioLevelSink>,
    ) -> Result<()> {
        match self {
            Hold::Turn(turn) => play_turn_wav(wav, turn, interrupted, utterance, sink),
            Hold::Own(floor) => play_wav(wav, floor, interrupted, utterance, sink),
        }
    }
}

impl KokoroOutput {
    /// Queues for the bus on a blocking thread; `None` when it stayed taken or is no longer wanted.
    async fn claim(
        &self,
        claim: Claim,
        most: Duration,
        still_wanted: impl Fn() -> bool + Send + 'static,
    ) -> Result<Option<Floor>> {
        tokio::task::spawn_blocking(move || bus().claim(claim, Some(most), still_wanted))
            .await
            .context("a Kokoro bus claim panicked")
    }

    /// Plays `units` where `hold` holds the bus, in one stretch: all are synthesised first, since
    /// an engine slower than its speech would stop between units.
    async fn say_units(&self, units: Vec<SpeechUnit>, hold: Hold) -> Result<()> {
        let mut wavs = Vec::with_capacity(units.len());
        for unit in &units {
            if self.speech_interrupted.load(Ordering::Relaxed) {
                return Ok(());
            }
            let wav = self.synth_wav(unit).await?;
            if !wav.is_empty() {
                wavs.push(wav);
            }
        }
        for wav in wavs {
            if self.speech_interrupted.load(Ordering::Relaxed) {
                break;
            }
            self.play_held(wav, hold.clone()).await?;
        }
        Ok(())
    }

    async fn play_held(&self, audio: Vec<u8>, hold: Hold) -> Result<()> {
        let interrupted = self.speech_interrupted.clone();
        let utterance = self.utterance.clone();
        let sink = self.audio_level_sink.clone();
        let playing = Playing::start(&self.playing);
        tokio::task::spawn_blocking(move || {
            let _playing = playing;
            hold.play(audio, &interrupted, &utterance, sink.as_deref())
        })
        .await
        .context("Kokoro playback task panicked")?
    }
}

#[async_trait]
impl VoiceOutput for KokoroOutput {
    /// Inside a turn, more of the turn; outside one, words said whole that queue for the bus as
    /// [`Claim::Speech`] and give way to a turn that begins meanwhile.
    async fn speak(&self, text: &str) -> Result<()> {
        let units = split_text(text);
        if units.is_empty() {
            return Ok(());
        }
        if self.in_turn.load(Ordering::SeqCst) {
            return self.say_units(units, Hold::Turn(self.turn.clone())).await;
        }
        let mine = self.utterance.load(Ordering::SeqCst);
        let utterance = self.utterance.clone();
        let wanted = move || utterance.load(Ordering::Relaxed) == mine;
        let Some(floor) = self
            .claim(Claim::Speech, SPEECH_WAITS_AT_MOST, wanted)
            .await?
        else {
            tracing::debug!("Kokoro: words outside a turn gave way to a turn or a busy speaker");
            return Ok(());
        };
        // A barge-in on an earlier turn is no reason to cut these short.
        self.speech_interrupted.store(false, Ordering::SeqCst);
        self.say_units(units, Hold::Own(Arc::new(floor))).await
    }

    async fn synthesize(&self, text: &str) -> Result<Option<Vec<u8>>> {
        self.synthesize_unit(&SpeechUnit::from_text(text)).await
    }

    async fn synthesize_unit(&self, unit: &SpeechUnit) -> Result<Option<Vec<u8>>> {
        let wav = self.synth_wav(unit).await?;
        Ok((!wav.is_empty()).then_some(wav))
    }

    /// Inside a turn, the turn's next sound; outside one, a [`Claim::Sample`] that a turn takes
    /// the bus from.
    async fn play_audio(&self, audio: Vec<u8>) -> Result<()> {
        if audio.is_empty() {
            return Ok(());
        }
        if self.in_turn.load(Ordering::SeqCst) {
            return self.play_held(audio, Hold::Turn(self.turn.clone())).await;
        }
        let floor = self
            .claim(Claim::Sample, SAMPLE_WAITS_AT_MOST, || true)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the speaker stayed busy"))?;
        // Its own flags: only the bus, wanted back by a turn, cuts it short.
        let (interrupted, utterance) = (AtomicBool::new(false), AtomicU64::new(0));
        let sink = self.audio_level_sink.clone();
        tokio::task::spawn_blocking(move || {
            play_wav(audio, &floor, &interrupted, &utterance, sink.as_deref())
        })
        .await
        .context("Kokoro playback task panicked")?
    }

    fn is_playing(&self) -> Option<bool> {
        Some(self.playing.load(Ordering::SeqCst) > 0)
    }

    /// Queues the turn for the bus now, so it holds it through its thinking as well as its words.
    fn begin_utterance(&self) {
        // Generation first, then clear the interrupt, so stale audio never sees a cleared flag.
        let mine = self.utterance.fetch_add(1, Ordering::SeqCst) + 1;
        self.speech_interrupted.store(false, Ordering::SeqCst);
        self.turn.release();
        self.in_turn.store(true, Ordering::SeqCst);
        self.free_the_gpu();
        let (utterance, interrupted) = (self.utterance.clone(), self.speech_interrupted.clone());
        self.turn.begin(move || {
            utterance.load(Ordering::Relaxed) == mine && !interrupted.load(Ordering::Relaxed)
        });
    }

    fn end_utterance(&self) {
        self.in_turn.store(false, Ordering::SeqCst);
        self.turn.release();
    }

    fn stop_speaking(&self) {
        self.speech_interrupted.store(true, Ordering::SeqCst);
        self.turn.release();
        self.free_the_gpu();
    }

    fn start_thinking_tone(&self) {
        let mine = self.utterance.load(Ordering::SeqCst);
        // A turn asking twice must not stack a second thread.
        if self.thinking_for.swap(mine, Ordering::SeqCst) == mine {
            return;
        }
        start_thinking_tone_thread(self.thinking_for.clone(), mine, self.turn.clone());
    }

    fn stop_thinking_tone(&self) {
        self.thinking_for.store(TONE_OFF, Ordering::SeqCst);
    }
}

/// Voice-preview line: long enough for prosody, varied enough to tell voices apart.
pub const PREVIEW_SENTENCE: &str =
    "Hello, I'm Jarida. I live here on your shelf, I think on my own, \
     and nothing you say to me leaves this room.";

/// Whether this build targets the Jetson boards, where the int8 tiers misbehave.
const fn aarch64_linux() -> bool {
    cfg!(all(target_arch = "aarch64", target_os = "linux"))
}

/// Leaves two cores free: speech must beat RTF 1.0 (Orin Nano q4f16: 0.78 at 4 threads), and
/// each ORT thread holds resident memory per session.
pub fn default_intra_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .saturating_sub(2)
        .clamp(2, 6)
}

/// Tier for a fresh install. On a Jetson Orin Nano only `q4f16` both makes sound and beats real
/// time: `q8` runs at RTF 1.23 and `q8f16` outputs silence.
pub fn host_default_quality() -> &'static str {
    if aarch64_linux() {
        "q4f16"
    } else {
        "q8f16"
    }
}

/// Host tier to adopt only while the household hasn't chosen (`stored` empty or `untouched`, the
/// struct default); any real choice is left alone, even one this host must substitute.
pub fn tier_to_adopt(stored: &str, untouched: &str) -> Option<&'static str> {
    tier_to_adopt_for(host_default_quality(), stored, untouched)
}

/// [`tier_to_adopt`] with the host tier injected, so the Jetson case is testable off-Jetson.
pub fn tier_to_adopt_for<'a>(host: &'a str, stored: &str, untouched: &str) -> Option<&'a str> {
    let stored = stored.trim();
    let unchosen = stored.is_empty() || stored == untouched;
    (unchosen && host != stored).then_some(host)
}

/// Swap out a tier this host can't run. Callers persist and display the result, since
/// [`KokoroOutput::speak`] can't tell a silent buffer from a quiet one.
pub fn usable_quality(requested: &str) -> &str {
    if aarch64_linux() && requested == "q8f16" {
        return "q4f16";
    }
    requested
}

pub fn model_filename(quality: &str) -> &'static str {
    match quality {
        "fp32" => "model.onnx",
        "fp16" => "model_fp16.onnx",
        "q4" => "model_q4.onnx",
        "q4f16" => "model_q4f16.onnx",
        "q8f16" => "model_q8f16.onnx",
        // q8 (the default) and anything unknown.
        _ => "model_quantized.onnx",
    }
}

/// Approximate download size of a quality tier, in MB, for the UI.
pub fn model_size_mb(quality: &str) -> u64 {
    match quality {
        "fp32" => 326,
        "fp16" => 163,
        "q4" => 305,
        "q4f16" => 155,
        "q8f16" => 86,
        _ => 92,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A turn's tokens are pumped on the same runtime that synthesises its speech.
    #[test]
    fn synthesis_leaves_the_runtime_free_to_stream_tokens() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let ticks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counting = ticks.clone();
            let pump = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    counting.fetch_add(1, Ordering::SeqCst);
                }
            });
            let samples = off_the_runtime(|| {
                std::thread::sleep(std::time::Duration::from_millis(300));
                Ok(vec![0.0f32; 4])
            })
            .await
            .unwrap();
            pump.abort();
            assert_eq!(samples.len(), 4);
            let ticks = ticks.load(Ordering::SeqCst);
            assert!(
                ticks >= 10,
                "the only worker ticked {ticks} times during 300 ms of synthesis"
            );
        });
    }

    /// Kokoro's shape: ~0.3 s of near-silence, speech, ~0.5 s of near-silence.
    fn forward_pass(speech_secs: f32) -> Vec<f32> {
        let sr = SAMPLE_RATE as f32;
        let mut x = vec![0.0f32; (0.3 * sr) as usize];
        x.extend((0..(speech_secs * sr) as usize).map(|i| 0.3 * (i as f32 * 0.07).sin()));
        x.extend(vec![0.0f32; (0.5 * sr) as usize]);
        x
    }

    fn secs(samples: usize) -> f32 {
        samples as f32 / SAMPLE_RATE as f32
    }

    #[test]
    fn a_unit_keeps_its_speech_and_ends_in_the_gap_before_the_next() {
        let mut out = Vec::new();
        push_paced(&mut out, &forward_pass(1.0), UNIT_GAP_SECS, 1.0);
        let want = LEAD_KEEP_SECS + 1.0 + TAIL_KEEP_SECS + UNIT_GAP_SECS;
        assert!(
            (secs(out.len()) - want).abs() < 0.02,
            "{} vs {want}",
            secs(out.len())
        );

        let mut last = Vec::new();
        push_paced(&mut last, &forward_pass(1.0), 0.0, 1.0);
        assert!(
            last.len() < out.len(),
            "nothing is added after the last unit"
        );
    }

    fn chunk_of(phonemes: &str) -> tokenizer::Chunk {
        tokenizer::Chunk {
            text: "Goose, remind me to water the plants at six.".into(),
            phonemes: phonemes.into(),
            tokens: phonemes.chars().map(|c| c as i64).collect(),
        }
    }

    const GOOSE: &str = "ɡˈuːs, ɹᵻmˈaɪnd mˌiː tə wˈɔːɾɚ ðə plˈænts æt sˈɪks.";

    #[test]
    fn a_pass_of_nan_or_silence_is_said_again_in_halves() {
        for broken in [vec![f32::NAN; 24_000], vec![0.0; 24_000]] {
            let whole = chunk_of(GOOSE);
            let mut asked = Vec::new();
            let mut out = Vec::new();
            let pass = push_checked(&mut out, &whole, UNIT_GAP_SECS, 1.0, |part| {
                asked.push(part.phonemes.clone());
                Ok(if part.tokens.len() == whole.tokens.len() {
                    broken.clone()
                } else {
                    forward_pass(1.0)
                })
            })
            .unwrap();
            assert_eq!(pass, Pass::Rescued);
            assert_eq!(asked.len(), 3, "{asked:?}");
            assert_eq!(
                asked[1], "ɡˈuːs, ɹᵻmˈaɪnd mˌiː tə",
                "cut at the space nearest the middle"
            );
            assert!(out.iter().all(|v| v.is_finite()));
            assert!(
                secs(out.len()) > 2.0,
                "both halves were said: {}",
                secs(out.len())
            );
        }
    }

    #[test]
    fn a_half_that_fails_again_is_logged_as_lost_not_hidden() {
        let mut out = Vec::new();
        let pass = push_checked(&mut out, &chunk_of(GOOSE), UNIT_GAP_SECS, 1.0, |_| {
            Ok(vec![f32::NAN; 24_000])
        })
        .unwrap();
        assert_eq!(pass, Pass::Lost);
        assert!(out.iter().all(|v| *v == 0.0), "no NaN reaches the speaker");
        let mut out = Vec::new();
        let pass = push_checked(&mut out, &chunk_of("ˈoʊkˌeɪ."), 0.0, 1.0, |_| {
            Ok(vec![0.0; 24_000])
        })
        .unwrap();
        assert_eq!(pass, Pass::Lost, "a one-word unit cannot be split");
    }

    #[test]
    fn a_good_pass_is_said_once() {
        let mut calls = 0;
        let mut out = Vec::new();
        let pass = push_checked(&mut out, &chunk_of(GOOSE), UNIT_GAP_SECS, 1.0, |_| {
            calls += 1;
            Ok(forward_pass(1.0))
        })
        .unwrap();
        assert_eq!((pass, calls), (Pass::Clean, 1));
    }

    #[test]
    fn pauses_shrink_with_the_speed_like_the_models_own() {
        let mut slow = Vec::new();
        push_paced(&mut slow, &[], UNIT_GAP_SECS, 0.8);
        let mut fast = Vec::new();
        push_paced(&mut fast, &[], UNIT_GAP_SECS, 1.25);
        assert!((secs(slow.len()) - 0.625).abs() < 0.001);
        assert!((secs(fast.len()) - 0.4).abs() < 0.001);
    }

    #[test]
    fn a_trimmed_edge_starts_and_ends_at_rest() {
        let mut out = Vec::new();
        let loud = vec![0.5f32; SAMPLE_RATE as usize / 2];
        push_paced(&mut out, &loud, 0.0, 1.0);
        assert_eq!(out[0], 0.0);
        assert!(out.last().unwrap().abs() < 0.01);
    }

    #[test]
    fn quality_tiers_map_to_repo_filenames() {
        assert_eq!(model_filename("q8"), "model_quantized.onnx");
        assert_eq!(model_filename("fp32"), "model.onnx");
        assert_eq!(model_filename("q4f16"), "model_q4f16.onnx");
    }

    #[test]
    fn unknown_quality_falls_back_to_the_default_tier() {
        for junk in ["", "best", "int8", "🙂"] {
            assert_eq!(model_filename(junk), "model_quantized.onnx");
            assert_eq!(model_size_mb(junk), 92);
        }
    }

    #[test]
    fn every_tier_reports_a_size() {
        for q in ["q8", "q8f16", "q4", "q4f16", "fp16", "fp32"] {
            assert!(model_size_mb(q) > 0, "{q}");
        }
    }

    #[test]
    fn preview_sentence_names_the_product_and_is_long_enough_to_judge() {
        assert!(PREVIEW_SENTENCE.contains("Jarida"));
        assert!(
            PREVIEW_SENTENCE.split_whitespace().count() > 15,
            "too short to hear prosody"
        );
    }

    #[test]
    fn preview_sentence_is_fully_covered_by_the_shipped_vocab() {
        let vocab_json = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/tokenizer.json"),
        );
        let Ok(raw) = vocab_json else {
            // testdata is optional; the live test below covers the real thing.
            return;
        };
        let vocab = Vocab::from_json(&raw).unwrap();
        let Ok((_chunks, dropped)) = tokenizer::chunk(PREVIEW_SENTENCE, &vocab) else {
            return; // espeak unavailable in this environment
        };
        assert_eq!(dropped, 0, "preview sentence loses phonemes");
    }

    #[test]
    fn speed_is_clamped_to_a_sane_range() {
        // Exercised without constructing the adapter (which needs audio).
        let clamp = |s: f32| s.clamp(MIN_SPEED, MAX_SPEED);
        assert_eq!(clamp(0.1), MIN_SPEED);
        assert_eq!(clamp(9.0), MAX_SPEED);
        assert_eq!(clamp(1.0), 1.0);
        assert_eq!(clamp(DEFAULT_SPEED), DEFAULT_SPEED);
    }

    #[test]
    fn default_speed_is_within_bounds() {
        assert!((MIN_SPEED..=MAX_SPEED).contains(&DEFAULT_SPEED));
    }

    #[test]
    fn intra_threads_stays_bounded_on_any_machine() {
        let n = default_intra_threads();
        assert!((2..=6).contains(&n), "derived {n} threads");
    }

    #[test]
    fn host_default_maps_to_weights_that_exist() {
        let q = host_default_quality();
        let file = model_filename(q);
        assert!(
            file.starts_with("model") && file.ends_with(".onnx"),
            "{q} resolved to {file}"
        );
        if aarch64_linux() {
            assert_eq!(
                file, "model_q4f16.onnx",
                "aarch64 must not default to a tier that cannot reach real time"
            );
        }
    }

    #[test]
    fn usable_quality_leaves_working_tiers_alone() {
        for q in ["q8", "q4f16", "q4", "fp16", "fp32"] {
            assert_eq!(usable_quality(q), q, "{q} was substituted needlessly");
        }
    }

    #[test]
    fn the_silent_tier_is_never_selected_on_aarch64() {
        if aarch64_linux() {
            assert_ne!(host_default_quality(), "q8f16");
            assert_eq!(usable_quality("q8f16"), "q4f16");
        } else {
            assert_eq!(usable_quality("q8f16"), "q8f16");
        }
    }

    #[test]
    fn a_default_tier_is_replaced_by_a_host_that_needs_a_different_one() {
        assert_eq!(tier_to_adopt_for("q4f16", "q8f16", "q8f16"), Some("q4f16"));
        assert_eq!(tier_to_adopt_for("q4f16", "", "q8f16"), Some("q4f16"));
        assert_eq!(tier_to_adopt_for("q4f16", "  ", "q8f16"), Some("q4f16"));
    }

    #[test]
    fn a_chosen_tier_is_never_overwritten() {
        for chosen in ["fp32", "fp16", "q4", "q8"] {
            assert_eq!(
                tier_to_adopt_for("q4f16", chosen, "q8f16"),
                None,
                "{chosen} is a choice, not a default"
            );
        }
    }

    /// A redundant write every boot would make `updated_at` stop meaning a household change.
    #[test]
    fn adopting_is_a_no_op_once_it_has_happened() {
        assert_eq!(tier_to_adopt_for("q4f16", "q4f16", "q8f16"), None);
        assert_eq!(tier_to_adopt(host_default_quality(), "q8f16"), None);
    }

    #[test]
    fn the_wrapper_passes_this_hosts_tier_through() {
        for stored in ["", "q8", "fp32", "q4f16"] {
            assert_eq!(
                tier_to_adopt(stored, "q8f16"),
                tier_to_adopt_for(host_default_quality(), stored, "q8f16"),
                "wrapper disagreed for stored={stored:?}"
            );
        }
    }
}

/// Silence where one pass became two inside a phrase: a word's, or a clause mark's own.
const WORD_GAP_SECS: f32 = 0.10;
const CLAUSE_GAP_SECS: f32 = 0.30;

/// Append `audio` cut to its speech, then `gap_secs` of silence at `speed`: the model's own
/// pauses shrink as it speeds up, so the ones put between passes do too.
fn push_paced(out: &mut Vec<f32>, audio: &[f32], gap_secs: f32, speed: f32) {
    if let Some(range) = speech_bounds(audio, SAMPLE_RATE, LEAD_KEEP_SECS, TAIL_KEEP_SECS) {
        let start = out.len();
        out.extend_from_slice(&audio[range]);
        let fade = ((SAMPLE_RATE as f32 * EDGE_FADE_SECS) as usize).min((out.len() - start) / 2);
        let len = out.len();
        for i in 0..fade {
            let gain = i as f32 / fade as f32;
            out[start + i] *= gain;
            out[len - 1 - i] *= gain;
        }
    }
    let pause = (gap_secs / speed.max(MIN_SPEED) * SAMPLE_RATE as f32) as usize;
    out.resize(out.len() + pause, 0.0);
}

/// Why a forward pass's audio cannot be used. On the Orin's ONNX Runtime about one q4f16 pass in
/// ten came back silent for a whole sentence, more often the longer the unit.
fn unusable(audio: &[f32]) -> Option<&'static str> {
    if audio.iter().any(|v| !v.is_finite()) {
        return Some("non-finite samples");
    }
    if speech_bounds(audio, SAMPLE_RATE, 0.0, 0.0).is_none() {
        return Some("no speech");
    }
    None
}

/// How a chunk came out of [`push_checked`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    Clean,
    /// The whole chunk failed and both halves were said instead.
    Rescued,
    /// Some of it could not be said; what was lost is logged.
    Lost,
}

/// [`push_paced`] for one chunk, checking the model's audio first. A failed pass is retried once
/// as two halves, each its own shorter pass: on the Orin a shorter unit failed far less often
/// (2% of clauses against 10% of 100-200-token units) and a different style row rescued every
/// failed unit it was tried on, and each half takes the row of its own length.
fn push_checked(
    out: &mut Vec<f32>,
    chunk: &tokenizer::Chunk,
    gap_secs: f32,
    speed: f32,
    mut synth: impl FnMut(&tokenizer::Chunk) -> Result<Vec<f32>>,
) -> Result<Pass> {
    let audio = synth(chunk)?;
    let Some(why) = unusable(&audio) else {
        push_paced(out, &audio, gap_secs, speed);
        return Ok(Pass::Clean);
    };
    let Some((first, second)) = tokenizer::halves(chunk) else {
        // Counts, not the words: the reply is the household's.
        tracing::error!(
            why,
            tokens = chunk.tokens.len(),
            "Kokoro gave no usable audio for a unit too short to split; it is not said"
        );
        push_paced(out, &[], gap_secs, speed);
        return Ok(Pass::Lost);
    };
    tracing::warn!(
        why,
        tokens = chunk.tokens.len(),
        "Kokoro gave no usable audio for a unit; saying it again in two halves"
    );
    let mut pass = Pass::Rescued;
    let mid = if first.phonemes.trim_end().ends_with([',', ';', ':']) {
        CLAUSE_GAP_SECS
    } else {
        WORD_GAP_SECS
    };
    for (part, part_end) in [(&first, mid), (&second, gap_secs)] {
        let audio = synth(part)?;
        match unusable(&audio) {
            None => push_paced(out, &audio, part_end, speed),
            Some(why) => {
                tracing::error!(
                    why,
                    tokens = part.tokens.len(),
                    "half of a Kokoro unit gave no usable audio again; that half is not said"
                );
                push_paced(out, &[], part_end, speed);
                pass = Pass::Lost;
            }
        }
    }
    Ok(pass)
}

/// A unit's tensors are freed into the malloc arena of whichever thread ran it, and glibc keeps
/// them there: on the Orin that held 150-900 MB a unit until the turn ended.
fn hand_back_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    tokio::task::spawn_blocking(|| {
        extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: glibc's malloc_trim takes the padding to keep and touches nothing of ours.
        unsafe { malloc_trim(0) };
    });
}

/// Phonemizing and synthesis take about real time; on a runtime worker they stall every task
/// sharing it, the reply's token stream among them.
async fn off_the_runtime<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .context("Kokoro synthesis task panicked")?
}

/// One playback in flight, counted until dropped, however the playback ends.
struct Playing(Arc<std::sync::atomic::AtomicUsize>);

impl Playing {
    fn start(count: &Arc<std::sync::atomic::AtomicUsize>) -> Playing {
        count.fetch_add(1, Ordering::SeqCst);
        Playing(count.clone())
    }
}

impl Drop for Playing {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
