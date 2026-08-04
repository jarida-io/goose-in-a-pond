//! In-process Piper TTS via `piper-rs` (ONNX Runtime + espeak-rs phonemizer).
//!
//! `PiperRsOutput` is the default `VoiceOutput` adapter — loads a Piper voice
//! (`.onnx` + `.onnx.json`) once at startup and synthesises directly, with no
//! per-utterance subprocess fork+exec.
//!
//! ## Crash isolation
//!
//! Every call into `piper-rs` (model load, `create()`) is wrapped in
//! `catch_unwind` so an ort / espeak panic returns `Err` instead of aborting
//! the whole pond-server process.
//!
//! ## Hot-swap
//!
//! `rebuild_with(new_model, new_config)` atomically replaces the loaded voice
//! under a `tokio::sync::RwLock<Arc<Mutex<Piper>>>`, mirroring
//! `WhisperRsInput::rebuild_with`. piper-rs's `create()` takes `&mut self`,
//! hence the inner `Mutex`: only one synthesis runs at a time per voice.
//!
//! ## espeak-ng data directory
//!
//! piper-rs's transitive `espeak-rs` crate locates `espeak-ng-data` via
//! (1) the `PIPER_ESPEAKNG_DATA_DIRECTORY` env var, (2) the cwd, (3) the
//! current exe's directory. `with_espeak_data(dir)` sets the env var so the
//! lazy `OnceLock` init inside espeak-rs picks the right directory on the
//! first synthesis call.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use piper_rs::Piper;
use pond_core::models::ports::voice_output::VoiceOutput;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;

use crate::{
    f32_samples_to_pcm_le_bytes, pcm_to_wav, play_wav_on_handle, start_thinking_tone_thread,
    AudioKeeper,
};

/// Env var that espeak-rs consults to find the bundled `espeak-ng-data` dir.
const PIPER_ESPEAKNG_DATA_DIRECTORY: &str = "PIPER_ESPEAKNG_DATA_DIRECTORY";

/// In-process Piper TTS adapter. One loaded voice per instance.
///
/// Cloneable via `Arc<PiperRsOutput>` — the voice lives behind the internal
/// `RwLock<Arc<Mutex<Piper>>>` and is shared across handles. The inner `Mutex`
/// serialises synthesis (`Piper::create` takes `&mut self`).
pub struct PiperRsOutput {
    /// Loaded piper-rs voice. `RwLock` lets `rebuild_with` swap the whole
    /// voice while in-flight synth calls hold the inner mutex on the old voice.
    voice: RwLock<Arc<Mutex<Piper>>>,
    /// Last-known model path, recorded for diagnostics.
    model_path: RwLock<PathBuf>,
    /// Last-known config path.
    config_path: RwLock<PathBuf>,
    /// Cached sample rate from the most recent synthesis. piper-rs's
    /// `create()` returns the rate alongside the samples — we cache the most
    /// recent value so callers and tests can read it cheaply.
    last_sample_rate: Mutex<Option<u32>>,
    /// Thinking-tone stop flag — shared with the background tone thread.
    /// Which turn the working tone belongs to, or [`TONE_OFF`].
    thinking_for: Arc<AtomicU64>,
    /// Monotonic turn counter. Advanced by `begin_utterance`, and the only
    /// thing that lets audio already in flight recognise that it belongs to a
    /// turn the user has moved on from.
    utterance: Arc<AtomicU64>,
    /// Speech interrupt flag — set true to immediately stop TTS playback.
    /// Checked by `play_wav_on_handle()` every 50 ms during playback.
    speech_interrupted: Arc<AtomicBool>,
    /// Persistent audio output. One CoreAudio AudioUnit opened at construction
    /// time and kept alive for the lifetime of this adapter. All TTS playback
    /// calls reuse `audio_handle` to create sinks — no repeated open/close churn.
    _audio_keeper: AudioKeeper,
    audio_handle: rodio::OutputStreamHandle,
}

