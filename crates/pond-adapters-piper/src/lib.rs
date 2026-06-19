//! Piper TTS adapter for Goose In A Pond.
//!
//! Implements the `VoiceOutput` port by:
//!   1. Spawning `piper --model <model> --output-raw --quiet`
//!   2. Writing the text to piper's stdin, then closing it
//!   3. Reading raw 16-bit PCM from piper's stdout
//!   4. Wrapping the PCM in a minimal WAV header
//!   5. Playing the WAV through the default audio output via `rodio`
//!
//! Piper is a fast, local neural TTS engine.  The `en_US-lessac-medium`
//! voice outputs mono 16-bit PCM at 22 050 Hz.  Run `pond-server setup`
//! to download the binary and voice model automatically.
//!
//! # Usage
//! ```no_run
//! use pond_adapters_piper::PiperOutput;
//! use std::path::PathBuf;
//!
//! let tts = PiperOutput::new(
//!     PathBuf::from("/data/bin/piper"),
//!     PathBuf::from("/data/models/tts/en_US-lessac-medium.onnx"),
//! );
//! ```

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use pond_core::models::ports::voice_output::VoiceOutput;
use std::io::Write as _;
use std::path::PathBuf;

// ── Quips ─────────────────────────────────────────────────────────────────────

/// Short reassurance phrases spoken while the LLM starts inference.
/// Aim for 1-2 seconds of synthesised audio each.
const QUIPS: &[&str] = &[
    "On it.",
    "Let me think.",
    "Ruffling through possibilities.",
    "Consulting the pond elders.",
    "Wading into the knowledge pool.",
    "Hatching a response.",
    "Paddling upstream.",
    "Assembling ideas, feather by feather.",
    "Skimming the surface.",
    "One moment.",
    "Let me check.",
    "Thinking that through.",
];

/// Pick a quip using sub-millisecond time as a cheap source of variety.
fn pick_quip() -> &'static str {
    let idx = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0)
        % QUIPS.len();
    QUIPS[idx]
}

// ── Barge-in constants ────────────────────────────────────────────────────────

/// RMS energy threshold for speech detection during barge-in monitoring.
/// Samples above this trigger an interrupt. Tuned for typical desktop mics.
const BARGE_IN_RMS_THRESHOLD: f32 = 0.02;

/// Duration in milliseconds of each audio analysis chunk for barge-in.
const BARGE_IN_CHUNK_MS: u64 = 100;

// ── PiperOutput ───────────────────────────────────────────────────────────────

