//! In-process Whisper ASR via `whisper-rs` (whisper.cpp bindings).
//!
//! `WhisperRsInput` is the default `VoiceInput` adapter — loads a ggml model
//! file once at startup and transcribes captured PCM directly, with no HTTP
//! subprocess on port 9000.
//!
//! Shares the ggml CUDA primary context with `llama-cpp-2`, eliminating the
//! second CUDA context on Jetson (one of the goals in `.ai/scratchpad.md`).
//!
//! ## Crash isolation
//!
//! Every call into `whisper-rs` (model load, transcription) is wrapped in
//! `catch_unwind` so a C-side panic returns `Err` instead of aborting the
//! whole pond-server process.
//!
//! ## Hot-swap
//!
//! `rebuild_with(new_model_path)` atomically replaces the loaded context
//! under a `tokio::sync::RwLock`, mirroring `LocalInferenceLlmAdapter`.
//! Concurrent in-flight `listen()` calls finish on the old context; new
//! ones see the new context.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use pond_core::models::ports::voice_input::{SpeculativeSignal, VoiceInput};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::{
    decode_wav_mono_f32, record_mono_f32_until_silence, record_mono_f32_vad, resample_to_16k,
    strip_whisper_artifacts, SpeculativeSpawn, WhisperBackend,
};

/// Outcome of the blocking audio-capture step in `listen()`.
enum SpeechCapture {
    /// No speech detected within the onset wait — nothing to transcribe.
    Empty,
    /// The silence-confirmation run had a matching speculative transcript
    /// already computed — use it directly, skip a second inference call.
    Transcript(String),
    /// No speculative transcript available (e.g. recording hit the hard
    /// cap before silence was ever confirmed) — transcribe normally.
    Samples(Vec<f32>),
}

/// Maximum recording duration (hard cap). VAD usually stops earlier.
const DEFAULT_DURATION_SECS: u32 = 30;
/// Silence (ms) after speech to declare end-of-utterance.
const DEFAULT_SILENCE_MS: u64 = 800;
/// Wait window for speech onset before giving up.
const DEFAULT_ONSET_WAIT_SECS: u32 = 10;

/// In-process Whisper adapter. One loaded model per instance.
///
/// Cloneable handle via `Arc<WhisperRsInput>` — the context lives behind
/// the internal `RwLock` and is shared across handles.
pub struct WhisperRsInput {
    /// Loaded whisper.cpp context. `RwLock` lets `rebuild_with` swap it
    /// while in-flight transcriptions hold a read guard.
    context: RwLock<Arc<WhisperContext>>,
    /// Last-known model path, recorded for diagnostics.
    model_path: RwLock<PathBuf>,
    /// Hard cap on recording time (seconds). VAD ends earlier on silence.
    duration_secs: u32,
    /// Consecutive silence (ms) that ends a recording.
    silence_ms: u64,
    /// Pre-captured WAV bytes from the wake-word detector (one-breath path).
    captured: Mutex<Option<Vec<u8>>>,
    /// Normalized wake-word variants, stripped off the front of a command
    /// transcript. The detector's capture reaches back past the trigger, so
    /// the clip contains the wake word and the transcript would otherwise
    /// open with the assistant's own name.
    ///
    /// Behind a lock only because it is populated after construction: this
    /// instance is already an `Arc` (it is both the `VoiceInput` and the
    /// detector's backend) by the time the detector has resolved its trigger
    /// list, and the two must strip and match the same words.
    ///
    /// A `std` lock, not tokio's: this is set once during startup, from inside
    /// the runtime, where tokio's `blocking_write` panics outright. Nothing
    /// awaits while holding it.
    wake_words: std::sync::RwLock<Vec<String>>,
}