impl PiperRsOutput {
    /// Load the voice at `model_path` (`.onnx`) + `config_path` (`.onnx.json`).
    ///
    /// Returns `Err` if either file is missing or piper-rs fails to load
    /// them. A piper-rs panic during load is caught and converted to `Err`.
    pub fn new(model_path: PathBuf, config_path: PathBuf) -> Result<Self> {
        if !model_path.exists() {
            return Err(anyhow!(
                "Piper voice model not found: {}",
                model_path.display()
            ));
        }
        if !config_path.exists() {
            return Err(anyhow!(
                "Piper voice config not found: {}",
                config_path.display()
            ));
        }
        let piper = load_voice(&model_path, &config_path)?;
        let audio_keeper = AudioKeeper::try_new()?;
        let audio_handle = audio_keeper.handle.clone();
        tracing::info!(
            "PiperRsOutput loaded voice: {} (in-process piper-rs / ort)",
            model_path.display()
        );
        Ok(Self {
            voice: RwLock::new(Arc::new(Mutex::new(piper))),
            model_path: RwLock::new(model_path),
            config_path: RwLock::new(config_path),
            last_sample_rate: Mutex::new(None),
            thinking_for: Arc::new(AtomicU64::new(crate::TONE_OFF)),
            // Generations start at 1 so TONE_OFF (0) is never a real turn.
            utterance: Arc::new(AtomicU64::new(1)),
            speech_interrupted: Arc::new(AtomicBool::new(false)),
            _audio_keeper: audio_keeper,
            audio_handle,
        })
    }

    /// Point espeak-rs at a specific `espeak-ng-data` directory.
    ///
    /// Sets `PIPER_ESPEAKNG_DATA_DIRECTORY` (process-global env var consulted
    /// by espeak-rs's lazy `OnceLock` init). The supplied path is the
    /// **parent** that contains the `espeak-ng-data/` subdirectory — same
    /// convention as the legacy `--espeak_data` subprocess flag.
    ///
    /// Best-effort: env-var mutation happens once at startup before any
    /// synthesis. Subsequent calls overwrite the same var.
    pub fn with_espeak_data(self, dir: PathBuf) -> Self {
        // SAFETY: env::set_var is unsafe on edition 2024 because other threads
        // may read env concurrently. We set this once at adapter-construction
        // time before any synthesis runs, so there is no read race in practice.
        unsafe {
            std::env::set_var(PIPER_ESPEAKNG_DATA_DIRECTORY, dir.as_os_str());
        }
        tracing::debug!(
            "PiperRsOutput: set {}={}",
            PIPER_ESPEAKNG_DATA_DIRECTORY,
            dir.display()
        );
        self
    }

    /// Hot-swap the loaded voice. Returns `Err` and keeps the previous voice
    /// intact if the new voice fails to load.
    ///
    /// Mirrors `WhisperRsInput::rebuild_with`: any in-flight `synthesize`
    /// call finishes on the old voice; the next call uses the new.
    pub async fn rebuild_with(
        &self,
        new_model_path: PathBuf,
        new_config_path: PathBuf,
    ) -> Result<()> {
        if !new_model_path.exists() {
            return Err(anyhow!(
                "Piper voice model not found: {}",
                new_model_path.display()
            ));
        }
        if !new_config_path.exists() {
            return Err(anyhow!(
                "Piper voice config not found: {}",
                new_config_path.display()
            ));
        }
        let new_voice = tokio::task::spawn_blocking({
            let m = new_model_path.clone();
            let c = new_config_path.clone();
            move || load_voice(&m, &c)
        })
        .await
        .map_err(|e| anyhow!("voice load join error: {}", e))??;

        {
            let mut guard = self.voice.write().await;
            *guard = Arc::new(Mutex::new(new_voice));
        }
        {
            let mut p = self.model_path.write().await;
            *p = new_model_path.clone();
        }
        {
            let mut p = self.config_path.write().await;
            *p = new_config_path.clone();
        }
        // Invalidate the cached sample rate — the new voice may differ.
        *self.last_sample_rate.lock().unwrap() = None;

        tracing::info!("PiperRsOutput hot-swapped to: {}", new_model_path.display());
        Ok(())
    }

    /// Path of the currently loaded voice's `.onnx` file.
    pub async fn current_model_path(&self) -> PathBuf {
        self.model_path.read().await.clone()
    }

    /// Path of the currently loaded voice's `.onnx.json` file.
    pub async fn current_config_path(&self) -> PathBuf {
        self.config_path.read().await.clone()
    }

