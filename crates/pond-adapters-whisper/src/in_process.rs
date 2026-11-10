//! In-process Whisper ASR via `whisper-rs`.
//! Shares ggml's CUDA primary context with `llama-cpp-2`, so Jetson keeps one CUDA context.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use pond_core::models::domain::gpu_gate::gpu_gate;
use pond_core::models::ports::voice_input::{SpeculativeSignal, VoiceInput};
use pond_voice::dsp::{RmsDetector, SpeechDetector};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::{
    decode_wav_mono_f32, record_mono_f32_until_silence, record_mono_f32_vad, resample_to_16k,
    strip_whisper_artifacts, SpeculativeSpawn, ThrottledAudioLevelSink, WhisperBackend,
};

/// Outcome of the blocking audio-capture step in `listen()`.
enum SpeechCapture {
    /// No speech detected within the onset wait — nothing to transcribe.
    Empty,
    /// A matching speculative transcript was already computed; use it, skip re-inference. The
    /// 16 kHz samples ride along for a model that hears them.
    Transcript(String, Vec<f32>),
    /// No usable speculative transcript (e.g. hit the hard cap first); transcribe normally.
    Samples(Vec<f32>),
}

/// Maximum recording duration (hard cap). VAD usually stops earlier.
const DEFAULT_DURATION_SECS: u32 = 30;
/// Silence (ms) after speech to declare end-of-utterance.
const DEFAULT_SILENCE_MS: u64 = 800;
/// RMS below which a frame is silence, for the default detector.
const END_OF_SPEECH_RMS: f32 = 0.005;
/// Wait window for speech onset before giving up.
const DEFAULT_ONSET_WAIT_SECS: u32 = 10;

/// A loaded model and the states it transcribes with, one per decode profile.
///
/// A state is kept and reused rather than created for every call. Creating one allocates its
/// KV caches and compute buffers on the GPU, and its first decode reserves GGML's CUDA scratch
/// pool and, for beam search, grows its self-attention cache. On the Jetson the LLM shares that
/// memory, and a call that found none failed in ways that took the Pond down: a double free
/// when the cache could not grow (fixed in the vendored whisper.cpp) and an abort when the pool
/// could not be reserved, which GGML does by design. A kept state makes every one of those
/// allocations once, so a transcription allocates nothing. The cost is that the memory stays
/// reserved between calls; it is measured in `docs/voice-pipeline-efficiency.md`.
struct Engine {
    context: Arc<WhisperContext>,
    /// For [`TranscribeOpts::accurate`] and any other beam-search profile.
    accurate: Mutex<Option<WhisperState>>,
    /// For [`TranscribeOpts::wake_word`] and any other greedy profile.
    greedy: Mutex<Option<WhisperState>>,
    /// States created over this engine's life, logged with each new one: a count that keeps
    /// rising means states are being lost to errors and the allocations are back on the call path.
    created: AtomicUsize,
}

impl Engine {
    /// Wrap `context` and prepare `prepare`'s state now, at load, with one decode of a second of
    /// silence. The other profile's state is made on its first use, if it ever comes.
    ///
    /// A failure here is logged and leaves the state to be made on first use, as before.
    fn new(context: WhisperContext, prepare: TranscribeOpts) -> Self {
        let engine = Self {
            context: Arc::new(context),
            accurate: Mutex::new(None),
            greedy: Mutex::new(None),
            created: AtomicUsize::new(0),
        };
        let started = std::time::Instant::now();
        match WhisperRsInput::transcribe_samples_with(&engine, vec![0.0; 16_000], prepare) {
            Ok(_) => tracing::info!(
                kind = "whisper_state_prepared",
                elapsed_ms = started.elapsed().as_millis() as u64,
                "whisper is ready to transcribe without allocating"
            ),
            Err(error) => tracing::warn!(
                kind = "whisper_state_not_prepared",
                error = format!("{error:#}"),
                "whisper could not prepare its state at load; the first transcription will"
            ),
        }
        engine
    }

    /// The slot for `opts`: beam search and greedy decoding size their caches differently.
    fn slot(&self, opts: &TranscribeOpts) -> (&Mutex<Option<WhisperState>>, &'static str) {
        match opts.beam_size {
            Some(_) => (&self.accurate, "accurate"),
            None => (&self.greedy, "greedy"),
        }
    }
}

/// In-process Whisper adapter. One loaded model per instance.
pub struct WhisperRsInput {
    /// Swapped by `rebuild_with`; in-flight transcriptions keep their `Arc` of the old one.
    engine: RwLock<Arc<Engine>>,
    /// Last-known model path, recorded for diagnostics.
    model_path: RwLock<PathBuf>,
    /// Hard cap on recording time (seconds). VAD ends earlier on silence.
    duration_secs: u32,
    /// Consecutive silence (ms) that ends a recording.
    silence_ms: u64,
    /// Pre-captured WAV bytes from the wake-word detector (one-breath path).
    captured: Mutex<Option<Vec<u8>>>,
    /// The 16 kHz samples behind the last `listen()` that returned words.
    heard: Mutex<Option<Vec<f32>>>,
    /// When speech that barged in over a reply began; the next capture starts there.
    onset: Mutex<Option<std::time::Instant>>,
    /// Normalized wake words, stripped because the capture reaches back past the trigger.
    /// `std` lock: it is set from inside the runtime, where tokio's `blocking_write` panics.
    wake_words: std::sync::RwLock<Vec<String>>,
    /// Optional live mic-level reporter, fed from the VAD recording loop.
    audio_level_sink: Option<Arc<ThrottledAudioLevelSink>>,
    /// Shared mic owner; all captures go through it so none races the wake-word detector.
    mic: pond_audio::MicHandle,
    /// Shared by both capture paths; long-lived since a model detector loads an ONNX session.
    /// It outlives the turn, so each capture must `reset()` it first.
    detector: Arc<std::sync::Mutex<Box<dyn SpeechDetector + Send>>>,
}

impl WhisperRsInput {
    /// Replace the speech detector both capture paths use.
    /// Takes a built detector: this crate is in CI's fast set and must not depend on `ort`.
    pub fn set_speech_detector(&self, detector: Box<dyn SpeechDetector + Send>) {
        match self.detector.lock() {
            Ok(mut slot) => *slot = detector,
            // A capture thread panicked; the detector is replaceable state, so swap it anyway.
            Err(poisoned) => *poisoned.into_inner() = detector,
        }
    }

    /// Load the ggml model at `model_path`. A whisper-rs panic during load becomes `Err`.
    pub fn new(model_path: PathBuf, mic: pond_audio::MicHandle) -> Result<Self> {
        Self::prepared(model_path, mic, TranscribeOpts::accurate())
    }