/// VoiceOutput adapter that synthesises speech via the Piper TTS subprocess.
pub struct PiperOutput {
    piper_bin: PathBuf,
    model: PathBuf,
    /// Sample rate of the model's raw PCM output.
    /// `en_US-lessac-medium` = 22 050 Hz.  Override with `with_sample_rate()`.
    sample_rate: u32,
    /// Optional path to the espeak-ng-data directory.
    /// When set, `--espeak_data <path>` is passed to piper.
    espeak_data: Option<PathBuf>,
    /// Thinking tone stop flag — shared with the background tone thread.
    thinking_active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Speech interrupt flag — set to true to immediately stop TTS playback.
    /// Checked by `play_wav_interruptible()` every 50ms during playback.
    speech_interrupted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Barge-in listener active flag — shared with the mic monitoring thread.
    barge_in_active: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl PiperOutput {
    /// Create a new `PiperOutput`.
    ///
    /// `piper_bin` — path to the piper executable.
    /// `model`     — path to the `.onnx` voice model file.
    pub fn new(piper_bin: PathBuf, model: PathBuf) -> Self {
        Self {
            piper_bin,
            model,
            sample_rate: 22_050,
            espeak_data: None,
            thinking_active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            speech_interrupted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            barge_in_active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Set the espeak-ng-data directory (passed as `--espeak_data` to piper).
    /// Required when piper was compiled against a different system path.
    pub fn with_espeak_data(mut self, path: PathBuf) -> Self {
        self.espeak_data = Some(path);
        self
    }

    /// Override the expected sample rate (default: 22 050 for lessac-medium).
    pub fn with_sample_rate(mut self, sample_rate: u32) -> Self {
        self.sample_rate = sample_rate;
        self
    }

    /// Assemble the piper command arguments.
    ///
    /// Exposed for integration tests that verify builder configuration without
    /// running a real piper binary. Not part of the stable public API.
    #[doc(hidden)]
    pub fn build_args(&self) -> Vec<String> {
        let mut args = vec![
            "--model".to_string(),
            self.model.to_string_lossy().to_string(),
            "--output-raw".to_string(),
            "--quiet".to_string(),
        ];
        if let Some(ref d) = self.espeak_data {
            args.push("--espeak_data".to_string());
            args.push(d.to_string_lossy().to_string());
        }
        args
    }
}

#[async_trait]
impl VoiceOutput for PiperOutput {
    fn start_thinking_tone(&self) {
        use std::sync::atomic::Ordering;
        // If already playing, don't spawn a second thread
        if self.thinking_active.swap(true, Ordering::SeqCst) {
            return;
        }
        let flag = self.thinking_active.clone();
        std::thread::spawn(move || {
            use rodio::{OutputStream, Sink};

            let Ok((_stream, handle)) = OutputStream::try_default() else {
                flag.store(false, Ordering::SeqCst);
                return;
            };
            let Ok(sink) = Sink::try_new(&handle) else {
                flag.store(false, Ordering::SeqCst);
                return;
            };
            sink.set_volume(0.08);

            let rate = 22050u32;
            let pulse_samples = rate as usize; // 1-second pulse
            let pulse: Vec<f32> = (0..pulse_samples)
                .map(|i| {
                    let t = i as f32 / rate as f32;
                    let envelope = (std::f32::consts::PI * t).sin();
                    (2.0 * std::f32::consts::PI * 440.0 * t).sin() * envelope * 0.5
                })
                .collect();

            while flag.load(Ordering::Relaxed) {
                let buf = rodio::buffer::SamplesBuffer::new(1, rate, pulse.clone());
                sink.append(buf);
                for _ in 0..10 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if !flag.load(Ordering::Relaxed) {
                        sink.stop();
                        return;
                    }
                }
            }
            sink.stop();
        });
    }

    fn stop_thinking_tone(&self) {
        self.thinking_active
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    fn stop_speaking(&self) {
        self.speech_interrupted
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn start_barge_in_listener(&self) {
        use std::sync::atomic::Ordering;

        // Don't spawn a second listener if one is already active
        if self.barge_in_active.swap(true, Ordering::SeqCst) {
            return;
        }

        let active_flag = self.barge_in_active.clone();
        let interrupt_flag = self.speech_interrupted.clone();

        std::thread::spawn(move || {
            use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

            let host = cpal::default_host();
            let device = match host.default_input_device() {
                Some(d) => d,
                None => {
                    tracing::debug!("Barge-in: no input device found");
                    active_flag.store(false, Ordering::SeqCst);
                    return;
                }
            };

            // Use the device's default input config
            let config = match device.default_input_config() {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!("Barge-in: no input config: {e}");
                    active_flag.store(false, Ordering::SeqCst);
                    return;
                }
            };

            let sample_rate = config.sample_rate().0;
            let channels = config.channels() as usize;
            // Number of samples per analysis window
            let chunk_samples = (sample_rate as u64 * BARGE_IN_CHUNK_MS / 1000) as usize * channels;

            let rms_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<f32>::with_capacity(
                chunk_samples,
            )));
            let rms_buf_write = rms_buf.clone();
            let active_for_callback = active_flag.clone();
            let interrupt_for_callback = interrupt_flag.clone();

            let stream_config: cpal::StreamConfig = config.into();

            let stream = device.build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if !active_for_callback.load(Ordering::Relaxed) {
                        return;
                    }
                    let mut buf = rms_buf_write.lock().unwrap();
                    buf.extend_from_slice(data);

                    if buf.len() >= chunk_samples {
                        // Compute RMS of the accumulated chunk
                        let sum_sq: f32 = buf.iter().map(|s| s * s).sum();
                        let rms = (sum_sq / buf.len() as f32).sqrt();
                        buf.clear();

                        if rms > BARGE_IN_RMS_THRESHOLD {
                            tracing::debug!("Barge-in: speech detected (RMS={rms:.4})");
                            interrupt_for_callback.store(true, Ordering::SeqCst);
                            active_for_callback.store(false, Ordering::SeqCst);
                        }
                    }
                },
                move |err| {
                    tracing::debug!("Barge-in stream error: {err}");
                },
                None,
            );

            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("Barge-in: failed to build input stream: {e}");
                    active_flag.store(false, Ordering::SeqCst);
                    return;
                }
            };

            if let Err(e) = stream.play() {
                tracing::debug!("Barge-in: failed to start stream: {e}");
                active_flag.store(false, Ordering::SeqCst);
                return;
            }