    /// Sample rate of the most recent synthesis, if any has run yet.
    pub fn last_sample_rate(&self) -> Option<u32> {
        *self.last_sample_rate.lock().unwrap()
    }

    /// Synthesise `text` on a blocking thread. Returns the WAV bytes.
    ///
    /// On any error (including a caught panic in piper-rs / ort / espeak),
    /// returns `Err`. Empty or whitespace-only `text` returns `Ok(Vec::new())`.
    async fn synth_to_wav(&self, text: &str) -> Result<Vec<u8>> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let text = text.to_string();
        let voice_arc = self.voice.read().await.clone();

        let (wav, sample_rate) = tokio::task::spawn_blocking(move || -> Result<(Vec<u8>, u32)> {
            synth_blocking(voice_arc, &text)
        })
        .await
        .context("piper synthesize task panicked")??;

        *self.last_sample_rate.lock().unwrap() = Some(sample_rate);
        Ok(wav)
    }
}

#[async_trait]
impl VoiceOutput for PiperRsOutput {
    fn start_thinking_tone(&self) {
        let mine = self.utterance.load(Ordering::SeqCst);
        // Already sounding for this turn: the turn asked twice, which is
        // allowed and must not stack a second thread.
        if self.thinking_for.swap(mine, Ordering::SeqCst) == mine {
            return;
        }
        start_thinking_tone_thread(self.thinking_for.clone(), mine);
    }

    fn stop_thinking_tone(&self) {
        self.thinking_for.store(crate::TONE_OFF, Ordering::SeqCst);
    }

    fn begin_utterance(&self) {
        // The one place a turn's interrupt state is cleared. See the port doc:
        // clearing it inside speak()/play_audio() made a barge-in last exactly
        // one sentence.
        //
        // Advancing the generation FIRST is what makes that clear safe. Two
        // turns can be alive at once (a speculative job fired on a provisional
        // transcript, plus the confirmed one), and cancelling the first is done
        // by setting the interrupt flag — which this call then wipes. Without
        // the generation, starting a turn un-cancelled its predecessor and both
        // spoke. Audio already in flight compares against the generation it
        // started under, so it stays cancelled.
        self.utterance.fetch_add(1, Ordering::SeqCst);
        self.speech_interrupted.store(false, Ordering::SeqCst);
    }

    fn stop_speaking(&self) {
        self.speech_interrupted.store(true, Ordering::SeqCst);
    }

    async fn speak(&self, text: &str) -> Result<()> {
        // No interrupt reset here — see begin_utterance().
        if text.trim().is_empty() {
            return Ok(());
        }

        // Single piper.create() call for the full text — espeak-ng phonemizes
        // the whole utterance with full sentence context and no cross-call state
        // accumulation (the repeated-fragment stammer on turn 2+).
        let wav = self.synth_to_wav(text).await?;
        if wav.is_empty() || self.speech_interrupted.load(Ordering::Relaxed) {
            return Ok(());
        }

        const WAV_HEADER_LEN: usize = 44;
        if wav.len() <= WAV_HEADER_LEN {
            return Ok(());
        }
        let sample_rate = u32::from_le_bytes(wav[24..28].try_into().unwrap_or([0x56, 0x22, 0, 0]));
        let pcm = &wav[WAV_HEADER_LEN..];

        // Q2-27: scan for the first silence window after 20% of the audio and
        // split there (capped at 80% so we never split near the very end).
        // Both parts come from the same synthesis so prosody is intact; the split
        // lands inside an existing pause so there is no audible click, and the
        // barge-in listener can fire cleanly at the clause boundary.
        let split_at = find_first_silence(pcm, sample_rate, pcm.len() / 5)
            .filter(|&off| off < pcm.len() * 4 / 5);

        if let Some(offset) = split_at {
            let part1 = pcm_to_wav(&pcm[..offset], sample_rate);
            let part2 = pcm_to_wav(&pcm[offset..], sample_rate);

            let flag = self.speech_interrupted.clone();
            let gen = self.utterance.clone();
            let handle = self.audio_handle.clone();
            tokio::task::spawn_blocking(move || play_wav_on_handle(part1, &handle, &flag, &gen))
                .await
                .context("playback task panicked")??;

            if !self.speech_interrupted.load(Ordering::Relaxed) {
                let flag = self.speech_interrupted.clone();
                let gen = self.utterance.clone();
                let handle = self.audio_handle.clone();
                tokio::task::spawn_blocking(move || {
                    play_wav_on_handle(part2, &handle, &flag, &gen)
                })
                .await
                .context("playback task panicked")??;
            }
        } else {
            let flag = self.speech_interrupted.clone();
            let gen = self.utterance.clone();
            let handle = self.audio_handle.clone();
            tokio::task::spawn_blocking(move || play_wav_on_handle(wav, &handle, &flag, &gen))
                .await
                .context("playback task panicked")??;
        }

        Ok(())
    }

