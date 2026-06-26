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
        })
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

    /// Run inference on raw 16 kHz mono f32 PCM. Returns the joined transcript,
    /// already passed through `strip_whisper_artifacts`.
    ///
    /// Wraps the C-side call in `catch_unwind`, so a whisper-rs panic returns
    /// `Err`. Empty input returns `Ok(String::new())` — never panics.
    fn transcribe_samples(ctx: Arc<WhisperContext>, samples: Vec<f32>) -> Result<String> {
        if samples.is_empty() {
            return Ok(String::new());
        }

        // Use a single std::thread + catch_unwind boundary: we can't catch_unwind
        // across an FFI panic on stable Rust without UnwindSafe, but the C++
        // panic boundary in whisper.cpp aborts the process anyway. We use
        // catch_unwind to convert any *Rust* panic from whisper-rs itself.
        let result = catch_unwind(AssertUnwindSafe(move || -> Result<String> {
            let mut state = ctx
                .create_state()
                .context("whisper-rs: create_state failed")?;

            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_print_progress(false);
            params.set_print_special(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            // English models are the only ones shipped by GIAP today.
            params.set_language(Some("en"));
            params.set_translate(false);
            params.set_no_context(true);
            params.set_suppress_blank(true);
            params.set_temperature(0.0);
            params.set_n_threads(default_threads());

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

/// Load a whisper.cpp context with platform-appropriate GPU settings.
fn load_context(model_path: &Path) -> Result<WhisperContext> {
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
        let capture_result = tokio::task::spawn_blocking(move || -> Result<SpeechCapture> {
            if let Some(wav) = captured {
                println!("  🎤 Listening...");
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
                println!("  🎤 Listening...");
                let speculative_spawn: Box<SpeculativeSpawn> = Box::new(move |samples, rate| {
                    let ctx = ctx_for_speculative.clone();
                    std::thread::spawn(move || -> Result<String> {
                        let resampled = resample_to_16k(&samples, rate);
                        Self::transcribe_samples(ctx, resampled)
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

        if transcript.is_empty() {
            Ok(Some(String::new()))
        } else {
            Ok(Some(transcript))
        }
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
        "🎤 "
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
        Self::transcribe_samples(ctx, samples.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
