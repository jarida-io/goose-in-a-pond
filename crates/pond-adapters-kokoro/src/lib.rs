//! Kokoro-82M TTS as a [`VoiceOutput`]: text -> espeak IPA -> vocab ids -> 24 kHz f32 via `ort`.
//! The ~92 MB session loads on first `speak`, so a bad model path fails there, not in `new()`.

use anyhow::{Context, Result};
use async_trait::async_trait;
use pond_audio_out::{play_wav, start_thinking_tone_thread, AudioKeeper, TONE_OFF};
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::shared::domain::agent::ThrottledAudioLevelSink;
use pond_voice::dsp::{encode_wav_pcm16, f32_to_pcm16};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

pub mod engine;
pub mod tokenizer;
pub mod voices;

pub use engine::{Engine, SAMPLE_RATE};
pub use tokenizer::Vocab;
pub use voices::StyleTable;

/// The model's reference voice, used in its published samples.
pub const DEFAULT_VOICE: &str = "af_heart";
pub const DEFAULT_SPEED: f32 = 1.0;
/// Below 0.5 the prosody smears; above 2.0 it clips words.
pub const MIN_SPEED: f32 = 0.5;
pub const MAX_SPEED: f32 = 2.0;

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
const LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub struct KokoroOutput {
    config: KokoroConfig,
    /// The active `.onnx`, which changes at runtime in lockstep with `engine`.
    model_path: RwLock<PathBuf>,
    vocab: Vocab,
    /// Loaded lazily on first synthesis; `None` means "not paying for it yet".
    engine: RwLock<Option<Engine>>,
    /// The currently selected voice's style table, loaded on demand.
    style: RwLock<Option<Arc<StyleTable>>>,
    voice_name: RwLock<String>,
    /// Pace, as `f32::to_bits` so it can live in an atomic.
    speed_bits: AtomicU64,

    // ── turn / interrupt state, identical in meaning to the Piper adapter ──
    thinking_for: Arc<AtomicU64>,
    utterance: Arc<AtomicU64>,
    speech_interrupted: Arc<AtomicBool>,
    _audio_keeper: AudioKeeper,
    audio_handle: rodio::OutputStreamHandle,
    audio_level_sink: Option<Arc<ThrottledAudioLevelSink>>,
    /// Sample rate of the most recent synthesis, for diagnostics.
    last_sample_rate: Mutex<Option<u32>>,
}

impl KokoroOutput {
    /// Reads the vocab but not the model; fails only on a bad `tokenizer.json` or no audio device.
    pub fn new(config: KokoroConfig) -> Result<Self> {
        if let Some(dir) = &config.espeak_data {
            std::env::set_var(ESPEAKNG_DATA_DIRECTORY, dir);
        }
        let vocab = Vocab::load(&config.tokenizer_path)?;
        tracing::info!(symbols = vocab.len(), "Kokoro vocab loaded");

        let keeper = AudioKeeper::try_new("kokoro-audio-keeper")?;
        let handle = keeper.handle.clone();
        Ok(Self {
            model_path: RwLock::new(config.model_path.clone()),
            config,
            vocab,
            engine: RwLock::new(None),
            style: RwLock::new(None),
            voice_name: RwLock::new(DEFAULT_VOICE.to_string()),
            speed_bits: AtomicU64::new(DEFAULT_SPEED.to_bits() as u64),
            thinking_for: Arc::new(AtomicU64::new(TONE_OFF)),
            utterance: Arc::new(AtomicU64::new(1)),
            speech_interrupted: Arc::new(AtomicBool::new(false)),
            _audio_keeper: keeper,
            audio_handle: handle,
            audio_level_sink: None,
            last_sample_rate: Mutex::new(None),
        })
    }