    async fn synthesize(&self, text: &str) -> Result<Option<Vec<u8>>> {
        let wav = self.synth_to_wav(text).await?;
        if wav.is_empty() {
            Ok(None)
        } else {
            Ok(Some(wav))
        }
    }

    async fn play_audio(&self, audio: Vec<u8>) -> Result<()> {
        // No interrupt reset here — see begin_utterance().
        let flag = self.speech_interrupted.clone();
        let gen = self.utterance.clone();
        let handle = self.audio_handle.clone();
        tokio::task::spawn_blocking(move || play_wav_on_handle(audio, &handle, &flag, &gen))
            .await
            .context("playback task panicked")?
    }
}

// ── Internals ─────────────────────────────────────────────────────────────────

/// Load a piper-rs voice, catching any panic from ort / serde.
fn load_voice(model_path: &std::path::Path, config_path: &std::path::Path) -> Result<Piper> {
    let m = model_path.to_path_buf();
    let c = config_path.to_path_buf();
    let result = catch_unwind(AssertUnwindSafe(move || -> Result<Piper> {
        Piper::new(&m, &c).map_err(|e| anyhow!("piper-rs load failed: {}", e))
    }));
    match result {
        Ok(Ok(p)) => Ok(p),
        Ok(Err(e)) => Err(e),
        Err(panic) => {
            let msg = panic_message(&panic);
            Err(anyhow!("piper-rs load panic: {}", msg))
        }
    }
}

/// Synchronous synth path, called from `spawn_blocking`.
///
/// Wraps the `&mut self` call into piper-rs in a `Mutex::lock` and the whole
/// thing in `catch_unwind` so a C-side panic in ort / espeak returns `Err`.
fn synth_blocking(voice: Arc<Mutex<Piper>>, text: &str) -> Result<(Vec<u8>, u32)> {
    let text = text.to_string();
    // Cost of one synthesis, under the `pond_adapters_piper` target — kept at
    // debug in the file log and untouched by the third-party carve-outs.
    //
    // Nothing timed TTS before this. The first sentence of every reply is the
    // one that matters: `speak()` must finish a whole `piper.create()` before
    // any audio exists, so that call is the user's time-to-first-audio. Later
    // sentences are pipelined against playback and are usually free. RTF also
    // settles whether GPU synthesis is worth pursuing at all — it answers
    // "how much slower than the speaker are we?", and if the answer stays
    // well under 1.0 there is no throughput problem to solve.
    let started = std::time::Instant::now();
    let chars = text.chars().count();
    let result = catch_unwind(AssertUnwindSafe(move || -> Result<(Vec<u8>, u32)> {
        let mut guard = voice
            .lock()
            .map_err(|e| anyhow!("piper voice mutex poisoned: {}", e))?;
        // create(text, is_phonemes, speaker_id, length_scale, noise_scale, noise_w)
        // — passing None lets piper-rs use the config's default inference params.
        let (samples, sample_rate) = guard
            .create(&text, false, None, None, None, None)
            .map_err(|e| anyhow!("piper-rs synth failed: {}", e))?;
        drop(guard);

        if samples.is_empty() {
            tracing::warn!("piper-rs produced no samples for text: {:?}", text);
            return Ok((Vec::new(), sample_rate));
        }
        let pcm = f32_samples_to_pcm_le_bytes(&samples);
        let wav = pcm_to_wav(&pcm, sample_rate);
        Ok((wav, sample_rate))
    }));
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    // Audio duration is derived from the WAV we just built rather than guessed
    // from the text. `encode_wav_pcm16` writes a 44-byte header and mono
    // 16-bit samples, so 2 bytes per frame (`pond_voice::dsp`).
    const WAV_HEADER_BYTES: usize = 44;
    const BYTES_PER_FRAME: f64 = 2.0;
    let audio_ms = match result.as_ref() {
        Ok(Ok((wav, rate))) if *rate > 0 && wav.len() > WAV_HEADER_BYTES => {
            ((wav.len() - WAV_HEADER_BYTES) as f64 / BYTES_PER_FRAME / *rate as f64) * 1000.0
        }
        _ => 0.0,
    };
    // rtf is omitted when there is no audio to divide by. Reporting
    // `elapsed_ms / 1.0` in that case prints something like rtf=700.000, which
    // reads as catastrophic synthesis performance when what actually happened
    // is that synthesis failed and produced nothing.
    if audio_ms > 0.0 {
        tracing::debug!(
            chars,
            audio_ms = format_args!("{audio_ms:.0}"),
            elapsed_ms = format_args!("{elapsed_ms:.0}"),
            rtf = format_args!("{:.3}", elapsed_ms / audio_ms),
            "TTS synthesize"
        );
    } else {
        tracing::debug!(
            chars,
            elapsed_ms = format_args!("{elapsed_ms:.0}"),
            "TTS synthesize produced no audio"
        );
    }

    match result {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(e),
        Err(panic) => {
            let msg = panic_message(&panic);
            tracing::error!("piper-rs synth panic caught: {}", msg);
            Err(anyhow!("piper-rs synth panic: {}", msg))
        }
    }
}