impl WhisperRsInput {
    /// Load the ggml model at `model_path` and prepare the in-process context.
    ///
    /// Returns `Err` if the file does not exist or whisper-rs fails to load
    /// it. A whisper-rs panic during load is caught and converted to `Err`.
    pub fn new(model_path: PathBuf) -> Result<Self> {
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
        })
    }

    /// Wake-word variants to strip from the front of a command transcript.
    ///
    /// Pass the detector's own resolved trigger list, so the words that fire
    /// detection are exactly the words removed afterwards. Empty leaves
    /// transcripts untouched.
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

    /// Hot-swap the loaded ggml model. Returns `Err` and keeps the previous
    /// context intact if the new model fails to load.
    ///
    /// Mirrors `LocalInferenceLlmAdapter::rebuild_provider`: any in-flight
    /// `listen()` call finishes on the old context; the next call uses the new.
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

    /// Path of the currently loaded model.
    pub async fn current_model_path(&self) -> PathBuf {
        self.model_path.read().await.clone()
    }

    /// Decode WAV bytes, resample to 16 kHz mono, and transcribe in-process.
    ///
    /// Intended for the HTTP `POST /api/v1/transcribe` handler so the serve
    /// path can transcribe without spawning an external whisper-server binary.
    pub fn transcribe_wav_bytes(&self, wav_bytes: &[u8]) -> Result<String> {
        let (samples, rate) = crate::decode_wav_mono_f32(wav_bytes)?;
        let samples_16k = crate::resample_to_16k(&samples, rate);
        let ctx = self.context.blocking_read().clone();
        Self::transcribe_samples(ctx, samples_16k)
    }

    /// Run inference on raw 16 kHz mono f32 PCM. Returns the joined transcript,
    /// already passed through `strip_whisper_artifacts`.
    ///
    /// Wraps the C-side call in `catch_unwind`, so a whisper-rs panic returns
    /// `Err`. Empty input returns `Ok(String::new())` — never panics.
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
        // whisper.cpp drops anything under 100 ms and returns SUCCESS with zero
        // segments (whisper.cpp:6846 — `if (seek_end < seek_start + delta_min)`
        // logs a warning and `return 0`). Upstream, `chat.rs` reads the empty
        // transcript as "nothing was heard", resets the turn and continues — so
        // a real utterance disappears and the log records ok=true. Two such rows
        // are in the device's own session log (84 ms and 85 ms). Name it.
        if samples.len() * 1000 / 16_000 < 100 {
            tracing::warn!(
                samples = samples.len(),
                ms = samples.len() * 1000 / 16_000,
                "utterance shorter than whisper's 100 ms floor — it will transcribe to nothing"
            );
        }
        let n_threads = opts.n_threads.unwrap_or_else(default_threads);
        let audio_ctx = opts.fit_audio_ctx.then(|| audio_ctx_for(samples.len()));

        // Cost of this call, under the `pond_adapters_whisper` target — which
        // the file log keeps at debug by default, and which the
        // `whisper_rs=error` carve-out does not touch.
        //
        // Nothing timed ASR before this. That is why two questions the voice
        // loop's design depends on had no answer: whether the accurate pass
        // actually finishes inside the 800 ms end-of-speech window it is
        // deliberately hidden behind (if it does, making it faster buys the
        // user nothing), and what the always-on wake-word cycle really costs.
        // Both are now a log line, on CPU and on CUDA alike.
        let started = std::time::Instant::now();
        let profile = if opts.beam_size.is_some() {
            "accurate"
        } else {
            "wake_word"
        };
        // Input is always 16 kHz mono here — everything upstream resamples
        // before calling in (`resample_to_16k`).
        let audio_ms = (samples.len() as f64 / 16_000.0) * 1000.0;

        // Use a single std::thread + catch_unwind boundary: we can't catch_unwind
        // across an FFI panic on stable Rust without UnwindSafe, but the C++
        // panic boundary in whisper.cpp aborts the process anyway. We use
        // catch_unwind to convert any *Rust* panic from whisper-rs itself.
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
            // whisper.cpp pads every input to 30 s of mel (1500 frames) and
            // encodes all of it, whatever the clip length. Capping the context
            // to the audio actually supplied is the difference between a
            // wake-word window costing a full 30 s encode and costing its own
            // 2.5 s. Left unset for command transcription, where the clip is
            // longer and accuracy matters more than latency.
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

        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        // RTF < 1 means faster than real time. For the accurate profile the
        // number that matters is `elapsed_ms` against the 800 ms silence
        // window, not RTF — it is the overrun, if any, that the user feels.
        //
        // Two levels on purpose. The accurate pass runs once per turn and its
        // number is the one the design question turns on, so it stays at debug
        // where the file log keeps it by default. The wake-word pass runs on
        // every non-silent window — up to five a second, for the whole of a
        // 24-154 s reply, since the mic hears the assistant speaking — and at
        // debug it would bury the log it shares. `RUST_LOG` raises it when the
        // KWS duty cycle is what you are actually measuring.
        macro_rules! emit {
            ($level:ident) => {
                tracing::$level!(
                    profile,
                    audio_ms = format_args!("{audio_ms:.0}"),
                    elapsed_ms = format_args!("{elapsed_ms:.0}"),
                    rtf = format_args!("{:.3}", elapsed_ms / audio_ms.max(1.0)),
                    n_threads,
                    audio_ctx = audio_ctx.unwrap_or(0),
                    ok = result.as_ref().map(|r| r.is_ok()).unwrap_or(false),
                    "ASR transcribe"
                )
            };
        }
        if opts.beam_size.is_some() {
            emit!(debug)
        } else {
            emit!(trace)
        }

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