    fn prepared(
        model_path: PathBuf,
        mic: pond_audio::MicHandle,
        prepare: TranscribeOpts,
    ) -> Result<Self> {
        if !model_path.exists() {
            return Err(anyhow!(
                "Whisper model file not found: {}",
                model_path.display()
            ));
        }
        let context = load_context(&model_path)?;
        tracing::info!(
            "WhisperRsInput loaded model: {} (in-process whisper.cpp)",
            model_path.display()
        );
        Ok(Self {
            engine: RwLock::new(Arc::new(Engine::new(context, prepare))),
            model_path: RwLock::new(model_path),
            duration_secs: DEFAULT_DURATION_SECS,
            silence_ms: DEFAULT_SILENCE_MS,
            captured: Mutex::new(None),
            heard: Mutex::new(None),
            onset: Mutex::new(None),
            wake_words: std::sync::RwLock::new(Vec::new()),
            audio_level_sink: None,
            mic,
            detector: Arc::new(Mutex::new(Box::new(RmsDetector::new(END_OF_SPEECH_RMS)))),
        })
    }

    /// Set the wake words to strip; pass the detector's resolved trigger list so the two match.
    pub fn set_wake_words(&self, variants: &[String]) {
        let normalized: Vec<String> = variants
            .iter()
            .map(|v| pond_voice::text::normalize_transcript(v))
            .filter(|v| !v.is_empty())
            .collect();
        tracing::debug!("ASR: stripping wake words {:?}", normalized);
        *self.wake_words.write().unwrap_or_else(|e| e.into_inner()) = normalized;
    }

    /// Override the maximum recording duration.
    pub fn with_duration(mut self, secs: u32) -> Self {
        self.duration_secs = secs;
        self
    }

    /// Override the end-of-speech silence threshold.
    pub fn with_silence_ms(mut self, ms: u64) -> Self {
        self.silence_ms = ms;
        self
    }

    /// Report live mic RMS through `sink` during the onset wait and recording.
    pub fn with_audio_level_sink(mut self, sink: Arc<ThrottledAudioLevelSink>) -> Self {
        self.audio_level_sink = Some(sink);
        self
    }

    /// Hot-swap the model (old one kept on `Err`); in-flight calls finish on the old context.
    pub async fn rebuild_with(&self, new_model_path: PathBuf) -> Result<()> {
        if !new_model_path.exists() {
            return Err(anyhow!(
                "Whisper model file not found: {}",
                new_model_path.display()
            ));
        }
        let new_engine = tokio::task::spawn_blocking({
            let p = new_model_path.clone();
            move || load_context(&p).map(|c| Engine::new(c, TranscribeOpts::accurate()))
        })
        .await
        .map_err(|e| anyhow!("model load join error: {}", e))??;

        let mut engine_guard = self.engine.write().await;
        *engine_guard = Arc::new(new_engine);
        drop(engine_guard);

        let mut path_guard = self.model_path.write().await;
        *path_guard = new_model_path.clone();

        tracing::info!(
            "WhisperRsInput hot-swapped to: {}",
            new_model_path.display()
        );
        Ok(())
    }

    /// The decode profiles that hold a kept state now, and how many states the loaded model has
    /// made; `None` while the model is being swapped. A state in use counts as held.
    pub fn held_states(&self) -> Option<(Vec<&'static str>, usize)> {
        let engine = self.engine.try_read().ok()?.clone();
        let held = |slot: &Mutex<Option<WhisperState>>| match slot.try_lock() {
            Ok(state) => state.is_some(),
            Err(std::sync::TryLockError::WouldBlock) => true,
            Err(std::sync::TryLockError::Poisoned(state)) => state.into_inner().is_some(),
        };
        let mut profiles = Vec::new();
        if held(&engine.accurate) {
            profiles.push("accurate");
        }
        if held(&engine.greedy) {
            profiles.push("greedy");
        }
        Some((profiles, engine.created.load(Ordering::Relaxed)))
    }

    pub async fn current_model_path(&self) -> PathBuf {
        self.model_path.read().await.clone()
    }

    /// Decode WAV bytes, resample to 16 kHz mono, and transcribe in-process.
    pub fn transcribe_wav_bytes(&self, wav_bytes: &[u8]) -> Result<String> {
        let (samples, rate) = crate::decode_wav_mono_f32(wav_bytes)?;
        let samples_16k = crate::resample_to_16k(&samples, rate);
        let engine = self.engine.blocking_read().clone();
        Self::transcribe_samples(&engine, samples_16k)
    }

    /// 16 kHz mono PCM through the accurate profile, the wake word left in. Blocking: call it
    /// from a blocking thread, where `blocking_read` is safe.
    pub fn transcribe_pcm(&self, samples: &[f32]) -> Result<String> {
        self.transcribe_pcm_prompted(samples, None)
    }

    /// As [`Self::transcribe_pcm`], with `prompt` read by Whisper as the text before the audio.
    pub fn transcribe_pcm_prompted(&self, samples: &[f32], prompt: Option<&str>) -> Result<String> {
        self.transcribe_pcm_with(samples, TranscribeOpts::accurate(), prompt)
    }

    /// As [`Self::transcribe_pcm_prompted`], through `opts`.
    pub fn transcribe_pcm_with(
        &self,
        samples: &[f32],
        opts: TranscribeOpts,
        prompt: Option<&str>,
    ) -> Result<String> {
        let engine = self.engine.blocking_read().clone();
        let text =
            Self::transcribe_unless_abandoned(&engine, samples.to_vec(), opts, None, prompt)?;
        Ok(match prompt {
            Some(prompt) if is_prompt_echo(&text, prompt) => String::new(),
            _ => text,
        })
    }

    /// Transcribe 16 kHz mono PCM (artifacts stripped); empty → `Ok("")`, panic → `Err`.
    fn transcribe_samples(engine: &Engine, samples: Vec<f32>) -> Result<String> {
        Self::transcribe_samples_with(engine, samples, TranscribeOpts::accurate())
    }

    /// As [`Self::transcribe_samples`], with per-call cost tuning.
    ///
    /// Calls with the same profile share one state, so they run one at a time; a second call
    /// waits for the first. Whisper serialises on the GPU anyway.
    fn transcribe_samples_with(
        engine: &Engine,
        samples: Vec<f32>,
        opts: TranscribeOpts,
    ) -> Result<String> {
        Self::transcribe_unless_abandoned(engine, samples, opts, None, None)
    }

