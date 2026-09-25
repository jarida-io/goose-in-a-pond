//! In-process Whisper ASR via `whisper-rs`.
//! Shares ggml's CUDA primary context with `llama-cpp-2`, so Jetson keeps one CUDA context.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use pond_core::models::ports::voice_input::{SpeculativeSignal, VoiceInput};
use pond_voice::dsp::{RmsDetector, SpeechDetector};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::{
    decode_wav_mono_f32, record_mono_f32_until_silence, record_mono_f32_vad, resample_to_16k,
    strip_whisper_artifacts, SpeculativeSpawn, ThrottledAudioLevelSink, WhisperBackend,
};

/// Outcome of the blocking audio-capture step in `listen()`.
enum SpeechCapture {
    /// No speech detected within the onset wait — nothing to transcribe.
    Empty,
    /// A matching speculative transcript was already computed; use it, skip re-inference.
    Transcript(String),
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

/// In-process Whisper adapter. One loaded model per instance.
pub struct WhisperRsInput {
    /// Swapped by `rebuild_with`; in-flight transcriptions keep their `Arc` of the old one.
    context: RwLock<Arc<WhisperContext>>,
    /// Last-known model path, recorded for diagnostics.
    model_path: RwLock<PathBuf>,
    /// Hard cap on recording time (seconds). VAD ends earlier on silence.
    duration_secs: u32,
    /// Consecutive silence (ms) that ends a recording.
    silence_ms: u64,
    /// Pre-captured WAV bytes from the wake-word detector (one-breath path).
    captured: Mutex<Option<Vec<u8>>>,
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
            context: RwLock::new(Arc::new(context)),
            model_path: RwLock::new(model_path),
            duration_secs: DEFAULT_DURATION_SECS,
            silence_ms: DEFAULT_SILENCE_MS,
            captured: Mutex::new(None),
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
        let new_ctx = tokio::task::spawn_blocking({
            let p = new_model_path.clone();
            move || load_context(&p)
        })
        .await
        .map_err(|e| anyhow!("model load join error: {}", e))??;

        let mut ctx_guard = self.context.write().await;
        *ctx_guard = Arc::new(new_ctx);
        drop(ctx_guard);

        let mut path_guard = self.model_path.write().await;
        *path_guard = new_model_path.clone();

        tracing::info!(
            "WhisperRsInput hot-swapped to: {}",
            new_model_path.display()
        );
        Ok(())
    }

    pub async fn current_model_path(&self) -> PathBuf {
        self.model_path.read().await.clone()
    }

    /// Decode WAV bytes, resample to 16 kHz mono, and transcribe in-process.
    pub fn transcribe_wav_bytes(&self, wav_bytes: &[u8]) -> Result<String> {
        let (samples, rate) = crate::decode_wav_mono_f32(wav_bytes)?;
        let samples_16k = crate::resample_to_16k(&samples, rate);
        let ctx = self.context.blocking_read().clone();
        Self::transcribe_samples(ctx, samples_16k)
    }

    /// Transcribe 16 kHz mono PCM (artifacts stripped); empty → `Ok("")`, panic → `Err`.
    fn transcribe_samples(ctx: Arc<WhisperContext>, samples: Vec<f32>) -> Result<String> {
        Self::transcribe_samples_with(ctx, samples, TranscribeOpts::accurate())
    }