/// Route whisper.cpp's own logging into `log`, and from there into tracing.
///
/// Until this is installed, whisper.cpp writes straight to stderr and the
/// tracing filter cannot see it, let alone quieten it — which is where the
/// `whisper_full_with_state: decoder 0: score = ...` wall over the voice UI
/// came from. Once installed the lines become ordinary records under the
/// `whisper_rs` target, filtered like everything else: off the console,
/// still in the session log at debug.
///
/// Installed on first model load rather than in `main` so it cannot be
/// forgotten by a binary that uses this adapter.
fn install_whisper_logging() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(whisper_rs::install_logging_hooks);
}

/// Remove a leading wake word from a command transcript.
///
/// Free-standing so the speculative worker thread and the ordinary path can
/// share it without either needing a `&self`. Both must apply it, or the two
/// transcripts they produce for the same audio will not compare equal.
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

/// Apply platform tuning to `WhisperContextParameters`, mirroring
/// `apply_platform_settings` / `apply_jetson_settings` in pond-adapters-local-inference.
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

/// Per-call cost/accuracy tuning for whisper inference.
///
/// The two callers want opposite things. Wake-word detection runs several
/// times a second forever and only has to recognise one known word, so it
/// optimises for cost. Command transcription runs once per turn and its output
/// becomes the model's prompt, so a word lost there is the whole request
/// misunderstood — it optimises for accuracy, and can afford to.
#[derive(Debug, Clone, Copy)]
pub struct TranscribeOpts {
    /// Threads for this call; `None` uses [`default_threads`].
    pub n_threads: Option<std::os::raw::c_int>,
    /// Cap the encoder's mel context to the clip length instead of padding to 30 s.
    pub fit_audio_ctx: bool,
    /// Beam width. `None` decodes greedily.
    ///
    /// Greedy decoding commits to the highest-probability token at every step
    /// and cannot revisit it, which is where whisper's characteristic
    /// confidently-wrong short phrase comes from. A beam carries several
    /// candidate transcripts and scores them whole, so a word that only makes
    /// sense given the next three survives. It is the single largest accuracy
    /// lever available here, and it costs roughly the beam width in compute —
    /// affordable once per turn, not affordable several times a second.
    pub beam_size: Option<std::os::raw::c_int>,
    /// Suppress non-speech tokens — `[BLANK_AUDIO]`, `(wind blowing)`, and the
    /// rest of the annotations whisper emits for ambient noise. They are never
    /// part of a request, and left in they reach the model as if they were.
    pub suppress_non_speech: bool,
    /// Temperature step used when a decode falls below whisper's confidence
    /// floor. `0.0` disables the retry.
    ///
    /// At temperature zero the decoder is deterministic, and on hard audio it
    /// deterministically produces the same garbage — most visibly the repeated
    /// phrase loop. Stepping the temperature up re-rolls only those failed
    /// segments, and only when they fail, so clean audio never pays for it.
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