            // Keep the stream alive while the listener is active
            while active_flag.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            // Stream is dropped here, releasing the mic
        });
    }

    fn stop_barge_in_listener(&self) {
        self.barge_in_active
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    async fn speak_quip(&self) -> Option<&'static str> {
        let quip = pick_quip();
        if let Err(e) = self.speak(quip).await {
            tracing::debug!("Quip TTS failed: {e}");
            return None;
        }
        Some(quip)
    }

    async fn speak(&self, text: &str) -> Result<()> {
        // Clear interrupt flag before this utterance
        self.speech_interrupted
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let bin = self.piper_bin.clone();
        let model = self.model.clone();
        let sample_rate = self.sample_rate;
        let text = text.to_string();
        let espeak_data = self.espeak_data.clone();
        let flag = self.speech_interrupted.clone();

        // Synthesize then play with interrupt support
        tokio::task::spawn_blocking(move || {
            let wav =
                synthesize_blocking(&bin, &model, espeak_data.as_deref(), sample_rate, &text)?;
            if wav.is_empty() {
                return Ok(());
            }
            play_wav_interruptible(wav, &flag)
        })
        .await
        .context("piper speak task panicked")??;

        Ok(())
    }

    async fn synthesize(&self, text: &str) -> Result<Option<Vec<u8>>> {
        let bin = self.piper_bin.clone();
        let model = self.model.clone();
        let sample_rate = self.sample_rate;
        let text = text.to_string();
        let espeak_data = self.espeak_data.clone();

        let wav = tokio::task::spawn_blocking(move || {
            synthesize_blocking(&bin, &model, espeak_data.as_deref(), sample_rate, &text)
        })
        .await
        .context("piper synthesize task panicked")??;

        if wav.is_empty() {
            Ok(None)
        } else {
            Ok(Some(wav))
        }
    }

    async fn play_audio(&self, audio: Vec<u8>) -> Result<()> {
        // Clear the interrupt flag before playback starts
        self.speech_interrupted
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let flag = self.speech_interrupted.clone();
        tokio::task::spawn_blocking(move || play_wav_interruptible(audio, &flag))
            .await
            .context("playback task panicked")?
    }
}

impl PiperOutput {
    /// Synthesize text to raw WAV bytes without playing.
    ///
    /// This is the first half of the speak pipeline: it runs piper and
    /// returns the PCM-wrapped-in-WAV buffer for later playback. By
    /// separating synthesis from playback, callers can pipeline: synthesize
    /// the NEXT sentence while the current one is still playing.
    pub async fn synthesize(&self, text: &str) -> Result<Vec<u8>> {
        let bin = self.piper_bin.clone();
        let model = self.model.clone();
        let sample_rate = self.sample_rate;
        let text = text.to_string();
        let espeak_data = self.espeak_data.clone();

        tokio::task::spawn_blocking(move || {
            synthesize_blocking(&bin, &model, espeak_data.as_deref(), sample_rate, &text)
        })
        .await
        .context("piper synthesize task panicked")?
    }

    /// Play pre-synthesized WAV bytes through the speaker.
    ///
    /// This is the second half of the speak pipeline. Blocks until
    /// playback finishes.
    pub async fn play(&self, wav: Vec<u8>) -> Result<()> {
        tokio::task::spawn_blocking(move || play_wav(wav))
            .await
            .context("playback task panicked")?
    }
}

// ── Blocking implementation ───────────────────────────────────────────────────

/// Synthesize text to WAV bytes without playing. Returns the WAV buffer.
fn synthesize_blocking(
    piper_bin: &std::path::Path,
    model: &std::path::Path,
    espeak_data: Option<&std::path::Path>,
    sample_rate: u32,
    text: &str,
) -> Result<Vec<u8>> {
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(piper_bin);
    cmd.args(["--model", &model.to_string_lossy()])
        .args(["--output-raw", "--quiet"]);
    if let Some(d) = espeak_data {
        cmd.args(["--espeak_data", &d.to_string_lossy()]);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn piper at {}", piper_bin.display()))?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("piper stdin unavailable"))?;
        stdin
            .write_all(text.as_bytes())
            .context("Failed to write text to piper stdin")?;
    }

    let output = child
        .wait_with_output()
        .context("Failed to wait for piper")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            return Err(anyhow!("piper exited with status {}", output.status));
        }
        return Err(anyhow!(
            "piper exited with status {}: {}",
            output.status,
            stderr
        ));
    }

    let pcm = output.stdout;
    if pcm.is_empty() {
        tracing::warn!("piper produced no PCM output for text: {:?}", text);
        return Ok(Vec::new());
    }

    Ok(pcm_to_wav(&pcm, sample_rate))
}