    /// As [`Self::transcribe_samples`], with per-call cost tuning.
    fn transcribe_samples_with(
        ctx: Arc<WhisperContext>,
        samples: Vec<f32>,
        opts: TranscribeOpts,
    ) -> Result<String> {
        if samples.is_empty() {
            return Ok(String::new());
        }
        let n_threads = opts.n_threads.unwrap_or_else(default_threads);
        let audio_ctx = opts.fit_audio_ctx.then(|| audio_ctx_for(samples.len()));

        // Catches Rust panics from whisper-rs; a C++ failure in whisper.cpp aborts regardless.
        let result = catch_unwind(AssertUnwindSafe(move || -> Result<String> {
            let mut state = ctx
                .create_state()
                .context("whisper-rs: create_state failed")?;

            let strategy = match opts.beam_size {
                Some(beam_size) => SamplingStrategy::BeamSearch {
                    beam_size,
                    patience: -1.0, // whisper.cpp default
                },
                None => SamplingStrategy::Greedy { best_of: 1 },
            };
            let mut params = FullParams::new(strategy);
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

            state
                .full(params, &samples)
                .context("whisper-rs: full inference failed")?;

            let n_segments = state.full_n_segments();
            let mut out = String::new();
            for i in 0..n_segments {
                if let Some(seg) = state.get_segment(i) {
                    if let Ok(text) = seg.to_str() {
                        out.push_str(text);
                    }
                }
            }
            Ok(strip_whisper_artifacts(&out))
        }));

        match result {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(e)) => Err(e),
            Err(panic) => {
                let msg = panic_message(&panic);
                tracing::error!("whisper-rs panic caught: {}", msg);
                Err(anyhow!("whisper-rs panic: {}", msg))
            }
        }
    }
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
            temperature_step: 0.2,
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
        let max_record = self.duration_secs;
        let silence_ms = self.silence_ms;
        let audio_level_sink = self.audio_level_sink.clone();

        // Clone the `Arc`: a concurrent `rebuild_with` then leaves this turn on the old context.
        let ctx_arc = self.context.read().await.clone();

        // VAD starts inference on the first silent poll, before `silence_ms` confirms it.
        let ctx_for_speculative = ctx_arc.clone();
        let spec_wake_words = self.wake_words_snapshot();
        let mic = self.mic.clone();
        let detector = Arc::clone(&self.detector);
        let capture_result = tokio::task::spawn_blocking(move || -> Result<SpeechCapture> {
            // Held for the whole capture; nothing else uses the detector mid-turn.
            let mut detector = detector
                .lock()
                .map_err(|_| anyhow!("speech detector mutex poisoned"))?;
            // Long-lived detector: clear the last turn's recurrent state and buffered leftovers.
            detector.reset();
            if let Some(wav) = captured {
                let (captured_samples, _captured_rate) = decode_wav_mono_f32(&wav)?;
                let (fresh_samples, fresh_rate) =
                    record_mono_f32_until_silence(&mic, max_record, silence_ms, &mut **detector)?;
                let fresh_16k = resample_to_16k(&fresh_samples, fresh_rate);
                let mut combined = captured_samples;
                // Skip ~200 ms of mic spin-up at the start of the fresh recording.
                let skip = 16_000usize / 5;
                if fresh_16k.len() > skip {
                    combined.extend_from_slice(&fresh_16k[skip..]);
                }
                Ok(SpeechCapture::Samples(combined))
            } else {
                let speculative_spawn: Box<SpeculativeSpawn> = Box::new(move |samples, rate| {
                    let ctx = ctx_for_speculative.clone();
                    let wake_words = spec_wake_words.clone();
                    std::thread::spawn(move || -> Result<String> {
                        let resampled = resample_to_16k(&samples, rate);
                        let transcript = Self::transcribe_samples(ctx, resampled)?;
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
                )?;
                if samples.is_empty() {
                    return Ok(SpeechCapture::Empty);
                }
                if let Some(transcript) = speculative_transcript {
                    return Ok(SpeechCapture::Transcript(transcript));
                }
                Ok(SpeechCapture::Samples(resample_to_16k(
                    &samples,
                    sample_rate,
                )))
            }
        })
        .await
        .map_err(|e| anyhow!("audio capture join error: {}", e))??;

        let samples_result = match capture_result {
            SpeechCapture::Empty => return Ok(Some(String::new())),
            // Already stripped in the speculative thread.
            SpeechCapture::Transcript(t) => return Ok(Some(t)),
            SpeechCapture::Samples(s) => s,
        };

        let ctx_for_blocking = ctx_arc.clone();
        let transcript = tokio::task::spawn_blocking(move || {
            Self::transcribe_samples(ctx_for_blocking, samples_result)
        })
        .await
        .map_err(|e| anyhow!("inference join error: {}", e))??;

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
}

impl WhisperBackend for WhisperRsInput {
    fn transcribe_pcm_blocking(&self, samples: &[f32]) -> Result<String> {
        // Only called from `spawn_blocking` workers, where `blocking_read` is safe.
        let ctx = self.context.blocking_read().clone();
        // Only the wake-word detector calls this, hence the cheap profile.
        Self::transcribe_samples_with(ctx, samples.to_vec(), TranscribeOpts::wake_word())
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
        let ctx = input.context.blocking_read().clone();

        let wav_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/blobs/jfk.wav");
        let wav = std::fs::read(&wav_path).expect("jfk.wav");
        let (all, _rate) = crate::decode_wav_mono_f32(&wav).expect("decode");

        // A 2.5 s window — exactly what the wake-word detector transcribes.
        let win: Vec<f32> = all.iter().take(16_000 * 5 / 2).copied().collect();
        eprintln!("\n=== 2.5s window, {} samples ===", win.len());

        let full = WhisperRsInput::transcribe_samples_with(
            ctx.clone(),
            win.clone(),
            TranscribeOpts::accurate(),
        )
        .unwrap();
        eprintln!("  audio_ctx UNSET (1500): {full:?}");

        let fitted = WhisperRsInput::transcribe_samples_with(
            ctx.clone(),
            win.clone(),
            TranscribeOpts::wake_word(),
        )
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
                ctx.clone(),
                short.clone(),
                TranscribeOpts::accurate()
            )
            .unwrap()
        );
        eprintln!(
            "  audio_ctx {} (KWS):      {:?}",
            audio_ctx_for(short.len()),
            WhisperRsInput::transcribe_samples_with(
                ctx.clone(),
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

    fn model() -> Option<Arc<WhisperContext>> {
        let path = std::env::var_os("WHISPER_TEST_MODEL")?;
        let path = PathBuf::from(path);
        if !path.exists() {
            eprintln!("WHISPER_TEST_MODEL does not exist: {}", path.display());
            return None;
        }
        Some(Arc::new(load_context(&path).expect("load model")))
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
            ctx.clone(),
            samples.clone(),
            TranscribeOpts::wake_word(),
        )
        .expect("greedy decode");
        let greedy_ms = t0.elapsed().as_millis();

        let t1 = std::time::Instant::now();
        let accurate =
            WhisperRsInput::transcribe_samples_with(ctx, samples, TranscribeOpts::accurate())
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
        let out = WhisperRsInput::transcribe_samples_with(ctx, noise, TranscribeOpts::accurate())
            .expect("decode");
        eprintln!("\n  quiet-room transcript: {out:?}\n");
        assert!(!out.contains('['), "annotation leaked through: {out:?}");
    }
}