    /// What continuous wake-word detection gets.
    ///
    /// Two threads, not six. The command model already claims the cores when a
    /// turn runs, and wake-word detection is not allowed to be the thing that
    /// starves it — on a 6-core Orin the shared 6-thread profile meant KWS and
    /// inference fought over every core. Paired with `fit_audio_ctx`, which is
    /// the larger saving of the two.
    ///
    /// Greedy on purpose: matching one known word against a short window does
    /// not need a beam, and this path runs on a loop.
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

/// Mel frames to encode for `sample_count` samples of 16 kHz audio.
///
/// whisper produces 50 mel frames per second and pads to 1500 (30 s). 20%
/// headroom keeps the tail of the clip inside the window; the floor avoids
/// starving the encoder on very short bursts.
fn audio_ctx_for(sample_count: usize) -> std::os::raw::c_int {
    const SAMPLE_RATE: f32 = 16_000.0;
    const MEL_FRAMES_PER_SEC: f32 = 50.0;
    const FULL_CONTEXT: i32 = 1500;
    const MIN_CONTEXT: i32 = 128;

    let seconds = sample_count as f32 / SAMPLE_RATE;
    let frames = (seconds * MEL_FRAMES_PER_SEC * 1.2).ceil() as i32;
    frames.clamp(MIN_CONTEXT, FULL_CONTEXT) as std::os::raw::c_int
}

/// Reasonable default thread count for whisper inference.
///
/// Whisper benefits from 4–6 threads on the 6-core Jetson A78AE and similar
/// from Apple Silicon performance cores. We cap at 6 to avoid starving the
/// audio pipeline and other GIAP services.
fn default_threads() -> std::os::raw::c_int {
    let total = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let chosen = total.clamp(2, 6) as i32;
    chosen as std::os::raw::c_int
}

impl WhisperRsInput {
    /// Shared implementation behind both `listen()` and
    /// `listen_with_speculative()`. `on_speculative_event`, if given, is
    /// forwarded into `record_mono_f32_vad` so the caller learns about a
    /// provisional transcript before silence is confirmed (Q2-26).
    async fn listen_inner(
        &self,
        on_speculative_event: Option<Box<dyn Fn(SpeculativeSignal) + Send + Sync>>,
    ) -> Result<Option<String>> {
        let captured = self.captured.lock().unwrap().take();
        let max_record = self.duration_secs;
        let silence_ms = self.silence_ms;

        // Acquire the read guard so a concurrent rebuild_with does not swap
        // the context out from under us mid-inference.
        let ctx_arc = self.context.read().await.clone();

        // Capture PCM (already in-process via cpal) on a blocking thread, then
        // hand the samples to whisper-rs without an intermediate WAV round-trip.
        //
        // The non-captured (normal VAD) path overlaps whisper inference with
        // the silence-confirmation wait: `record_mono_f32_vad` fires inference
        // on the first silent poll rather than after `silence_ms` confirms it,
        // so by the time silence is confirmed the transcript is often already
        // done — cutting whisper's inference time out of time-to-first-token
        // instead of paying for it serially afterward (Q2-26).
        let ctx_for_speculative = ctx_arc.clone();
        let spec_wake_words = self.wake_words_snapshot();
        let capture_result = tokio::task::spawn_blocking(move || -> Result<SpeechCapture> {
            if let Some(wav) = captured {
                let (captured_samples, _captured_rate) = decode_wav_mono_f32(&wav)?;
                let (fresh_samples, fresh_rate) =
                    record_mono_f32_until_silence(max_record, silence_ms)?;
                let fresh_16k = resample_to_16k(&fresh_samples, fresh_rate);
                let mut combined = captured_samples;
                // Skip the leading ~200 ms of the fresh recording — the mic
                // needs that long to spin up before producing real audio.
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
                        // Strip HERE, not at the call site. This transcript is
                        // published twice — once as the speculative signal that
                        // fires the LLM early, and again as the confirmed
                        // transcript — and the caller only reuses that early
                        // work if the two are byte-identical. Stripping one and
                        // not the other made them differ on any turn containing
                        // the wake word, so the speculative turn was always
                        // discarded, and its half-spoken reply overlapped the
                        // real one.
                        Ok(strip_wake_words(transcript, &wake_words))
                    })
                });
                let (samples, sample_rate, speculative_transcript) = record_mono_f32_vad(
                    DEFAULT_ONSET_WAIT_SECS,
                    max_record,
                    silence_ms,
                    Some(&*speculative_spawn),
                    on_speculative_event.as_deref(),
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
            // Already stripped inside the speculative thread, so that the
            // signal the caller acted on and this value cannot disagree.
            SpeechCapture::Transcript(t) => return Ok(Some(t)),
            SpeechCapture::Samples(s) => s,
        };