    /// As [`Self::transcribe_samples_with`], giving up once `abandoned` is raised: before
    /// decoding if it was raised while this waited for the state, else at the next decode step.
    fn transcribe_unless_abandoned(
        engine: &Engine,
        samples: Vec<f32>,
        opts: TranscribeOpts,
        abandoned: Option<&AtomicBool>,
        prompt: Option<&str>,
    ) -> Result<String> {
        let is_abandoned = || abandoned.is_some_and(|flag| flag.load(Ordering::SeqCst));
        if samples.is_empty() {
            return Ok(String::new());
        }
        let mut samples = samples;
        let peak = samples.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        if peak > 0.005 && peak < 0.25 {
            let scale = (0.25 / peak).min(8.0);
            for s in &mut samples {
                *s *= scale;
            }
        }
        let n_threads = opts.n_threads.unwrap_or_else(default_threads);
        let sample_count = samples.len();
        // whisper.cpp refuses an `audio_ctx` above the model's own, which the wake model lowers.
        let audio_ctx = opts
            .fit_audio_ctx
            .then(|| audio_ctx_for(sample_count).min(engine.context.model_n_audio_ctx()));

        let (slot, profile) = engine.slot(&opts);
        // A panic mid-decode leaves the state in an unknown condition, so it is replaced.
        let mut held = slot.lock().unwrap_or_else(|poisoned| {
            // Forget the poisoning once the state is dropped, or every later call would land
            // here again and rebuild the state this slot exists to keep.
            slot.clear_poison();
            let mut held = poisoned.into_inner();
            *held = None;
            held
        });
        if is_abandoned() {
            return Err(anyhow!(ABANDONED));
        }

        // Catches Rust panics from whisper-rs; a C++ failure in whisper.cpp aborts regardless.
        let result = catch_unwind(AssertUnwindSafe(|| -> Result<String> {
            let state = match held.as_mut() {
                Some(state) => state,
                None => {
                    let state = engine
                        .context
                        .create_state()
                        .context("whisper-rs: create_state failed")?;
                    // Counted outside the log macro: its fields are evaluated only when
                    // something is listening.
                    let created = engine.created.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::info!(
                        kind = "whisper_state_created",
                        profile,
                        created,
                        "whisper made a state to keep for this profile"
                    );
                    held.insert(state)
                }
            };

            let strategy = match opts.beam_size {
                Some(beam_size) => SamplingStrategy::BeamSearch {
                    beam_size,
                    patience: -1.0, // whisper.cpp default
                },
                None => SamplingStrategy::Greedy { best_of: 1 },
            };
            // Tokenised per call and borrowed: `set_initial_prompt` leaks its string every call.
            let prompt_tokens = match prompt {
                Some(text) => engine
                    .context
                    .tokenize(text, MAX_PROMPT_TOKENS)
                    .context("whisper-rs: the prompt did not tokenize")?,
                None => Vec::new(),
            };
            let mut params = FullParams::new(strategy);
            if !prompt_tokens.is_empty() {
                params.set_tokens(&prompt_tokens);
            }
            params.set_print_progress(false);
            params.set_print_special(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            // English models are the only ones shipped by GIAP today.
            params.set_language(Some("en"));
            params.set_translate(false);
            params.set_no_context(true);
            params.set_suppress_blank(true);
            params.set_suppress_nst(opts.suppress_non_speech);
            params.set_temperature(0.0);
            params.set_temperature_inc(opts.temperature_step);
            params.set_n_threads(n_threads);
            if let Some(ctx_frames) = audio_ctx {
                params.set_audio_ctx(ctx_frames);
            }
            // Not `set_abort_callback_safe`: in whisper-rs 0.16 its trampoline reads the boxed
            // closure as the wrong type, and the box is never freed.
            if let Some(flag) = abandoned {
                // SAFETY: `flag` outlives `state.full` below, the only call that reads it, and
                // `abort_when_raised` only loads it.
                unsafe {
                    params.set_abort_callback(Some(abort_when_raised));
                    params.set_abort_callback_user_data(
                        flag as *const AtomicBool as *mut std::ffi::c_void,
                    );
                }
            }

            let _gpu = gpu_gate().compute_blocking();
            state
                .full(params, &samples)
                .context("whisper-rs: full inference failed")?;

            let n_segments = state.full_n_segments();
            let mut out = String::new();
            for i in 0..n_segments {
                if let Some(seg) = state.get_segment(i) {
                    if seg.no_speech_probability() > 0.6 {
                        tracing::debug!(
                            segment = i,
                            no_speech_prob = seg.no_speech_probability(),
                            "whisper: dropping segment with high no_speech_probability"
                        );
                        continue;
                    }
                    if let Ok(text) = seg.to_str() {
                        out.push_str(text);
                    }
                }
            }
            Ok(strip_whisper_artifacts(&out))
        }));

        match result {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(_)) if is_abandoned() => {
                tracing::debug!(
                    kind = "whisper_transcription_abandoned",
                    samples = sample_count,
                    "whisper stopped a transcription nobody is waiting for"
                );
                Err(anyhow!(ABANDONED))
            }
            // Most often the GPU had no room for something this call needed. The state is kept:
            // a failed decode leaves it usable, and making a new one would allocate again.
            Ok(Err(e)) => {
                tracing::warn!(
                    kind = "whisper_transcription_failed",
                    error = format!("{e:#}"),
                    samples = sample_count,
                    "whisper could not transcribe this audio"
                );
                Err(e)
            }
            Err(panic) => {
                *held = None;
                let msg = panic_message(&panic);
                tracing::error!("whisper-rs panic caught: {}", msg);
                Err(anyhow!("whisper-rs panic: {}", msg))
            }
        }
    }
}

const ABANDONED: &str = "transcription abandoned";

/// Over noise Whisper can say its prompt back, once or over and over, the last copy cut short, or
/// loop just its opening ("Hey goose. Hey goose. Hey goose."); nobody says it that way.
fn is_prompt_echo(text: &str, prompt: &str) -> bool {
    let heard = pond_voice::text::normalize_transcript(text);
    let prompt = pond_voice::text::normalize_transcript(prompt);
    let (heard, prompt): (Vec<&str>, Vec<&str>) = (
        heard.split_whitespace().collect(),
        prompt.split_whitespace().collect(),
    );
    let said_over = |words: usize, copies: usize| {
        heard.len() >= words * copies
            && heard
                .iter()
                .enumerate()
                .all(|(i, w)| *w == prompt[i % words])
    };
    !prompt.is_empty()
        && (said_over(prompt.len(), 1)
            || (1..prompt.len()).any(|words| said_over(words, LOOPED_OPENING)))
}

/// Copies of the prompt's opening that make a loop: a person may call the pond twice in a breath.
const LOOPED_OPENING: usize = 3;
/// Whisper keeps only the last half of its 448-token text context as the prompt.
const MAX_PROMPT_TOKENS: usize = 224;

/// whisper.cpp's abort hook, checked after the encoder and after each decode step.
unsafe extern "C" fn abort_when_raised(flag: *mut std::ffi::c_void) -> bool {
    // SAFETY: the user data installed beside this hook is a live `&AtomicBool`.
    unsafe { &*(flag as *const AtomicBool) }.load(Ordering::SeqCst)
}

/// Route whisper.cpp's stderr logging into tracing; run on model load so no binary forgets it.
fn install_whisper_logging() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(whisper_rs::install_logging_hooks);
}