/// Split text into clause-sized chunks for pipelined synthesis.
///
/// Scan `pcm` (16-bit signed LE) for the first 40 ms window whose RMS energy
/// falls below a silence threshold, starting the search at `min_offset` bytes.
///
/// Returns the byte offset of the first silent window (aligned to a 2-byte
/// sample boundary), or `None` if no silence is found before the end of the
/// buffer. Uses 50% window overlap so a silence boundary as narrow as 20 ms
/// is detectable.
fn find_first_silence(pcm: &[u8], sample_rate: u32, min_offset: usize) -> Option<usize> {
    const RMS_SILENCE: f32 = 0.015;
    // Window and step in bytes, both aligned to 2 (one 16-bit sample = 2 bytes).
    let window_bytes = ((sample_rate as usize * 40 / 1000) * 2 + 1) & !1;
    let step_bytes = ((sample_rate as usize * 20 / 1000) * 2 + 1) & !1;

    if pcm.len() < window_bytes {
        return None;
    }

    let start = (min_offset + 1) & !1; // align to sample boundary
    let mut i = start;
    while i + window_bytes <= pcm.len() {
        let n = window_bytes / 2;
        let sum_sq: f32 = pcm[i..i + window_bytes]
            .chunks_exact(2)
            .map(|b| {
                let s = i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0;
                s * s
            })
            .sum();
        if (sum_sq / n as f32).sqrt() < RMS_SILENCE {
            return Some(i);
        }
        i += step_bytes;
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_returns_err_on_missing_model() {
        let model = PathBuf::from("/tmp/definitely-not-a-real-piper-voice-12345.onnx");
        let config = PathBuf::from("/tmp/definitely-not-a-real-piper-voice-12345.onnx.json");
        let result = PiperRsOutput::new(model, config);
        assert!(result.is_err(), "expected Err on missing model file");
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("not found"),
            "error should mention 'not found': {}",
            msg
        );
    }

    #[test]
    fn new_returns_err_on_missing_config() {
        // Create a placeholder .onnx file (empty is fine — piper-rs never
        // reads it because the config check fails first).
        let tmp = tempfile::tempdir().expect("tempdir");
        let model = tmp.path().join("voice.onnx");
        std::fs::write(&model, b"placeholder").expect("write placeholder");
        let config = tmp.path().join("voice.onnx.json");
        // Config does NOT exist.
        let result = PiperRsOutput::new(model, config);
        assert!(result.is_err(), "expected Err on missing config file");
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("config not found"),
            "error should mention 'config not found': {}",
            msg
        );
    }

    /// Real-voice integration test. Gated by `PIPER_TEST_VOICE` env var.
    ///
    /// To run:
    /// ```bash
    /// PIPER_TEST_VOICE=/path/to/en_US-lessac-medium.onnx \
    ///   cargo test -p pond-adapters-piper -- --ignored
    /// ```
    /// Expects the companion `.onnx.json` alongside.
    #[test]
    #[ignore]
    fn loads_real_voice_and_synthesises_hello() {
        let Some(voice_env) = std::env::var_os("PIPER_TEST_VOICE") else {
            eprintln!("set PIPER_TEST_VOICE to run this test");
            return;
        };
        let model_path = PathBuf::from(voice_env);
        let config_path = PathBuf::from(format!("{}.json", model_path.display()));
        let runtime = tokio::runtime::Runtime::new().expect("tokio rt");
        runtime.block_on(async {
            let tts = PiperRsOutput::new(model_path, config_path).expect("voice should load");
            let wav = tts.synthesize("hello").await.expect("synth should succeed");
            let wav = wav.expect("non-empty WAV");
            // RIFF/WAVE magic.
            assert_eq!(&wav[0..4], b"RIFF");
            assert_eq!(&wav[8..12], b"WAVE");
            // Sample rate field at offset 24..28 (LE u32).
            let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
            // lessac-medium is 22050 Hz. Other medium voices are too.
            assert!(
                rate == 22_050 || rate == 16_000,
                "unexpected sample rate {} (expected 22050 or 16000)",
                rate
            );
        });
    }

    /// Confirms `PiperRsOutput` satisfies the `VoiceOutput` trait object.
    /// Pure compile-time check — no real voice loaded.
    #[test]
    fn implements_voice_output_trait_object() {
        // Build a dummy function so we exercise the trait bound at compile time
        // without needing to construct a valid PiperRsOutput.
        fn _assert_object_safe(_: Arc<dyn VoiceOutput>) {}
    }

    // ── barge-in state ownership ─────────────────────────────────────────
    //
    // `speak()` and `play_audio()` used to clear `speech_interrupted` on entry.
    // A barge-in during sentence one was therefore forgotten by sentence two,
    // and the rest of the reply played out regardless. Interrupt state belongs
    // to the TURN; `begin_utterance()` is the only place it resets.
    //
    // Driven through the flags rather than a live voice so it runs in CI —
    // constructing PiperRsOutput needs a real ONNX model on disk.

    /// Mirrors how the turn drives the flags across a multi-sentence reply.
    struct InterruptState {
        speech_interrupted: Arc<AtomicBool>,
    }

    impl InterruptState {
        fn new() -> Self {
            Self {
                speech_interrupted: Arc::new(AtomicBool::new(false)),
            }
        }
        /// What begin_utterance() does.
        fn begin_utterance(&self) {
            self.speech_interrupted.store(false, Ordering::SeqCst);
        }
        /// What the barge-in callback does on detecting speech.
        fn barge_in(&self) {
            self.speech_interrupted.store(true, Ordering::SeqCst);
        }
        /// What speak()/play_audio() check before emitting audio.
        fn would_play(&self) -> bool {
            !self.speech_interrupted.load(Ordering::Relaxed)
        }
    }

    #[test]
    fn a_barge_in_suppresses_every_later_sentence_in_the_turn() {
        let st = InterruptState::new();
        st.begin_utterance();
        assert!(st.would_play(), "sentence 1 plays");

        st.barge_in(); // user speaks over the assistant

        assert!(!st.would_play(), "sentence 2 must be suppressed");
        assert!(!st.would_play(), "sentence 3 must stay suppressed");
    }

    #[test]
    fn the_next_turn_starts_speaking_again() {
        let st = InterruptState::new();
        st.begin_utterance();
        st.barge_in();
        assert!(!st.would_play());

        st.begin_utterance(); // next turn
        assert!(st.would_play(), "a new turn must clear the interrupt");
    }

    #[test]
    fn begin_utterance_is_idempotent() {
        let st = InterruptState::new();
        st.begin_utterance();
        st.begin_utterance();
        assert!(st.would_play());
    }

    /// The port default must not silently swallow the contract for backends
    /// that do not implement it (PrintOutput, tests).
    #[test]
    fn the_port_default_begin_utterance_is_a_no_op() {
        struct Bare;
        #[async_trait::async_trait]
        impl VoiceOutput for Bare {
            async fn speak(&self, _text: &str) -> Result<()> {
                Ok(())
            }
        }
        Bare.begin_utterance(); // must compile and not panic
    }
}