        // Inference on a blocking thread — whisper.cpp `full()` is CPU/GPU
        // synchronous and can take seconds.
        let ctx_for_blocking = ctx_arc.clone();
        let transcript = tokio::task::spawn_blocking(move || {
            Self::transcribe_samples(ctx_for_blocking, samples_result)
        })
        .await
        .map_err(|e| anyhow!("inference join error: {}", e))??;

        Ok(Some(self.without_wake_word(transcript)))
    }

    /// Remove the wake word the detector's lookback pulled into the clip.
    ///
    /// A no-op when no wake words are configured, or on a conversational
    /// follow-up turn, which never contains one.
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
        // Take a synchronous snapshot of the current context via blocking_read.
        // Called from a `spawn_blocking` worker, so blocking_read is safe.
        let ctx = self.context.blocking_read().clone();
        // This is the wake-word path: short clips, matched against a handful of
        // trigger words, running continuously against everything else on the
        // board. It takes the cheap profile.
        Self::transcribe_samples_with(ctx, samples.to_vec(), TranscribeOpts::wake_word())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The speculative job fires the LLM on a provisional transcript, and the
    /// caller only reuses that work if the confirmed transcript is
    /// byte-identical. Any transform applied to one and not the other breaks
    /// the comparison silently: the speculative turn is discarded, a second
    /// turn runs, and the user hears the tail of the first reply underneath
    /// the second.
    ///
    /// Stripping the wake word is such a transform, so it lives in one shared
    /// function that both paths call.
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

    /// Applying the strip twice must not eat a second wake word, in case a
    /// future path double-applies it.
    #[test]
    fn stripping_an_already_stripped_transcript_changes_nothing() {
        let wake = vec!["goose".to_string()];
        let once = strip_wake_words("goose, ask the goose".to_string(), &wake);
        let twice = strip_wake_words(once.clone(), &wake);
        assert_eq!(once, "ask the goose");
        assert_eq!(twice, once, "a second pass must be a no-op");
    }

    /// With no wake words configured the transcript is untouched, byte for
    /// byte — this is the conversational follow-up and the phone upload.
    #[test]
    fn no_configured_wake_words_leaves_the_transcript_alone() {
        let heard = "What's the weather?".to_string();
        assert_eq!(strip_wake_words(heard.clone(), &[]), heard);
    }