/// Remove a leading wake word; both transcript paths must use it or their outputs differ.
fn strip_wake_words(transcript: String, wake_words: &[String]) -> String {
    if wake_words.is_empty() || transcript.is_empty() {
        return transcript;
    }
    let stripped = pond_voice::text::strip_leading_wake_word(&transcript, wake_words);
    if stripped != transcript {
        tracing::debug!("ASR: wake word removed, command is {:?}", stripped);
    }
    stripped
}

/// Load a whisper.cpp context with platform-appropriate GPU settings.
fn load_context(model_path: &Path) -> Result<WhisperContext> {
    install_whisper_logging();
    let path_str = model_path.to_string_lossy().to_string();
    let mut params = WhisperContextParameters::default();
    apply_platform_params(&mut params);

    let result = catch_unwind(AssertUnwindSafe(|| {
        WhisperContext::new_with_params(&path_str, params)
    }));
    match result {
        Ok(Ok(ctx)) => Ok(ctx),
        Ok(Err(e)) => Err(anyhow!("whisper-rs load failed: {}", e)),
        Err(panic) => {
            let msg = panic_message(&panic);
            Err(anyhow!("whisper-rs load panic: {}", msg))
        }
    }
}

/// Best-effort message extraction from a `catch_unwind` payload.
fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = panic.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic payload>".to_string()
}

/// Platform tuning, mirroring `apply_platform_settings` in pond-adapters-local-inference.
#[cfg(feature = "cuda")]
fn apply_platform_params(params: &mut WhisperContextParameters) {
    params.use_gpu(true).flash_attn(true);
    tracing::info!("WhisperRsInput: CUDA backend enabled (flash_attn on)");
}

#[cfg(all(not(feature = "cuda"), target_os = "macos"))]
fn apply_platform_params(params: &mut WhisperContextParameters) {
    params.use_gpu(true).flash_attn(true);
    tracing::info!("WhisperRsInput: Metal backend enabled (flash_attn on)");
}

#[cfg(all(not(feature = "cuda"), not(target_os = "macos")))]
fn apply_platform_params(params: &mut WhisperContextParameters) {
    params.use_gpu(false);
    tracing::info!("WhisperRsInput: CPU backend (no GPU feature compiled in)");
}

/// Per-call whisper tuning: wake-word detection optimises for cost, commands for accuracy.
#[derive(Debug, Clone, Copy)]
pub struct TranscribeOpts {
    /// Threads for this call; `None` uses [`default_threads`].
    pub n_threads: Option<std::os::raw::c_int>,
    /// Cap the encoder's mel context to the clip length instead of padding to 30 s.
    pub fit_audio_ctx: bool,
    /// Beam width; `None` decodes greedily. The biggest accuracy lever, at ~width× the compute.
    pub beam_size: Option<std::os::raw::c_int>,
    /// Drop noise annotations (`[BLANK_AUDIO]`, `(wind blowing)`) so they never reach the model.
    pub suppress_non_speech: bool,
    /// Retry temperature step for segments under whisper's confidence floor; `0.0` disables it.
    pub temperature_step: f32,
}

impl TranscribeOpts {
    /// What the user's actual speech gets: every accuracy lever, once a turn.
    pub fn accurate() -> Self {
        Self {
            n_threads: None,
            fit_audio_ctx: false,
            beam_size: Some(5),
            suppress_non_speech: true,
            temperature_step: 0.0,
        }
    }

    /// A room segment's opening: the accurate decode without its retries, which over noise cost up
    /// to five more decodes for a transcript that comes out empty anyway.
    pub fn segment_scan() -> Self {
        Self {
            temperature_step: 0.0,
            ..Self::accurate()
        }
    }

    /// Cheap KWS profile; two threads so detection can't starve the command model of Orin cores.
    pub fn wake_word() -> Self {
        Self {
            n_threads: Some(2),
            fit_audio_ctx: true,
            beam_size: None,
            suppress_non_speech: true,
            temperature_step: 0.0,
        }
    }
}

/// Mel frames to encode for `sample_count` 16 kHz samples. The 20% headroom keeps the clip's
/// tail inside the window; the floor avoids starving the encoder on very short bursts.
fn audio_ctx_for(sample_count: usize) -> std::os::raw::c_int {
    const SAMPLE_RATE: f32 = 16_000.0;
    const MEL_FRAMES_PER_SEC: f32 = 50.0;
    const FULL_CONTEXT: i32 = 1500;
    const MIN_CONTEXT: i32 = 128;

    let seconds = sample_count as f32 / SAMPLE_RATE;
    let frames = (seconds * MEL_FRAMES_PER_SEC * 1.2).ceil() as i32;
    frames.clamp(MIN_CONTEXT, FULL_CONTEXT) as std::os::raw::c_int
}

/// Whisper thread count, capped at 6 so the audio pipeline and other services aren't starved.
fn default_threads() -> std::os::raw::c_int {
    let total = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let chosen = total.clamp(2, 6) as i32;
    chosen as std::os::raw::c_int
}