fn speak_blocking(
    piper_bin: &std::path::Path,
    model: &std::path::Path,
    espeak_data: Option<&std::path::Path>,
    sample_rate: u32,
    text: &str,
) -> Result<()> {
    use std::process::{Command, Stdio};

    // Spawn piper, pipe stdin + stdout.  Capture stderr so we can include it
    // in the error message if piper exits non-zero.
    let mut cmd = Command::new(piper_bin);
    cmd.args(["--model", &model.to_string_lossy()])
        .args(["--output-raw", "--quiet"]);
    if let Some(d) = espeak_data {
        cmd.args(["--espeak_data", &d.to_string_lossy()]);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn piper at {}", piper_bin.display()))?;

    // Write text to stdin and close it so piper knows there is no more input.
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("piper stdin unavailable"))?;
        stdin
            .write_all(text.as_bytes())
            .context("Failed to write text to piper stdin")?;
        // `stdin` dropped here → EOF signalled to piper
    }

    // Read all raw PCM from stdout.
    let output = child
        .wait_with_output()
        .context("Failed to wait for piper")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            return Err(anyhow!("piper exited with status {}", output.status));
        }
        return Err(anyhow!(
            "piper exited with status {}: {}",
            output.status,
            stderr
        ));
    }

    let pcm = output.stdout;
    if pcm.is_empty() {
        tracing::warn!("piper produced no PCM output for text: {:?}", text);
        return Ok(());
    }

    // Wrap raw PCM in a WAV container so rodio can decode it.
    let wav = pcm_to_wav(&pcm, sample_rate);

    // Play through the default audio output device.
    play_wav(wav)
}

// ── WAV encoder ───────────────────────────────────────────────────────────────

/// Wrap raw 16-bit mono PCM in a minimal RIFF/WAV container.
fn pcm_to_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let channels: u16 = 1;
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
    let block_align = channels * bits_per_sample / 8;
    let data_len = pcm.len() as u32;
    let riff_len = 36 + data_len;

    let mut wav = Vec::with_capacity(44 + pcm.len());
    // RIFF chunk
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_len.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    // fmt  sub-chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    // data sub-chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

// ── Audio playback ────────────────────────────────────────────────────────────

fn play_wav(wav: Vec<u8>) -> Result<()> {
    play_wav_interruptible(wav, &std::sync::atomic::AtomicBool::new(false))
}

/// Play WAV audio with interrupt support.
///
/// Polls the `interrupted` flag every 50ms. When set to true, immediately
/// stops the rodio sink and returns Ok. This enables wake-word interruption
/// of TTS playback with <50ms response time.
fn play_wav_interruptible(wav: Vec<u8>, interrupted: &std::sync::atomic::AtomicBool) -> Result<()> {
    use rodio::{Decoder, OutputStream, Sink};
    use std::io::Cursor;
    use std::sync::atomic::Ordering;

    let cursor = Cursor::new(wav);
    let decoder = Decoder::new(cursor).context("Failed to decode WAV for playback")?;

    let (_stream, stream_handle) =
        OutputStream::try_default().context("No audio output device found")?;
    let sink = Sink::try_new(&stream_handle).context("Failed to create audio sink")?;

    sink.append(decoder);

    // Poll for interrupt instead of blocking until end
    while !sink.empty() {
        if interrupted.load(Ordering::Relaxed) {
            sink.stop();
            tracing::debug!("TTS playback interrupted by wake word");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piper_args_include_model_and_flags() {
        let tts = PiperOutput::new(
            PathBuf::from("/data/bin/piper"),
            PathBuf::from("/data/models/tts/en_US-lessac-medium.onnx"),
        );
        let args = tts.build_args();
        assert_eq!(args[0], "--model");
        assert!(args[1].contains("en_US-lessac-medium.onnx"));
        assert!(args.contains(&"--output-raw".to_string()));
        assert!(args.contains(&"--quiet".to_string()));
    }

    #[test]
    fn with_sample_rate_overrides_default() {
        let tts = PiperOutput::new(PathBuf::from("piper"), PathBuf::from("model.onnx"))
            .with_sample_rate(16_000);
        assert_eq!(tts.sample_rate, 16_000);
    }

    #[test]
    fn pcm_to_wav_header_is_correct() {
        // 2 bytes of PCM (one 16-bit sample at 22050 Hz mono)
        let pcm = vec![0x01u8, 0x00u8];
        let wav = pcm_to_wav(&pcm, 22_050);

        // RIFF magic
        assert_eq!(&wav[0..4], b"RIFF");
        // WAVE magic
        assert_eq!(&wav[8..12], b"WAVE");
        // fmt  chunk ID
        assert_eq!(&wav[12..16], b"fmt ");
        // data chunk ID
        assert_eq!(&wav[36..40], b"data");
        // data length
        let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_len as usize, pcm.len());
        // PCM payload
        assert_eq!(&wav[44..], pcm.as_slice());
    }

    #[test]
    fn piper_output_compiles_as_voice_output() {
        use pond_core::models::ports::voice_output::VoiceOutput;
        use std::sync::Arc;
        let _out: Arc<dyn VoiceOutput> = Arc::new(PiperOutput::new(
            PathBuf::from("piper"),
            PathBuf::from("model.onnx"),
        ));
    }
}