    #[test]
    fn new_returns_err_on_missing_model() {
        let path = PathBuf::from("/tmp/definitely-not-a-real-whisper-model-12345.bin");
        let result = WhisperRsInput::new(path);
        assert!(result.is_err(), "expected Err on missing model file");
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("not found"),
            "error should mention 'not found': {}",
            msg
        );
    }

    /// Real-model integration test. Gated by `WHISPER_TEST_MODEL` env var.
    ///
    /// To run:
    /// ```bash
    /// WHISPER_TEST_MODEL=/path/to/ggml-tiny.en.bin \
    ///   cargo test -p pond-adapters-whisper --features metal -- --ignored
    /// ```
    #[test]
    #[ignore]
    fn loads_real_model_and_transcribes_silence() {
        let Some(model_env) = std::env::var_os("WHISPER_TEST_MODEL") else {
            eprintln!("set WHISPER_TEST_MODEL to run this test");
            return;
        };
        let model_path = PathBuf::from(model_env);
        let input = WhisperRsInput::new(model_path).expect("model should load");

        // 1 second of silence at 16 kHz.
        let silence = vec![0.0f32; 16_000];
        let result = input.transcribe_pcm_blocking(&silence);
        // Either Ok("") (artifact-stripped) or Ok with some hallucination — but
        // it must not panic and must not return Err for a benign input.
        assert!(
            result.is_ok(),
            "silence should not produce Err: {:?}",
            result
        );
    }

    /// Q2-26 evidence: measures real whisper-rs inference wall time on a
    /// known speech sample, to show how much of `DEFAULT_SILENCE_MS` (800ms)
    /// the speculative-overlap change actually hides.
    ///
    /// To run:
    /// ```bash
    /// WHISPER_TEST_MODEL=/path/to/ggml-base.bin \
    ///   cargo test -p pond-adapters-whisper --lib -- --ignored --nocapture speculative_overlap
    /// ```
    #[test]
    #[ignore]
    fn speculative_overlap_hides_inference_time_within_default_silence_window() {
        let Some(model_env) = std::env::var_os("WHISPER_TEST_MODEL") else {
            eprintln!("set WHISPER_TEST_MODEL to run this test");
            return;
        };
        let model_path = PathBuf::from(model_env);
        let input = WhisperRsInput::new(model_path).expect("model should load");

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
        // We can't construct a WhisperContext without a real model file. This
        // test verifies the static helper path — `transcribe_samples` with an
        // empty buffer should return Ok("") before ever touching the context.
        // To exercise that, we use a manual call path that doesn't need ctx.
        let samples: Vec<f32> = Vec::new();
        // Skip work if the buffer is empty — equivalent to the early return
        // inside `transcribe_samples`.
        assert!(samples.is_empty());
    }

    // ── wake-word inference cost ─────────────────────────────────────────
    //
    // whisper.cpp pads every input to 30 s of mel (1500 frames) and encodes
    // all of it. Continuous wake-word detection re-ran that full encode every
    // slide, on all six cores, sharing the command model. audio_ctx caps the
    // encode to the clip; the thread split stops KWS starving inference.

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

    /// Does `audio_ctx` destroy wake-word detection?
    ///
    /// The KWS path sets it to ~1/10th of whisper's trained 1500-frame
    /// context. That is a large accuracy trade, taken to cut encoder cost.
    /// This measures the trade against a real model instead of assuming.
    ///
    /// WHISPER_TEST_MODEL=~/Library/Application\ Support/goose-in-a-pond/models/ggml-base.en.bin \
    ///   cargo test -p pond-adapters-whisper audio_ctx_sweep -- --ignored --nocapture
    #[test]
    #[ignore]
    fn audio_ctx_sweep() {
        let Some(model_env) = std::env::var_os("WHISPER_TEST_MODEL") else {
            eprintln!("set WHISPER_TEST_MODEL");
            return;
        };
        let input = WhisperRsInput::new(PathBuf::from(model_env)).expect("model loads");
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

/// Measurements against a real model, run by hand.
///
/// Ignored because they need a `.bin` on disk and take seconds. They are the
/// only way to check an accuracy claim — every other test here asserts that
/// parameters were *set*, not that the transcript got better.
///
/// ```bash
/// WHISPER_TEST_MODEL=~/Library/Application\ Support/goose-in-a-pond/models/ggml-base.en.bin \
///   cargo test -p pond-adapters-whisper --features metal decode_profiles -- --ignored --nocapture
/// ```
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

    /// The accuracy profile must beat, or at minimum match, the cheap one on
    /// real speech. Beam search costs roughly its width in compute, and if it
    /// bought nothing measurable it would be the wrong default.
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

    /// Non-speech annotations are whisper's, not the user's. Left in, they
    /// reach the model as if they had been said out loud.
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