impl WhisperRsInput {
    /// Body of both `listen` methods; `on_speculative_event` hears provisional transcripts.
    async fn listen_inner(
        &self,
        on_speculative_event: Option<Box<dyn Fn(SpeculativeSignal) + Send + Sync>>,
    ) -> Result<Option<String>> {
        let captured = self.captured.lock().unwrap().take();
        let onset = self.onset.lock().unwrap().take();
        let max_record = self.duration_secs;
        let silence_ms = self.silence_ms;
        let audio_level_sink = self.audio_level_sink.clone();

        // Clone the `Arc`: a concurrent `rebuild_with` then leaves this turn on the old engine.
        let engine = self.engine.read().await.clone();

        // VAD starts inference on the first silent poll, before `silence_ms` confirms it.
        let engine_for_speculative = engine.clone();
        let spec_wake_words = self.wake_words_snapshot();
        let mic = self.mic.clone();
        let detector = Arc::clone(&self.detector);
        // Dropping a `spawn_blocking` handle only detaches it; this flag ends the capture.
        let stop = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = crate::StopOnDrop(stop.clone());
        let capture_result = tokio::task::spawn_blocking(move || -> Result<SpeechCapture> {
            // Held for the whole capture; nothing else uses the detector mid-turn.
            let mut detector = detector
                .lock()
                .map_err(|_| anyhow!("speech detector mutex poisoned"))?;
            // Long-lived detector: clear the last turn's recurrent state and buffered leftovers.
            detector.reset();
            if let Some(onset) = onset {
                let (samples, rate) = record_mono_f32_until_silence(
                    &mic,
                    max_record,
                    silence_ms,
                    Some(onset),
                    &mut **detector,
                    &stop,
                )?;
                Ok(SpeechCapture::Samples(resample_to_16k(&samples, rate)))
            } else if let Some(wav) = captured {
                let (captured_samples, _captured_rate) = decode_wav_mono_f32(&wav)?;
                let (fresh_samples, fresh_rate) = record_mono_f32_until_silence(
                    &mic,
                    max_record,
                    silence_ms,
                    None,
                    &mut **detector,
                    &stop,
                )?;
                let fresh_16k = resample_to_16k(&fresh_samples, fresh_rate);
                let mut combined = captured_samples;
                // Skip ~200 ms of mic spin-up at the start of the fresh recording.
                let skip = 16_000usize / 5;
                if fresh_16k.len() > skip {
                    combined.extend_from_slice(&fresh_16k[skip..]);
                }
                Ok(SpeechCapture::Samples(combined))
            } else {
                let speculative_spawn: Box<SpeculativeSpawn> =
                    Box::new(move |samples, rate, abandoned| {
                        let engine = engine_for_speculative.clone();
                        let wake_words = spec_wake_words.clone();
                        std::thread::spawn(move || -> Result<String> {
                            let resampled = resample_to_16k(&samples, rate);
                            // Same profile as the full transcription, so the same state: an
                            // abandoned job must give it up rather than make that one wait.
                            let transcript = Self::transcribe_unless_abandoned(
                                &engine,
                                resampled,
                                TranscribeOpts::accurate(),
                                Some(&abandoned),
                                None,
                            )?;
                            // Strip here so speculative and confirmed transcripts stay byte-identical.
                            Ok(strip_wake_words(transcript, &wake_words))
                        })
                    });
                let (samples, sample_rate, speculative_transcript) = record_mono_f32_vad(
                    &mic,
                    DEFAULT_ONSET_WAIT_SECS,
                    max_record,
                    silence_ms,
                    Some(&*speculative_spawn),
                    on_speculative_event.as_deref(),
                    audio_level_sink.as_deref(),
                    &mut **detector,
                    &stop,
                )?;
                if samples.is_empty() {
                    return Ok(SpeechCapture::Empty);
                }
                if let Some(transcript) = speculative_transcript {
                    return Ok(SpeechCapture::Transcript(
                        transcript,
                        resample_to_16k(&samples, sample_rate),
                    ));
                }
                Ok(SpeechCapture::Samples(resample_to_16k(
                    &samples,
                    sample_rate,
                )))
            }
        })
        .await
        .map_err(|e| anyhow!("audio capture join error: {}", e))??;

        *self.heard.lock().unwrap() = None;
        let samples_result = match capture_result {
            SpeechCapture::Empty => return Ok(Some(String::new())),
            // Already stripped in the speculative thread.
            SpeechCapture::Transcript(t, samples) => {
                *self.heard.lock().unwrap() = Some(samples);
                return Ok(Some(t));
            }
            SpeechCapture::Samples(s) => s,
        };

        let kept = samples_result.clone();
        let transcript =
            tokio::task::spawn_blocking(move || Self::transcribe_samples(&engine, samples_result))
                .await
                .map_err(|e| anyhow!("inference join error: {}", e))??;
        *self.heard.lock().unwrap() = Some(kept);

        Ok(Some(self.without_wake_word(transcript)))
    }

    /// Remove the wake word the detector's lookback pulled into the clip.
    fn without_wake_word(&self, transcript: String) -> String {
        strip_wake_words(transcript, &self.wake_words_snapshot())
    }