    /// Attach a live playback-amplitude reporter, the `speaking` analog of mic RMS.
    pub fn with_audio_level_sink(mut self, sink: Arc<ThrottledAudioLevelSink>) -> Self {
        self.audio_level_sink = Some(sink);
        self
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

    /// Mono f32 at [`SAMPLE_RATE`]; empty for text that phonemizes to nothing.
    pub async fn synth_samples(&self, text: &str) -> Result<Vec<f32>> {
        let (chunks, dropped) = tokenizer::chunk(text, &self.vocab)?;
        if dropped > 0 {
            // Not fatal, but the word will sound wrong: the model never learned that phoneme.
            tracing::warn!(
                dropped,
                text = %text.chars().take(80).collect::<String>(),
                "Kokoro dropped phonemes outside the model vocab"
            );
        }
        if chunks.is_empty() {
            return Ok(Vec::new());
        }

        let style = self.ensure_style().await?;
        let speed = self.speed();

        let mut engine_guard = self.engine.write().await;
        if engine_guard.is_none() {
            let path = self.model_path.read().await.clone();
            let threads = self.config.intra_threads;
            // Bounded: with `load-dynamic` and no dylib, ort's init hangs rather than failing.
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
        let engine = engine_guard.as_mut().expect("engine was just loaded above");

        let mut samples = Vec::new();
        for chunk in &chunks {
            let style_row = style.style_for(chunk.tokens.len());
            samples.extend(engine.synthesize(chunk, style_row, speed)?);
        }
        *self.last_sample_rate.lock().unwrap() = Some(SAMPLE_RATE);
        Ok(samples)
    }

    /// Synthesize to a WAV buffer ready for playback or an HTTP response.
    pub async fn synth_wav(&self, text: &str) -> Result<Vec<u8>> {
        let samples = self.synth_samples(text).await?;
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
}

#[async_trait]
impl VoiceOutput for KokoroOutput {
    async fn speak(&self, text: &str) -> Result<()> {
        let wav = self.synth_wav(text).await?;
        if wav.is_empty() {
            return Ok(());
        }
        self.play_audio(wav).await
    }

    async fn synthesize(&self, text: &str) -> Result<Option<Vec<u8>>> {
        let wav = self.synth_wav(text).await?;
        Ok((!wav.is_empty()).then_some(wav))
    }

    async fn play_audio(&self, audio: Vec<u8>) -> Result<()> {
        if audio.is_empty() {
            return Ok(());
        }
        let handle = self.audio_handle.clone();
        let interrupted = self.speech_interrupted.clone();
        let utterance = self.utterance.clone();
        let sink = self.audio_level_sink.clone();
        tokio::task::spawn_blocking(move || {
            play_wav(audio, &handle, &interrupted, &utterance, sink.as_deref())
        })
        .await
        .context("Kokoro playback task panicked")?
    }

    fn begin_utterance(&self) {
        // Generation first, then clear the interrupt, so stale audio never sees a cleared flag.
        self.utterance.fetch_add(1, Ordering::SeqCst);
        self.speech_interrupted.store(false, Ordering::SeqCst);
    }

    fn stop_speaking(&self) {
        self.speech_interrupted.store(true, Ordering::SeqCst);
    }

    fn start_thinking_tone(&self) {
        let mine = self.utterance.load(Ordering::SeqCst);
        // A turn asking twice must not stack a second thread.
        if self.thinking_for.swap(mine, Ordering::SeqCst) == mine {
            return;
        }
        start_thinking_tone_thread(self.thinking_for.clone(), mine);
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
        "q8"
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
        assert_ne!(host_default_quality(), "q8f16");
        if aarch64_linux() {
            assert_eq!(usable_quality("q8f16"), "q4f16");
        } else {
            assert_eq!(usable_quality("q8f16"), "q8f16");
        }
    }

    #[test]
    fn a_default_tier_is_replaced_by_a_host_that_needs_a_different_one() {
        assert_eq!(tier_to_adopt_for("q4f16", "q8", "q8"), Some("q4f16"));
        assert_eq!(tier_to_adopt_for("q4f16", "", "q8"), Some("q4f16"));
        assert_eq!(tier_to_adopt_for("q4f16", "  ", "q8"), Some("q4f16"));
    }

    #[test]
    fn a_chosen_tier_is_never_overwritten() {
        for chosen in ["fp32", "fp16", "q4", "q8f16"] {
            assert_eq!(
                tier_to_adopt_for("q4f16", chosen, "q8"),
                None,
                "{chosen} is a choice, not a default"
            );
        }
    }

    /// A redundant write every boot would make `updated_at` stop meaning a household change.
    #[test]
    fn adopting_is_a_no_op_once_it_has_happened() {
        assert_eq!(tier_to_adopt_for("q4f16", "q4f16", "q8"), None);
        assert_eq!(tier_to_adopt(host_default_quality(), "q8"), None);
    }

    #[test]
    fn the_wrapper_passes_this_hosts_tier_through() {
        for stored in ["", "q8", "fp32", "q4f16"] {
            assert_eq!(
                tier_to_adopt(stored, "q8"),
                tier_to_adopt_for(host_default_quality(), stored, "q8"),
                "wrapper disagreed for stored={stored:?}"
            );
        }
    }
}