    /// A copy of the wake-word list, for handing to a worker thread.
    fn wake_words_snapshot(&self) -> Vec<String> {
        self.wake_words
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[async_trait]
impl VoiceInput for WhisperRsInput {
    async fn listen(&self) -> Result<Option<String>> {
        self.listen_inner(None).await
    }

    async fn listen_with_speculative(
        &self,
        on_speculative: Box<dyn Fn(SpeculativeSignal) + Send + Sync>,
    ) -> Result<Option<String>> {
        self.listen_inner(Some(on_speculative)).await
    }

    fn prompt(&self) -> &str {
        "listening"
    }

    fn prime_with_captured(&self, wav: Vec<u8>) {
        *self.captured.lock().unwrap() = Some(wav);
    }

    fn prime_with_onset(&self, onset: std::time::Instant) {
        *self.onset.lock().unwrap() = Some(onset);
    }

    fn take_heard_audio(&self) -> Option<Vec<u8>> {
        let samples = self.heard.lock().unwrap().take()?;
        Some(pond_voice::dsp::encode_wav_pcm16(
            &pond_voice::dsp::f32_to_pcm16(&samples),
            16_000,
        ))
    }
}

impl WhisperBackend for WhisperRsInput {
    fn transcribe_pcm_blocking(&self, samples: &[f32]) -> Result<String> {
        // Only called from `spawn_blocking` workers, where `blocking_read` is safe.
        let engine = self.engine.blocking_read().clone();
        // Only the wake-word detector calls this, hence the cheap profile.
        Self::transcribe_samples_with(&engine, samples.to_vec(), TranscribeOpts::wake_word())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mismatch discards the speculative turn and the user hears two overlapping replies.
    #[test]
    fn both_transcripts_for_one_clip_get_identical_wake_word_treatment() {
        let wake = vec!["goose".to_string()];
        let heard = "Goose, turn the kitchen lights on.".to_string();

        let speculative = strip_wake_words(heard.clone(), &wake);
        let confirmed = strip_wake_words(heard, &wake);

        assert_eq!(
            speculative, confirmed,
            "the reuse gate compares these directly"
        );
        assert_eq!(speculative, "turn the kitchen lights on.");
    }

    #[test]
    fn the_prompt_said_back_is_an_echo_and_a_request_that_starts_like_it_is_not() {
        let prompt = "Hey goose. Jerry Anyumba.";
        assert!(is_prompt_echo("Hey goose, Jerry Anyumba.", prompt));
        assert!(is_prompt_echo(
            "Hey goose. Jerry Anyumba. Hey goose. Jerry Anyumba. Hey goose.",
            prompt
        ));
        assert!(
            !is_prompt_echo("Hey goose. Jerry", prompt),
            "shorter than one copy"
        );
        assert!(!is_prompt_echo(
            "Hey goose, Jerry Anyumba is home, turn the lights on.",
            prompt
        ));
        assert!(!is_prompt_echo("", prompt));
        assert!(!is_prompt_echo("anything", ""));
    }

    #[test]
    fn the_wake_phrase_looped_from_the_prompt_is_an_echo_and_said_once_or_twice_is_not() {
        let prompt = "Hey goose. Jerry Anyumba.";
        let looped = "Hey goose. ".repeat(8);
        assert!(is_prompt_echo(&looped, prompt));
        assert!(is_prompt_echo(
            "Hey goose, hey goose, hey goose, hey",
            prompt
        ));
        assert!(!is_prompt_echo("Hey goose.", prompt), "a wake");
        assert!(
            !is_prompt_echo("Hey goose. Hey goose.", prompt),
            "called twice"
        );
        assert!(!is_prompt_echo(
            "Hey goose, hey goose, hey goose, what time is it?",
            prompt
        ));
    }

    #[test]
    fn stripping_an_already_stripped_transcript_changes_nothing() {
        let wake = vec!["goose".to_string()];
        let once = strip_wake_words("goose, ask the goose".to_string(), &wake);
        let twice = strip_wake_words(once.clone(), &wake);
        assert_eq!(once, "ask the goose");
        assert_eq!(twice, once, "a second pass must be a no-op");
    }

    #[test]
    fn no_configured_wake_words_leaves_the_transcript_alone() {
        let heard = "What's the weather?".to_string();
        assert_eq!(strip_wake_words(heard.clone(), &[]), heard);
    }

    /// A no-hardware `MicHandle`, for tests that only need one to construct, not to capture.
    fn test_mic() -> pond_audio::MicHandle {
        let (mic, _join) = pond_audio::spawn(
            Box::new(pond_audio::testing::ScriptedCapture::silence(0, 20)),
            pond_audio::CAPTURE_RATE_HZ,
            5_000,
            true,
        );
        mic
    }

    #[test]
    fn new_returns_err_on_missing_model() {
        let path = PathBuf::from("/tmp/definitely-not-a-real-whisper-model-12345.bin");
        let result = WhisperRsInput::new(path, test_mic());
        assert!(result.is_err(), "expected Err on missing model file");
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("not found"),
            "error should mention 'not found': {}",
            msg
        );
    }

    /// Needs `WHISPER_TEST_MODEL` pointing at a ggml model; run with `--ignored`.
    #[test]
    #[ignore]
    fn loads_real_model_and_transcribes_silence() {
        let Some(model_env) = std::env::var_os("WHISPER_TEST_MODEL") else {
            eprintln!("set WHISPER_TEST_MODEL to run this test");
            return;
        };
        let model_path = PathBuf::from(model_env);
        let input = WhisperRsInput::new(model_path, test_mic()).expect("model should load");

        // 1 second of silence at 16 kHz.
        let silence = vec![0.0f32; 16_000];
        let result = input.transcribe_pcm_blocking(&silence);
        // `Ok("")` or a hallucination are both fine; it just must not panic or `Err`.
        assert!(
            result.is_ok(),
            "silence should not produce Err: {:?}",
            result
        );
    }

    /// Measures real inference time against `DEFAULT_SILENCE_MS`; needs `WHISPER_TEST_MODEL`.
    #[test]
    #[ignore]
    fn speculative_overlap_hides_inference_time_within_default_silence_window() {
        let Some(model_env) = std::env::var_os("WHISPER_TEST_MODEL") else {
            eprintln!("set WHISPER_TEST_MODEL to run this test");
            return;
        };
        let model_path = PathBuf::from(model_env);
        let input = WhisperRsInput::new(model_path, test_mic()).expect("model should load");

        let wav_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/blobs/jfk.wav");
        let wav_bytes = std::fs::read(&wav_path).expect("jfk.wav fixture missing");
        let (samples, _rate) = decode_wav_mono_f32(&wav_bytes).expect("decode jfk.wav");

        let start = std::time::Instant::now();
        let transcript = input
            .transcribe_pcm_blocking(&samples)
            .expect("transcription should not error");
        let elapsed = start.elapsed();

        assert!(
            !transcript.trim().is_empty(),
            "jfk.wav should transcribe to real text"
        );
        println!(
            "whisper inference wall time: {:?} (silence-confirmation window this overlaps with: {}ms)",
            elapsed, DEFAULT_SILENCE_MS
        );
    }

    #[test]
    fn the_abort_hook_reads_the_flag_it_was_given() {
        let flag = AtomicBool::new(false);
        let data = &flag as *const AtomicBool as *mut std::ffi::c_void;
        // SAFETY: `flag` is live for both calls.
        assert!(!unsafe { abort_when_raised(data) });
        flag.store(true, Ordering::SeqCst);
        assert!(unsafe { abort_when_raised(data) });
    }

    #[test]
    fn empty_pcm_via_trait_does_not_panic() {
        // No `WhisperContext` without a model file; mirrors the empty-input early return only.
        let samples: Vec<f32> = Vec::new();
        assert!(samples.is_empty());
    }

    #[test]
    fn audio_ctx_tracks_the_clip_not_the_30_second_pad() {
        // 2.5 s window = 125 mel frames + 20% headroom = 150.
        let frames = audio_ctx_for(16_000 * 5 / 2);
        assert_eq!(frames, 150);
        assert!(frames < 1500, "must be far below the full 30s context");
    }

    #[test]
    fn audio_ctx_has_a_floor_for_very_short_bursts() {
        assert_eq!(audio_ctx_for(16_000 / 10), 128, "100ms clamps to the floor");
        assert_eq!(audio_ctx_for(0), 128);
    }

    #[test]
    fn audio_ctx_never_exceeds_the_full_context() {
        // 60 s of audio would compute past 1500 without the clamp.
        assert_eq!(audio_ctx_for(16_000 * 60), 1500);
    }

    #[test]
    fn a_typical_wake_word_utterance_is_an_order_of_magnitude_cheaper() {
        let frames = audio_ctx_for(16_000 * 12 / 10); // 1.2 s "hey goose"
        assert!(
            frames * 10 < 1500,
            "1.2s should cost <10% of a full encode, got {frames}/1500"
        );
    }

    #[test]
    fn the_wake_word_profile_is_cheap_and_the_accurate_one_is_not() {
        let kws = TranscribeOpts::wake_word();
        assert_eq!(kws.n_threads, Some(2), "must not claim all six cores");
        assert!(kws.fit_audio_ctx);

        let cmd = TranscribeOpts::accurate();
        assert_eq!(
            cmd.n_threads, None,
            "command transcription uses the default"
        );
        assert!(
            !cmd.fit_audio_ctx,
            "the user's actual speech keeps the full context"
        );
    }

    /// Measures what the KWS `audio_ctx` cap costs in accuracy; needs `WHISPER_TEST_MODEL`.
    #[test]
    #[ignore]
    fn audio_ctx_sweep() {
        let Some(model_env) = std::env::var_os("WHISPER_TEST_MODEL") else {
            eprintln!("set WHISPER_TEST_MODEL");
            return;
        };
        let input = WhisperRsInput::new(PathBuf::from(model_env), test_mic()).expect("model loads");
        let ctx = input.engine.blocking_read().clone();

        let wav_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/blobs/jfk.wav");
        let wav = std::fs::read(&wav_path).expect("jfk.wav");
        let (all, _rate) = crate::decode_wav_mono_f32(&wav).expect("decode");

        // A 2.5 s window — exactly what the wake-word detector transcribes.
        let win: Vec<f32> = all.iter().take(16_000 * 5 / 2).copied().collect();
        eprintln!("\n=== 2.5s window, {} samples ===", win.len());

        let full =
            WhisperRsInput::transcribe_samples_with(&ctx, win.clone(), TranscribeOpts::accurate())
                .unwrap();
        eprintln!("  audio_ctx UNSET (1500): {full:?}");

        let fitted =
            WhisperRsInput::transcribe_samples_with(&ctx, win.clone(), TranscribeOpts::wake_word())
                .unwrap();
        eprintln!(
            "  audio_ctx {} (KWS):     {fitted:?}",
            audio_ctx_for(win.len())
        );

        // The wake word itself is short — this is the case that matters.
        let short: Vec<f32> = all.iter().take(16_000 * 12 / 10).copied().collect();
        eprintln!("\n=== 1.2s window (wake-word length) ===");
        eprintln!(
            "  audio_ctx UNSET:        {:?}",
            WhisperRsInput::transcribe_samples_with(
                &ctx,
                short.clone(),
                TranscribeOpts::accurate()
            )
            .unwrap()
        );
        eprintln!(
            "  audio_ctx {} (KWS):      {:?}",
            audio_ctx_for(short.len()),
            WhisperRsInput::transcribe_samples_with(
                &ctx,
                short.clone(),
                TranscribeOpts::wake_word()
            )
            .unwrap()
        );
    }
}

/// Real-model accuracy checks, run by hand with `WHISPER_TEST_MODEL` set.
#[cfg(test)]
mod decode_profiles {
    use super::*;

    fn model() -> Option<Arc<Engine>> {
        let path = std::env::var_os("WHISPER_TEST_MODEL")?;
        let path = PathBuf::from(path);
        if !path.exists() {
            eprintln!("WHISPER_TEST_MODEL does not exist: {}", path.display());
            return None;
        }
        Some(Arc::new(Engine::new(
            load_context(&path).expect("load model"),
            TranscribeOpts::accurate(),
        )))
    }

    fn jfk_samples() -> Vec<f32> {
        let wav =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/blobs/jfk.wav");
        let bytes = std::fs::read(wav).expect("tests/blobs/jfk.wav");
        let (samples, rate) = crate::decode_wav_mono_f32(&bytes).expect("decode");
        crate::resample_to_16k(&samples, rate)
    }

    #[test]
    #[ignore]
    fn the_accurate_profile_transcribes_real_speech_correctly() {
        let Some(ctx) = model() else {
            eprintln!("set WHISPER_TEST_MODEL to run this");
            return;
        };
        let samples = jfk_samples();

        let t0 = std::time::Instant::now();
        let greedy = WhisperRsInput::transcribe_samples_with(
            &ctx,
            samples.clone(),
            TranscribeOpts::wake_word(),
        )
        .expect("greedy decode");
        let greedy_ms = t0.elapsed().as_millis();

        let t1 = std::time::Instant::now();
        let accurate =
            WhisperRsInput::transcribe_samples_with(&ctx, samples, TranscribeOpts::accurate())
                .expect("beam decode");
        let accurate_ms = t1.elapsed().as_millis();

        eprintln!("\n  cheap    {greedy_ms:>5}ms  {greedy:?}");
        eprintln!("  accurate {accurate_ms:>5}ms  {accurate:?}\n");

        // The known content of jfk.wav. Every content word must survive.
        for word in [
            "ask", "not", "what", "your", "country", "can", "do", "for", "you",
        ] {
            assert!(
                accurate.to_lowercase().contains(word),
                "the accuracy profile dropped {word:?} from: {accurate:?}"
            );
        }
    }

    /// A transcription allocates nothing: loading makes the accurate state, every later call
    /// reuses it, and the wake-word state is made once, on its first use.
    #[test]
    #[ignore]
    fn each_profile_keeps_one_state_across_transcriptions() {
        let Some(ctx) = model() else {
            eprintln!("set WHISPER_TEST_MODEL to run this");
            return;
        };
        let created = || ctx.created.load(Ordering::Relaxed);
        assert_eq!(created(), 1, "loading prepares the accurate state");
        for _ in 0..3 {
            let text = WhisperRsInput::transcribe_samples(&ctx, jfk_samples()).expect("decode");
            assert!(text.to_lowercase().contains("country"), "{text:?}");
        }
        assert_eq!(created(), 1, "an accurate transcription made a state");
        for _ in 0..2 {
            WhisperRsInput::transcribe_samples_with(
                &ctx,
                jfk_samples(),
                TranscribeOpts::wake_word(),
            )
            .expect("decode");
        }
        assert_eq!(created(), 2, "the wake-word state is made once");
    }

    /// An abandoned speculative job shares the full transcription's state; it must give it up
    /// at its next decode step rather than make that transcription wait out its decode.
    #[test]
    #[ignore]
    fn an_abandoned_transcription_does_not_hold_up_the_next() {
        let Some(ctx) = model() else {
            eprintln!("set WHISPER_TEST_MODEL to run this");
            return;
        };
        let short = jfk_samples();
        // Three 30 s windows, so the decode runs long enough to abandon partway.
        let long = short.repeat(8);

        let t0 = std::time::Instant::now();
        WhisperRsInput::transcribe_samples(&ctx, short.clone()).expect("decode");
        let alone = t0.elapsed();
        let t0 = std::time::Instant::now();
        WhisperRsInput::transcribe_samples(&ctx, long.clone()).expect("decode");
        let whole = t0.elapsed();

        let abandoned = Arc::new(AtomicBool::new(false));
        let job = std::thread::spawn({
            let (ctx, abandoned) = (ctx.clone(), abandoned.clone());
            move || {
                WhisperRsInput::transcribe_unless_abandoned(
                    &ctx,
                    long,
                    TranscribeOpts::accurate(),
                    Some(&abandoned),
                    None,
                )
            }
        });
        std::thread::sleep(whole / 4);
        abandoned.store(true, Ordering::SeqCst);

        let t0 = std::time::Instant::now();
        let text = WhisperRsInput::transcribe_samples(&ctx, short).expect("decode");
        let waited = t0.elapsed();
        let outcome = job.join().expect("the job must not panic");
        eprintln!("\n  alone {alone:?}, whole {whole:?}, after abandoning {waited:?}\n");

        let error = outcome.expect_err("an abandoned job returns no transcript");
        assert!(error.to_string().contains("abandoned"), "{error:#}");
        assert!(text.to_lowercase().contains("country"), "{text:?}");
        assert!(
            waited < alone + whole / 2,
            "queued behind the abandoned job: {waited:?} (alone {alone:?}, whole {whole:?})"
        );
        // The aborted state still serves.
        let again = WhisperRsInput::transcribe_samples(&ctx, jfk_samples()).expect("decode");
        assert!(again.to_lowercase().contains("country"), "{again:?}");
    }

    #[test]
    #[ignore]
    fn the_accurate_profile_emits_no_bracketed_annotations() {
        let Some(ctx) = model() else {
            eprintln!("set WHISPER_TEST_MODEL to run this");
            return;
        };
        // Near-silence with a little noise is what provokes them.
        let noise: Vec<f32> = (0..16_000 * 3)
            .map(|i| (i as f32 * 0.7).sin() * 0.001)
            .collect();
        let out = WhisperRsInput::transcribe_samples_with(&ctx, noise, TranscribeOpts::accurate())
            .expect("decode");
        eprintln!("\n  quiet-room transcript: {out:?}\n");
        assert!(!out.contains('['), "annotation leaked through: {out:?}");
    }

    /// Fill the GPU with Whisper states until there is no room for another, as the LLM does
    /// in production, and require that the failure is an error. Before the GGML change in
    /// `vendor/whisper-rs-sys/POND-PATCH.md` it was a `SIGSEGV`, so a crash here is that bug.
    ///
    /// It exhausts device memory on purpose: stop the Pond first. CUDA builds only, because
    /// on unified memory without a device limit it would fill RAM before anything failed.
    #[cfg(feature = "cuda")]
    #[test]
    #[ignore]
    fn running_out_of_gpu_memory_is_an_error_not_a_crash() {
        let Some(ctx) = model() else {
            eprintln!("set WHISPER_TEST_MODEL to run this");
            return;
        };
        let mut held = Vec::new();
        let error = loop {
            match ctx.context.create_state() {
                Ok(state) => held.push(state),
                Err(error) => break error,
            }
            assert!(
                held.len() < 256,
                "the GPU never filled; is this a CUDA build?"
            );
        };
        eprintln!("\n  {} states fit before: {error}\n", held.len());
        assert!(
            !held.is_empty(),
            "not even one state fit, so nothing was tested"
        );

        drop(held);
        let after = WhisperRsInput::transcribe_samples(&ctx, jfk_samples())
            .expect("memory released by the failed and the held states serves a transcription");
        assert!(after.to_lowercase().contains("country"), "{after:?}");
    }

    /// The point of keeping a state: with the GPU full, a transcription still works, because it
    /// allocates nothing. Before states were kept, this call created one and failed. Stop the
    /// Pond first, as for the tests around it. CUDA builds only.
    #[cfg(feature = "cuda")]
    #[test]
    #[ignore]
    fn a_kept_state_transcribes_on_a_full_gpu() {
        let Some(ctx) = model() else {
            eprintln!("set WHISPER_TEST_MODEL to run this");
            return;
        };
        let mut held = Vec::new();
        while let Ok(state) = ctx.context.create_state() {
            held.push(state);
            assert!(
                held.len() < 256,
                "the GPU never filled; is this a CUDA build?"
            );
        }
        eprintln!("\n  {} states filled the GPU\n", held.len());
        let text = WhisperRsInput::transcribe_samples(&ctx, jfk_samples())
            .expect("a kept state transcribes with no room left");
        assert!(text.to_lowercase().contains("country"), "{text:?}");
        assert_eq!(ctx.created.load(Ordering::Relaxed), 1, "a state was made");
        drop(held);
    }

    /// The other place a full GPU fails: decoding, after the state was created. A state is
    /// created with a self-attention cache for one decoder, and beam search grows it to
    /// `beams + 2` decoders' worth (42 MiB for `base` with five beams) on its first decode.
    /// whisper.cpp freed the caller's state when that failed, and whisper-rs freed it again
    /// on drop; on 2026-10-05 the Pond took `SIGBUS` a second after
    /// `whisper_kv_cache_init() failed for self-attention cache`. So a crash here is that bug.
    ///
    /// One beam decode runs first, as the Pond's earlier transcriptions have by the time this
    /// happens in production. Then the GPU is filled with states, and each decodes with beam
    /// search until one cannot grow its cache. Stop the Pond first, as for the test above.
    /// CUDA builds only.
    ///
    /// Where a run fails depends on how much memory the fill leaves, so run it until it prints
    /// `beam decodes fit before`. A run that aborts with `cuMemAddressReserve` in its log has
    /// hit a different failure and says nothing about this one: each state's first decode
    /// reserves GGML's CUDA scratch pool, and GGML aborts when that fails. On the Jetson on
    /// 2026-10-05, three of four runs without the fix aborted there; the fourth reached the
    /// cache, got `-7`, and took `SIGBUS` on drop, as the Pond had. With the fix, every run
    /// that reached the cache passed.
    #[cfg(feature = "cuda")]
    #[test]
    #[ignore]
    fn running_out_of_gpu_memory_while_decoding_is_an_error_not_a_crash() {
        let Some(ctx) = model() else {
            eprintln!("set WHISPER_TEST_MODEL to run this");
            return;
        };
        let samples = jfk_samples();
        let beam = || {
            let mut params = FullParams::new(SamplingStrategy::BeamSearch {
                beam_size: 5,
                patience: -1.0,
            });
            params.set_language(Some("en"));
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            params
        };
        ctx.context
            .create_state()
            .expect("a state on an idle GPU")
            .full(beam(), &samples)
            .expect("a beam decode on an idle GPU");

        let mut held = Vec::new();
        while let Ok(state) = ctx.context.create_state() {
            held.push(state);
            assert!(
                held.len() < 256,
                "the GPU never filled; is this a CUDA build?"
            );
        }
        assert!(
            !held.is_empty(),
            "not even one state fit, so nothing was tested"
        );

        let mut decoded = 0;
        let (failed, error) = held
            .iter_mut()
            .enumerate()
            .find_map(|(i, state)| match state.full(beam(), &samples) {
                Ok(()) => {
                    decoded += 1;
                    None
                }
                Err(error) => Some((i, error)),
            })
            .expect("every state grew its cache; the GPU never filled");
        eprintln!("\n  {decoded} beam decodes fit before: {error}\n");
        assert!(
            matches!(error, whisper_rs::WhisperError::GenericError(-7)),
            "the decode failed somewhere other than growing its cache: {error}"
        );

        // The failed state has no self-attention cache; auto-detect decodes before whisper_full
        // recreates it, and asserted on the missing buffer.
        let mut failed = held.swap_remove(failed);
        drop(held);
        let mut auto = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        auto.set_language(Some("auto"));
        auto.set_print_progress(false);
        auto.set_print_realtime(false);
        auto.set_print_timestamps(false);
        failed
            .full(auto, &samples)
            .expect("an auto-detect decode on the state whose cache could not grow");

        // Dropped here; before the fix the failed state was freed twice.
        drop(failed);
        let after = WhisperRsInput::transcribe_samples(&ctx, jfk_samples())
            .expect("memory released by the failed and the held states serves a transcription");
        assert!(after.to_lowercase().contains("country"), "{after:?}");
    }
}
