//! Whisper ASR adapter for Goose In A Pond.
//!
//! Exports:
//! - `WhisperRsInput`         — in-process `VoiceInput` port (whisper-rs, default)
//! - `WhisperInput`           — legacy HTTP `VoiceInput` port (gated by `legacy-subprocess`)
//! - `WhisperKeywordDetector` — `WakeWordDetector` port: poll mic until trigger phrase heard
//! - `WhisperBackend`         — backend trait the detector uses to transcribe windows
//!
//! ## Default — in-process (`WhisperRsInput`)
//!
//! Loads a ggml `.bin` model directly via the whisper.cpp bindings. No port,
//! no subprocess, no multipart HTTP. Shares the ggml CUDA primary context with
//! `llama-cpp-2` on Jetson.
//!
//! ## Legacy — HTTP (`WhisperInput`)
//!
//! Behind `#[cfg(feature = "legacy-subprocess")]`. Records via `cpal`, encodes
//! to WAV, POSTs multipart to a local whisper.cpp server at `:9000`. Kept as a
//! one-release escape valve.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use pond_core::models::ports::voice_input::SpeculativeSignal;
use pond_core::models::ports::wake_word::{StreamingWakeWordDetector, WakeWordActivation};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[cfg(feature = "legacy-subprocess")]
use pond_core::models::ports::voice_input::VoiceInput;

mod in_process;
pub use in_process::WhisperRsInput;

/// Play a short two-tone confirmation ping (C6→E6, ~220ms).
/// Called when the wake word is detected so the user gets immediate audio feedback.
fn play_wake_ping() {
    std::thread::spawn(|| {
        use rodio::{OutputStream, Sink};
        let Ok((_stream, handle)) = OutputStream::try_default() else {
            return;
        };
        let Ok(sink) = Sink::try_new(&handle) else {
            return;
        };
        sink.set_volume(0.35);

        let rate = 44100u32;
        let tone = |freq: f32, ms: u64| -> Vec<f32> {
            let n = (rate as u64 * ms / 1000) as usize;
            (0..n)
                .map(|i| {
                    let t = i as f32 / rate as f32;
                    let env = 1.0 - (i as f32 / n as f32); // fade-out
                    (2.0 * std::f32::consts::PI * freq * t).sin() * env * 0.6
                })
                .collect()
        };
        // C6 (1047 Hz) then E6 (1319 Hz) — quick ascending chime
        let mut samples = tone(1047.0, 100);
        samples.extend(tone(1319.0, 120));
        let buf = rodio::buffer::SamplesBuffer::new(1, rate, samples);
        sink.append(buf);
        sink.sleep_until_end();
    });
}

/// Default whisper.cpp server URL.
#[cfg(feature = "legacy-subprocess")]
pub const DEFAULT_HOST: &str = "http://127.0.0.1:9000";

// ── WhisperBackend trait ──────────────────────────────────────────────────────

/// Synchronous transcription backend.
///
/// Both the in-process `WhisperRsInput` and the legacy HTTP `WhisperInput`
/// implement this trait. The `WhisperKeywordDetector` holds an
/// `Arc<dyn WhisperBackend>` and calls `transcribe_pcm_blocking` on each window
/// during the wake-word detection loop.
///
/// Called from inside `tokio::task::spawn_blocking`, so a blocking call is fine.
pub trait WhisperBackend: Send + Sync {
    /// Transcribe 16 kHz mono f32 PCM. Implementations should pass the result
    /// through `strip_whisper_artifacts`. Returns an empty string for silence /
    /// no detected speech (never panics).
    fn transcribe_pcm_blocking(&self, samples: &[f32]) -> Result<String>;
}

// ── WhisperInput (legacy HTTP) ───────────────────────────────────────────────

/// VoiceInput adapter that records from the microphone and transcribes via
/// a local whisper.cpp HTTP server.
#[cfg(feature = "legacy-subprocess")]
pub struct WhisperInput {
    client: reqwest::Client,
    transcription_url: String,
    /// Hard cap on recording time (seconds). VAD usually stops earlier.
    duration_secs: u32,
    /// How long (ms) of silence after speech to declare end-of-utterance.
    silence_ms: u64,
    /// Pre-captured WAV bytes from the wake-word detector (one-breath path).
    /// When `Some`, `listen()` transcribes these bytes instead of recording fresh.
    captured: Mutex<Option<Vec<u8>>>,
}

#[cfg(feature = "legacy-subprocess")]
impl WhisperInput {
    /// Create a new adapter pointing at `server_url` (e.g. `"http://127.0.0.1:9000"`).
    /// Defaults to `DEFAULT_HOST` when `server_url` is `None`.
    pub fn new(server_url: Option<&str>) -> Self {
        let base = server_url.unwrap_or(DEFAULT_HOST);
        Self {
            client: reqwest::Client::new(),
            transcription_url: format!("{}/inference", base),
            duration_secs: 30,
            silence_ms: 800,
            captured: Mutex::new(None),
        }
    }

    /// Override the maximum recording duration (default: 30 seconds).
    pub fn with_duration(mut self, secs: u32) -> Self {
        self.duration_secs = secs;
        self
    }

    /// Override the end-of-speech silence threshold (default: 1200 ms).
    pub fn with_silence_ms(mut self, ms: u64) -> Self {
        self.silence_ms = ms;
        self
    }
}

#[cfg(feature = "legacy-subprocess")]
#[async_trait]
impl VoiceInput for WhisperInput {
    async fn listen(&self) -> Result<Option<String>> {
        let captured = self.captured.lock().unwrap().take();
        let max_record = self.duration_secs;
        let silence_ms = self.silence_ms;

        if let Some(wav) = captured {
            // One-breath path: we have pre-captured audio from the wake listener,
            // but the user may still be speaking.  Decode what we have, continue
            // recording from the mic until silence, combine, then transcribe.
            let wav_bytes = tokio::task::spawn_blocking(move || -> Result<Option<Vec<u8>>> {
                println!("  🎤 Listening...");

                // Decode the pre-captured 16kHz WAV.
                let (captured_samples, captured_rate) = decode_wav_mono_f32(&wav)?;

                // Continue recording — no onset wait, just listen until silence.
                let (fresh_samples, fresh_rate) =
                    record_mono_f32_until_silence(max_record, silence_ms)?;

                // Resample fresh recording to match captured rate (16 kHz).
                let fresh_16k = resample_to_16k(&fresh_samples, fresh_rate);

                // Combine: captured audio first, then continuation.
                let mut combined = captured_samples;
                // Skip the leading silence from the fresh recording — the mic
                // needs ~200ms to spin up before producing real audio.
                let skip = (captured_rate as usize) / 5; // ~200ms at 16kHz
                if fresh_16k.len() > skip {
                    combined.extend_from_slice(&fresh_16k[skip..]);
                }

                if combined.is_empty() {
                    return Ok(None);
                }
                Ok(Some(encode_wav_mono_16k(&combined)))
            })
            .await??;

            match wav_bytes {
                // Transcribe and convert None (blank audio) to empty string
                // so the chat loop resets to wake word mode instead of exiting.
                Some(wav) => Ok(self.transcribe_wav(wav).await?.or(Some(String::new()))),
                None => Ok(Some(String::new())),
            }
        } else {
            // Normal path: VAD-aware recording — waits for speech, stops on silence.
            let wav_bytes = tokio::task::spawn_blocking(move || -> Result<Option<Vec<u8>>> {
                println!("  🎤 Listening...");
                let (samples, sample_rate, _speculative) = record_mono_f32_vad(
                    10,         // max 10s waiting for speech to start
                    max_record, // hard cap on total recording
                    silence_ms, // end-of-speech silence threshold
                    None,       // legacy HTTP path: no in-process whisper context to overlap with
                    None,
                )?;
                if samples.is_empty() {
                    return Ok(None);
                }
                let samples_16k = resample_to_16k(&samples, sample_rate);
                Ok(Some(encode_wav_mono_16k(&samples_16k)))
            })
            .await??;

            match wav_bytes {
                Some(wav) => Ok(self.transcribe_wav(wav).await?.or(Some(String::new()))),
                None => Ok(Some(String::new())),
            }
        }
    }

    fn prompt(&self) -> &str {
        "🎤 "
    }

    /// Store WAV bytes for `listen()` to transcribe instead of recording fresh.
    fn prime_with_captured(&self, wav: Vec<u8>) {
        *self.captured.lock().unwrap() = Some(wav);
    }
}

#[cfg(feature = "legacy-subprocess")]
impl WhisperInput {
    /// POST pre-encoded WAV bytes to the whisper server and return the transcript.
    ///
    /// Exposed publicly so callers can transcribe audio from any source (e.g.
    /// a file on disk) without going through microphone capture.
    pub async fn transcribe_wav(&self, wav_bytes: Vec<u8>) -> Result<Option<String>> {
        let part = reqwest::multipart::Part::bytes(wav_bytes)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e| anyhow!("MIME error: {}", e))?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("response_format", "json");

        let resp = self
            .client
            .post(&self.transcription_url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| anyhow!("whisper server request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("whisper server error {}: {}", status, body));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow!("whisper response parse error: {}", e))?;

        let raw = json["text"].as_str().unwrap_or("").trim().to_string();

        // Strip Whisper artifacts — bracketed tags like [BLANK_AUDIO], [MUSIC],
        // [NOISE], [LAUGHTER], etc.  These are not speech.
        let text = strip_whisper_artifacts(&raw);

        Ok(if text.is_empty() { None } else { Some(text) })
    }
}

#[cfg(feature = "legacy-subprocess")]
impl WhisperBackend for WhisperInput {
    fn transcribe_pcm_blocking(&self, samples: &[f32]) -> Result<String> {
        let wav = encode_wav_mono_16k(samples);
        let server_url = self
            .transcription_url
            .strip_suffix("/inference")
            .unwrap_or(&self.transcription_url);
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(transcribe_blocking(&client, server_url, wav)?.unwrap_or_default())
    }
}

/// Remove Whisper non-speech tags (`[BLANK_AUDIO]`, `[MUSIC]`, `[NOISE]`, …)
/// and return the remaining text trimmed.  If nothing real remains, returns "".
pub(crate) fn strip_whisper_artifacts(text: &str) -> String {
    // Strip all [BRACKETED_TAGS] — Whisper uses these for non-speech events.
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        if let Some(close) = rest[open..].find(']') {
            rest = &rest[open + close + 1..];
        } else {
            rest = &rest[open..];
            break;
        }
    }
    out.push_str(rest);

    // Strip (PARENTHESIZED TAGS) — e.g. (inaudible), (music), (laughing)
    let mut cleaned = String::with_capacity(out.len());
    let mut prest = out.as_str();
    while let Some(open) = prest.find('(') {
        cleaned.push_str(&prest[..open]);
        if let Some(close) = prest[open..].find(')') {
            prest = &prest[open + close + 1..];
        } else {
            prest = &prest[open..];
            break;
        }
    }
    cleaned.push_str(prest);

    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return String::new();
    }

    // Reject common Whisper hallucinations on silence / noise.
    let lower = cleaned.to_lowercase();
    const EXACT_HALLUCINATIONS: &[&str] = &[
        ".",
        "..",
        "...",
        ",",
        "!",
        "?",
        "thank you",
        "thanks for watching",
        "thanks for listening",
        "thanks",
        "you",
        "bye",
        "bye bye",
        "okay",
        "the end",
        "subtitles by",
        "subtitle",
        "so",
        "um",
        "uh",
        "hmm",
        "huh",
        "ah",
        "oh",
        "i'm sorry",
        "i don't know",
        "please subscribe",
        "like and subscribe",
    ];
    if EXACT_HALLUCINATIONS.iter().any(|h| lower == *h) {
        return String::new();
    }

    // Reject very short transcripts (1-2 chars) — almost always noise artifacts.
    if cleaned.len() <= 2 {
        return String::new();
    }

    // Reject if the transcript is just the same word/syllable repeated.
    let words: Vec<&str> = lower.split_whitespace().collect();
    if words.len() >= 2 && words.iter().all(|w| *w == words[0]) {
        return String::new();
    }

    cleaned.to_string()
}

// ── WAV decoding ─────────────────────────────────────────────────────────────

/// Decode a 16-bit mono PCM WAV (as produced by `encode_wav_mono_16k`) back to
/// f32 samples.  Returns `(samples, sample_rate)`.
pub(crate) fn decode_wav_mono_f32(wav: &[u8]) -> Result<(Vec<f32>, u32)> {
    if wav.len() < 44 {
        return Err(anyhow!("WAV too short ({} bytes)", wav.len()));
    }
    // Read sample rate from the fmt chunk (bytes 24..28).
    let sample_rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
    // Data payload starts at byte 44.
    let pcm = &wav[44..];
    let samples: Vec<f32> = pcm
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32_767.0)
        .collect();
    Ok((samples, sample_rate))
}

// ── Audio capture ─────────────────────────────────────────────────────────────

/// Record from the microphone until the speaker stops talking.
///
/// Unlike `record_mono_f32_vad`, this skips the "wait for speech onset" phase
/// — it assumes the speaker is already talking (or about to be).  Used after
/// wake word detection to continue capturing the user's command.
///
/// Stops when `silence_ms` consecutive milliseconds of silence are detected,
/// or after `max_record_secs` total recording time.
pub(crate) fn record_mono_f32_until_silence(
    max_record_secs: u32,
    silence_ms: u64,
) -> Result<(Vec<f32>, u32)> {
    const SILENCE_RMS: f32 = 0.005;
    const POLL_MS: u64 = 30;

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("No audio input device found"))?;

    let config = device
        .default_input_config()
        .map_err(|e| anyhow!("Failed to get input config: {}", e))?;

    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let samples_writer = Arc::clone(&samples);

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mono: Vec<f32> = data
                    .chunks(channels)
                    .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                    .collect();
                samples_writer.lock().unwrap().extend_from_slice(&mono);
            },
            |e| eprintln!("  ⚠ Audio stream error: {}", e),
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let mono: Vec<f32> = data
                    .chunks(channels)
                    .map(|frame| {
                        let sum: f32 = frame.iter().map(|&s| s as f32 / i16::MAX as f32).sum();
                        sum / channels as f32
                    })
                    .collect();
                samples_writer.lock().unwrap().extend_from_slice(&mono);
            },
            |e| eprintln!("  ⚠ Audio stream error: {}", e),
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.into(),
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                let mono: Vec<f32> = data
                    .chunks(channels)
                    .map(|frame| {
                        let sum: f32 = frame
                            .iter()
                            .map(|&s| s as f32 / u16::MAX as f32 * 2.0 - 1.0)
                            .sum();
                        sum / channels as f32
                    })
                    .collect();
                samples_writer.lock().unwrap().extend_from_slice(&mono);
            },
            |e| eprintln!("  ⚠ Audio stream error: {}", e),
            None,
        )?,
        fmt => return Err(anyhow!("Unsupported audio sample format: {:?}", fmt)),
    };

    stream
        .play()
        .map_err(|e| anyhow!("Failed to start audio stream: {}", e))?;

    // Record until silence or hard cap — no onset wait.
    let max_ms = max_record_secs as u64 * 1000;
    let mut elapsed_ms: u64 = 0;
    let mut silent_for: u64 = 0;

    while elapsed_ms < max_ms {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        elapsed_ms += POLL_MS;

        let rms = {
            let buf = samples.lock().unwrap();
            let recent = (sample_rate as u64 * POLL_MS / 1000) as usize;
            let start = buf.len().saturating_sub(recent);
            rms_energy(&buf[start..])
        };

        if rms < SILENCE_RMS {
            silent_for += POLL_MS;
            if silent_for >= silence_ms {
                tracing::debug!(
                    "Continue-record: end-of-speech after {}ms silence ({}ms total)",
                    silent_for,
                    elapsed_ms
                );
                break;
            }
        } else {
            silent_for = 0;
        }
    }

    drop(stream);
    let recorded = match Arc::try_unwrap(samples) {
        Ok(mutex) => mutex.into_inner().unwrap(),
        Err(arc) => arc.lock().unwrap().clone(),
    };

    Ok((recorded, sample_rate))
}

/// Decides when to fire (and discard) a speculative transcription job during
/// the end-of-speech silence wait, decoupled from cpal/mic I/O so the
/// decision logic itself can be unit tested with synthetic RMS sequences.
///
/// `record_mono_f32_vad` normally waits for `silence_ms` of confirmed
/// silence before doing anything with the recording. That confirmation
/// window is dead time — the audio is already final the moment silence
/// *starts*, in the common case where the user doesn't resume speaking.
/// This lets the caller start whisper inference on the first silent poll,
/// overlapping it with the rest of the confirmation wait, instead of
/// starting inference only after confirmation completes.
#[derive(Debug, PartialEq, Eq)]
enum VadEvent {
    /// No state transition — caller does nothing.
    None,
    /// First silent poll after speech: caller should spawn a speculative
    /// transcription of the audio captured so far.
    SpawnSpeculative,
    /// Speech resumed before silence was confirmed: caller should discard
    /// any in-flight speculative job — it covers a too-short clip.
    DiscardSpeculative,
    /// `silence_ms` of silence confirmed: caller should stop recording.
    /// Whatever speculative job is currently in flight (if any) was spawned
    /// from this exact silence run and is safe to use as the final result.
    Confirmed,
}

struct SpeculativeVad {
    silent_for_ms: u64,
    silence_ms: u64,
    poll_ms: u64,
}

impl SpeculativeVad {
    fn new(silence_ms: u64, poll_ms: u64) -> Self {
        Self {
            silent_for_ms: 0,
            silence_ms,
            poll_ms,
        }
    }

    /// Feed one poll's RMS reading. Call once per `poll_ms` tick during
    /// Phase 2 (after speech onset has been confirmed).
    fn on_rms(&mut self, rms: f32, silence_threshold: f32) -> VadEvent {
        if rms < silence_threshold {
            let was_speaking = self.silent_for_ms == 0;
            self.silent_for_ms += self.poll_ms;
            if self.silent_for_ms >= self.silence_ms {
                VadEvent::Confirmed
            } else if was_speaking {
                VadEvent::SpawnSpeculative
            } else {
                VadEvent::None
            }
        } else {
            let was_silent = self.silent_for_ms != 0;
            self.silent_for_ms = 0;
            if was_silent {
                VadEvent::DiscardSpeculative
            } else {
                VadEvent::None
            }
        }
    }
}

/// Spawns a background transcription of `samples` at `sample_rate`, returning
/// a handle the caller can join once end-of-speech is confirmed.
pub(crate) type SpeculativeSpawn =
    dyn Fn(Vec<f32>, u32) -> std::thread::JoinHandle<Result<String>> + Send + Sync;

/// VAD-aware audio recording from the default input device.
///
/// Instead of recording a fixed duration, this uses voice activity detection:
///   1. Waits up to `max_wait_secs` for the user to start speaking
///   2. Once speech is detected (RMS > onset threshold), records everything
///   3. Stops when the user pauses for `silence_ms` consecutive milliseconds
///   4. Hard cap at `max_record_secs` total recording time
///
/// `speculative_spawn`, if given, is called once per silence run (debounced —
/// not on every poll) with the audio captured so far, overlapping whisper
/// inference with the rest of the silence-confirmation wait. If the run that
/// triggers confirmation has a matching speculative job, its result is
/// returned as the third tuple element so the caller can skip a second,
/// redundant full-utterance inference call.
///
/// `on_speculative_event`, if given, is notified as soon as the speculative
/// job completes — `Ready(transcript)` — even before silence is confirmed
/// (Q2-26), so the caller can start downstream work (e.g. the LLM call)
/// early. If speech resumes after a `Ready` notification, `Invalidated` is
/// sent so the caller can cancel that work.
///
/// Returns mono f32 PCM samples and the device's sample rate.
/// Returns `Ok((empty, rate, None))` if no speech was detected within the wait period.
pub(crate) fn record_mono_f32_vad(
    max_wait_secs: u32,
    max_record_secs: u32,
    silence_ms: u64,
    speculative_spawn: Option<&SpeculativeSpawn>,
    on_speculative_event: Option<&(dyn Fn(SpeculativeSignal) + Send + Sync)>,
) -> Result<(Vec<f32>, u32, Option<String>)> {
    const SPEECH_RMS: f32 = 0.010; // onset threshold — lowered for better sensitivity
    const SILENCE_RMS: f32 = 0.005; // end-of-speech threshold (hysteresis)
    const POLL_MS: u64 = 30;

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("No audio input device found"))?;

    let config = device
        .default_input_config()
        .map_err(|e| anyhow!("Failed to get input config: {}", e))?;

    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let samples_writer = Arc::clone(&samples);

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mono: Vec<f32> = data
                    .chunks(channels)
                    .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                    .collect();
                samples_writer.lock().unwrap().extend_from_slice(&mono);
            },
            |e| eprintln!("  ⚠ Audio stream error: {}", e),
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let mono: Vec<f32> = data
                    .chunks(channels)
                    .map(|frame| {
                        let sum: f32 = frame.iter().map(|&s| s as f32 / i16::MAX as f32).sum();
                        sum / channels as f32
                    })
                    .collect();
                samples_writer.lock().unwrap().extend_from_slice(&mono);
            },
            |e| eprintln!("  ⚠ Audio stream error: {}", e),
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config.into(),
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                let mono: Vec<f32> = data
                    .chunks(channels)
                    .map(|frame| {
                        let sum: f32 = frame
                            .iter()
                            .map(|&s| s as f32 / u16::MAX as f32 * 2.0 - 1.0)
                            .sum();
                        sum / channels as f32
                    })
                    .collect();
                samples_writer.lock().unwrap().extend_from_slice(&mono);
            },
            |e| eprintln!("  ⚠ Audio stream error: {}", e),
            None,
        )?,
        fmt => return Err(anyhow!("Unsupported audio sample format: {:?}", fmt)),
    };

    stream
        .play()
        .map_err(|e| anyhow!("Failed to start audio stream: {}", e))?;

    // ── Phase 1: wait for speech onset ──────────────────────────────────────
    let max_wait_ms = max_wait_secs as u64 * 1000;
    let mut waited_ms: u64 = 0;
    let mut speech_detected = false;

    while waited_ms < max_wait_ms {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        waited_ms += POLL_MS;

        let rms = {
            let buf = samples.lock().unwrap();
            let recent = (sample_rate as u64 * POLL_MS / 1000) as usize;
            let start = buf.len().saturating_sub(recent);
            rms_energy(&buf[start..])
        };

        if rms >= SPEECH_RMS {
            speech_detected = true;
            break;
        }
    }

    if !speech_detected {
        drop(stream);
        let recorded = match Arc::try_unwrap(samples) {
            Ok(mutex) => mutex.into_inner().unwrap(),
            Err(arc) => arc.lock().unwrap().clone(),
        };
        return Ok((recorded, sample_rate, None)); // empty or just noise
    }

    // ── Phase 2: record until end-of-speech ─────────────────────────────────
    let max_record_ms = max_record_secs as u64 * 1000;
    let mut recorded_ms: u64 = 0;
    let mut vad = SpeculativeVad::new(silence_ms, POLL_MS);
    let mut speculative: Option<std::thread::JoinHandle<Result<String>>> = None;
    // Set once the in-flight speculative job has been joined and the caller
    // notified via `Ready` — retained so a later `Confirmed` can reuse it
    // without re-joining, and so a later `DiscardSpeculative` knows to fire
    // `Invalidated` (only needed if the caller already heard `Ready`).
    let mut speculative_ready: Option<String> = None;
    let mut confirmed = false;

    while recorded_ms < max_record_ms {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        recorded_ms += POLL_MS;

        let rms = {
            let buf = samples.lock().unwrap();
            let recent = (sample_rate as u64 * POLL_MS / 1000) as usize;
            let start = buf.len().saturating_sub(recent);
            rms_energy(&buf[start..])
        };

        match vad.on_rms(rms, SILENCE_RMS) {
            VadEvent::SpawnSpeculative => {
                if let Some(spawn) = speculative_spawn {
                    let snapshot = samples.lock().unwrap().clone();
                    speculative = Some(spawn(snapshot, sample_rate));
                    speculative_ready = None;
                }
            }
            VadEvent::DiscardSpeculative => {
                if speculative_ready.is_some() {
                    if let Some(cb) = on_speculative_event {
                        cb(SpeculativeSignal::Invalidated);
                    }
                }
                speculative = None; // abandon the in-flight job, it covered a too-short clip
                speculative_ready = None;
            }
            VadEvent::Confirmed => {
                tracing::debug!("VAD: end-of-speech confirmed ({}ms total)", recorded_ms);
                confirmed = true;
                break;
            }
            VadEvent::None => {}
        }

        // Poll the speculative job (non-blocking) and notify the caller the
        // instant it's ready — this is what lets the LLM start before
        // silence is confirmed, not just before the redundant re-transcribe.
        if speculative_ready.is_none() {
            if let Some(handle) = &speculative {
                if handle.is_finished() {
                    let handle = speculative.take().unwrap();
                    if let Ok(Ok(transcript)) = handle.join() {
                        speculative_ready = Some(transcript.clone());
                        if let Some(cb) = on_speculative_event {
                            cb(SpeculativeSignal::Ready(transcript));
                        }
                    }
                }
            }
        }
    }

    drop(stream);
    let recorded = match Arc::try_unwrap(samples) {
        Ok(mutex) => mutex.into_inner().unwrap(),
        Err(arc) => arc.lock().unwrap().clone(),
    };

    let speculative_transcript = if confirmed {
        speculative_ready.or_else(|| speculative.and_then(|h| h.join().ok().and_then(|r| r.ok())))
    } else {
        None
    };

    Ok((recorded, sample_rate, speculative_transcript))
}

// ── DSP helpers ───────────────────────────────────────────────────────────────

/// Linear interpolation resample to 16 000 Hz (whisper's expected rate).
pub(crate) fn resample_to_16k(samples: &[f32], src_rate: u32) -> Vec<f32> {
    if src_rate == 16_000 {
        return samples.to_vec();
    }
    let ratio = 16_000.0_f64 / src_rate as f64;
    let new_len = (samples.len() as f64 * ratio) as usize;
    (0..new_len)
        .map(|i| {
            let src = i as f64 / ratio;
            let lo = src.floor() as usize;
            let hi = (lo + 1).min(samples.len().saturating_sub(1));
            let frac = (src - src.floor()) as f32;
            samples[lo] * (1.0 - frac) + samples[hi] * frac
        })
        .collect()
}

// ── WAV encoding ──────────────────────────────────────────────────────────────

/// Encode mono 16-bit PCM at 16 kHz as a WAV byte vector.
/// Avoids any external WAV crate dependency.
pub(crate) fn encode_wav_mono_16k(samples: &[f32]) -> Vec<u8> {
    let sample_rate: u32 = 16_000;
    let channels: u16 = 1;
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align = channels * bits_per_sample / 8;
    let data_len = (samples.len() * 2) as u32; // 2 bytes per i16 sample

    let mut buf = Vec::with_capacity(44 + data_len as usize);

    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());

    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());

    for &s in samples {
        let sample = (s.clamp(-1.0, 1.0) * 32_767.0) as i16;
        buf.extend_from_slice(&sample.to_le_bytes());
    }

    buf
}

// ── WhisperKeywordDetector ────────────────────────────────────────────────────

/// Configuration for the sliding-window wake-word detector.
#[derive(Clone)]
pub struct KeywordDetectorConfig {
    /// Width of the audio window fed to whisper on each cycle (milliseconds).
    /// Default: 2500 ms — wide enough for multi-word wake phrases and natural speech.
    pub window_ms: u64,
    /// How far to advance the window on each detection cycle (milliseconds).
    /// Default: 400 ms — ~6 overlapping checks per window, responsive detection.
    pub slide_ms: u64,
    /// Maximum audio to capture after detection fires (milliseconds).
    /// Acts as a hard ceiling — VAD silence detection exits earlier when enabled.
    /// Default: 5000 ms.
    pub post_trigger_ms: u64,
    /// Enable two-threshold hysteresis.
    /// When a ≤3-token transcript contains the trigger, re-check once with a
    /// shorter slide before firing — reduces false positives from noise bursts.
    /// Default: true.
    pub hysteresis_enabled: bool,
    /// Slide advance used during the hysteresis re-check (milliseconds).
    /// Default: 200 ms.
    pub hysteresis_slide_ms: u64,
    /// Minimum RMS energy required to send a window to whisper.
    /// Windows below this level are skipped entirely — eliminates ~90% of
    /// whisper calls during silence and prevents ambient-noise false positives.
    /// Default: 0.01 (~−40 dBFS). Set to 0.0 to disable the gate.
    pub energy_threshold: f32,
    /// How long (ms) of consecutive silence terminates the post-trigger capture.
    /// The ring buffer is snapshotted as soon as this silence duration elapses,
    /// instead of always waiting the full `post_trigger_ms`.
    /// Default: 600 ms. Set to 0 to disable (always wait full `post_trigger_ms`).
    pub post_trigger_silence_ms: u64,
    /// How long (ms) to sleep before re-arming detection after each activation.
    /// Prevents re-triggering on TTS echo or residual room noise.
    /// Default: 2000 ms. Set to 0 to disable.
    pub cooldown_ms: u64,
}

impl Default for KeywordDetectorConfig {
    fn default() -> Self {
        Self {
            window_ms: 2500,
            slide_ms: 300,
            post_trigger_ms: 5000,
            hysteresis_enabled: true,
            hysteresis_slide_ms: 150,
            energy_threshold: 0.003,
            post_trigger_silence_ms: 600,
            cooldown_ms: 2000,
        }
    }
}

/// WakeWordDetector that uses a continuous audio ring buffer and a sliding
/// detection window to reduce latency and support the one-breath command flow.
///
/// Siri-inspired improvements over the old sequential 2 s-clip poller:
/// - **Rolling ring buffer** — `cpal` streams continuously; no gaps between clips.
/// - **Overlapping windows** — default 1500 ms window slides every 500 ms,
///   giving ~1–1.5 s detection latency vs ~2.5 s before.
/// - **Two-threshold hysteresis** — a ≤3-token match triggers a 200 ms re-check
///   before firing, reducing false positives without meaningful latency cost.
/// - **One-breath audio hand-off** — on confirmed detection, 4 s of trailing
///   audio is captured and returned so `VoiceInput::listen()` can transcribe
///   the command without a separate recording window.
///
/// Implements `StreamingWakeWordDetector`; the blanket impl provides
/// `WakeWordDetector` automatically.
pub struct WhisperKeywordDetector {
    /// Transcription backend — `WhisperRsInput` (in-process) by default,
    /// `WhisperInput` (HTTP) when `legacy-subprocess` is the backend wired in.
    backend: Arc<dyn WhisperBackend>,
    /// All normalized trigger variants. A transcript matching *any* of these fires detection.
    triggers: Vec<String>,
    prompt: String,
    config: KeywordDetectorConfig,
}

impl WhisperKeywordDetector {
    /// Create a detector that calls `backend` to transcribe each window.
    /// `trigger` is the wake phrase (e.g. `"goose"`).
    pub fn new(backend: Arc<dyn WhisperBackend>, trigger: impl Into<String>) -> Self {
        let raw = trigger.into();
        let prompt = format!("Say \"{}\" to activate...", raw);
        Self {
            backend,
            triggers: vec![normalize_transcript(&raw)],
            prompt,
            config: KeywordDetectorConfig::default(),
        }
    }

    /// Load calibrated transcription variants collected during onboarding.
    ///
    /// When `variants` is non-empty, the detector matches against any of them
    /// (OR logic), making detection robust to Whisper's inconsistent output
    /// (e.g. "hey goose" / "hey, goose" / "a goose").
    ///
    /// When `variants` is empty the detector keeps the single normalized trigger
    /// set by `new()`, plus built-in fuzzy variants for common wake words.
    pub fn with_transcriptions(mut self, variants: Vec<String>) -> Self {
        if !variants.is_empty() {
            self.triggers = variants
                .iter()
                .map(|v| normalize_transcript(v))
                .filter(|v| !v.is_empty())
                .collect();
            // Fallback: if all variants normalized to empty, keep the existing trigger.
            if self.triggers.is_empty() {
                self.triggers = vec![normalize_transcript(&self.prompt)];
            }
        }
        // Always add built-in fuzzy variants for the primary trigger.
        // Whisper frequently misheard common wake words.
        let primary = self.triggers.first().cloned().unwrap_or_default();
        let builtins = builtin_fuzzy_variants(&primary);
        for variant in builtins {
            if !self.triggers.contains(&variant) {
                self.triggers.push(variant);
            }
        }
        self
    }

    /// Override detection parameters.
    pub fn with_config(mut self, config: KeywordDetectorConfig) -> Self {
        self.config = config;
        self
    }
}

/// Built-in fuzzy variants for common wake words.
///
/// Whisper (especially tiny/base models) frequently mishears short words.
/// These variants catch the most common transcription errors without
/// requiring user calibration.
fn builtin_fuzzy_variants(primary_trigger: &str) -> Vec<String> {
    match primary_trigger {
        "goose" => vec![
            "goose".to_string(),
            "goos".to_string(),
            "gooes".to_string(),
            "gus".to_string(),
            "gooch".to_string(),
            "hey goose".to_string(),
            "a goose".to_string(),
            "the goose".to_string(),
        ],
        "hey goose" => vec![
            "hey goose".to_string(),
            "hey goos".to_string(),
            "hey gus".to_string(),
            "a goose".to_string(),
            "hey gooch".to_string(),
        ],
        _ => vec![],
    }
}

/// Strip punctuation and normalise whitespace for wake-word comparison.
///
/// Whisper adds commas and periods to transcripts (e.g. "Hey, goose.") which
/// breaks a naive `contains()` against "hey goose".  Keeping only alphabetic
/// chars and collapsing whitespace makes the match robust.
fn normalize_transcript(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphabetic() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Count whitespace-separated tokens in a normalized transcript.
fn token_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Root-mean-square energy of a mono f32 sample slice.
/// Returns 0.0 for an empty slice.
fn rms_energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[async_trait]
impl StreamingWakeWordDetector for WhisperKeywordDetector {
    async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation> {
        let backend = self.backend.clone();
        let triggers = self.triggers.clone();
        let config = self.config.clone();

        tokio::task::spawn_blocking(move || detection_loop(backend, triggers, config))
            .await
            .map_err(|e| anyhow!("detection thread panicked: {}", e))?
    }

    fn activation_prompt(&self) -> &str {
        &self.prompt
    }
}

/// Blocking detection loop — runs inside `tokio::task::spawn_blocking`.
///
/// Opens a continuous cpal input stream into a ring buffer, then slides a
/// detection window over it, sending each window to whisper.cpp for
/// transcription.  Returns when the trigger phrase is confirmed.
fn detection_loop(
    backend: Arc<dyn WhisperBackend>,
    triggers: Vec<String>,
    config: KeywordDetectorConfig,
) -> Result<WakeWordActivation> {
    // ── Open continuous audio stream ─────────────────────────────────────────
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("No audio input device found"))?;
    let stream_config = device
        .default_input_config()
        .map_err(|e| anyhow!("Failed to get input config: {}", e))?;

    let sample_rate = stream_config.sample_rate().0;
    let channels = stream_config.channels() as usize;

    // Ring buffer holds (window_ms + post_trigger_ms) worth of samples.
    let max_samples =
        ((config.window_ms + config.post_trigger_ms) * sample_rate as u64 / 1000) as usize;
    let ring: Arc<Mutex<VecDeque<f32>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(max_samples)));
    let ring_writer = ring.clone();

    let stream = match stream_config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config.into(),
            move |data: &[f32], _| {
                let mut r = ring_writer.lock().unwrap();
                for chunk in data.chunks(channels) {
                    let mono = chunk.iter().copied().sum::<f32>() / channels as f32;
                    r.push_back(mono);
                }
                while r.len() > max_samples {
                    r.pop_front();
                }
            },
            |e| tracing::warn!("audio stream error: {}", e),
            None,
        )?,
        cpal::SampleFormat::I16 => {
            let ring_writer2 = ring.clone();
            device.build_input_stream(
                &stream_config.into(),
                move |data: &[i16], _| {
                    let mut r = ring_writer2.lock().unwrap();
                    for chunk in data.chunks(channels) {
                        let sum: f32 = chunk.iter().map(|&s| s as f32 / i16::MAX as f32).sum();
                        r.push_back(sum / channels as f32);
                    }
                    while r.len() > max_samples {
                        r.pop_front();
                    }
                },
                |e| tracing::warn!("audio stream error: {}", e),
                None,
            )?
        }
        fmt => return Err(anyhow!("Unsupported audio format: {:?}", fmt)),
    };
    stream
        .play()
        .map_err(|e| anyhow!("Failed to start audio stream: {}", e))?;

    // ── Cooldown — wait before re-arming (prevents TTS echo re-trigger) ───────
    if config.cooldown_ms > 0 {
        tracing::debug!("KWS: cooldown {}ms before arming", config.cooldown_ms);
        std::thread::sleep(std::time::Duration::from_millis(config.cooldown_ms));
    }

    // ── Detection loop ────────────────────────────────────────────────────────
    let window_samples = (config.window_ms * sample_rate as u64 / 1000) as usize;
    let mut slide_ms = config.slide_ms;
    let mut hysteresis = false;

    loop {
        std::thread::sleep(std::time::Duration::from_millis(slide_ms));

        // Snapshot the latest window_ms samples from the ring buffer.
        let snapshot: Vec<f32> = {
            let r = ring.lock().unwrap();
            let start = r.len().saturating_sub(window_samples);
            r.range(start..).copied().collect()
        };

        if snapshot.len() < window_samples / 2 {
            continue; // buffer not yet full enough — keep waiting
        }

        // ── Energy gate — skip silent windows before hitting whisper ──────────
        if config.energy_threshold > 0.0 {
            let rms = rms_energy(&snapshot);
            if rms < config.energy_threshold {
                tracing::trace!("KWS: silent window skipped (rms={:.4})", rms);
                continue;
            }
        }

        // Transcribe the window via the backend (in-process or HTTP).
        let resampled = resample_to_16k(&snapshot, sample_rate);

        let transcript = match backend.transcribe_pcm_blocking(&resampled) {
            Ok(t) if !t.is_empty() => {
                // Backend implementations already strip artifacts, but call
                // it again so a stray bracketed tag never makes it into the
                // trigger-matching path.
                let cleaned = strip_whisper_artifacts(&t);
                if cleaned.is_empty() {
                    tracing::debug!("KWS: artifact-only transcript stripped: {:?}", t);
                    continue;
                }
                normalize_transcript(&cleaned)
            }
            Ok(_) => {
                tracing::debug!("No speech in window");
                continue;
            }
            Err(e) => {
                tracing::warn!("Whisper error (retrying): {}", e);
                continue;
            }
        };

        let matched = triggers.iter().any(|t| transcript.contains(t.as_str()));
        tracing::debug!(
            "KWS window: \"{}\" (triggers: {:?}, matched: {})",
            transcript,
            triggers,
            matched
        );

        if matched {
            // ── Hysteresis check ──────────────────────────────────────────────
            if config.hysteresis_enabled && !hysteresis && token_count(&transcript) <= 3 {
                // Short noisy transcript — could be a false positive. Re-check once.
                tracing::debug!("Hysteresis: entering re-check mode");
                hysteresis = true;
                slide_ms = config.hysteresis_slide_ms;
                continue;
            }

            // ── Confirmed — capture trailing command audio ────────────────────
            tracing::info!(
                "Wake word confirmed: \"{}\" (matched triggers: {:?})",
                transcript,
                triggers
            );
            println!("  🟢 Wake word detected!");
            play_wake_ping();

            // VAD-gated post-trigger: poll every 50 ms and exit as soon as the
            // microphone goes silent for `post_trigger_silence_ms` consecutive ms.
            // Falls back to waiting the full `post_trigger_ms` if VAD is disabled
            // or the user keeps speaking past the ceiling.
            let poll_ms = 50u64;
            let mut elapsed_ms = 0u64;
            let mut silent_for_ms = 0u64;

            while elapsed_ms < config.post_trigger_ms {
                std::thread::sleep(std::time::Duration::from_millis(poll_ms));
                elapsed_ms += poll_ms;

                if config.post_trigger_silence_ms > 0 && config.energy_threshold > 0.0 {
                    let recent_rms = {
                        let r = ring.lock().unwrap();
                        let recent_samples = (sample_rate as u64 * poll_ms / 1000) as usize;
                        let start = r.len().saturating_sub(recent_samples);
                        let chunk: Vec<f32> = r.range(start..).copied().collect();
                        rms_energy(&chunk)
                    };
                    if recent_rms < config.energy_threshold {
                        silent_for_ms += poll_ms;
                        if silent_for_ms >= config.post_trigger_silence_ms {
                            tracing::debug!(
                                "KWS: VAD silence after {}ms — snapping command audio early",
                                elapsed_ms
                            );
                            break;
                        }
                    } else {
                        silent_for_ms = 0; // voice still present — reset counter
                    }
                }
            }

            let post_samples = (elapsed_ms * sample_rate as u64 / 1000) as usize;
            let command_audio: Vec<f32> = {
                let r = ring.lock().unwrap();
                let start = r.len().saturating_sub(post_samples);
                r.range(start..).copied().collect()
            };

            drop(stream); // stop recording

            let cmd_resampled = resample_to_16k(&command_audio, sample_rate);
            let cmd_wav = encode_wav_mono_16k(&cmd_resampled);

            return Ok(WakeWordActivation {
                captured_audio: Some(cmd_wav),
            });
        }

        // No trigger found.
        if hysteresis {
            tracing::debug!("Hysteresis: re-check clean, returning to normal slide");
            hysteresis = false;
            slide_ms = config.slide_ms;
        }
    }
}

/// POST WAV bytes to whisper.cpp using a blocking HTTP client.
#[cfg(feature = "legacy-subprocess")]
fn transcribe_blocking(
    client: &reqwest::blocking::Client,
    server_url: &str,
    wav: Vec<u8>,
) -> Result<Option<String>> {
    let part = reqwest::blocking::multipart::Part::bytes(wav)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| anyhow!("MIME: {}", e))?;
    let form = reqwest::blocking::multipart::Form::new()
        .part("file", part)
        .text("response_format", "json");

    let resp = client
        .post(format!("{}/inference", server_url))
        .multipart(form)
        .send()
        .map_err(|e| anyhow!("whisper request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(anyhow!("whisper error {}", resp.status()));
    }

    let json: serde_json::Value = resp.json().map_err(|e| anyhow!("parse error: {}", e))?;
    let raw = json["text"].as_str().unwrap_or("").trim().to_string();
    let text = strip_whisper_artifacts(&raw);
    Ok(if text.is_empty() { None } else { Some(text) })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "legacy-subprocess")]
    #[test]
    fn whisper_input_default_prompt() {
        use pond_core::models::ports::voice_input::VoiceInput;
        assert_eq!(WhisperInput::new(None).prompt(), "🎤 ");
    }

    #[cfg(feature = "legacy-subprocess")]
    #[test]
    fn whisper_input_custom_url() {
        let w = WhisperInput::new(Some("http://192.168.1.100:9000"));
        assert!(w.transcription_url.starts_with("http://192.168.1.100:9000"));
    }

    #[test]
    fn encode_wav_has_riff_header() {
        let samples = vec![0.0f32; 160]; // 10ms of silence
        let wav = encode_wav_mono_16k(&samples);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
    }

    #[test]
    fn encode_wav_correct_data_length() {
        let n = 100usize;
        let samples = vec![0.5f32; n];
        let wav = encode_wav_mono_16k(&samples);
        // 44-byte header + n * 2 bytes of PCM data
        assert_eq!(wav.len(), 44 + n * 2);
    }

    #[test]
    fn resample_passthrough_at_16k() {
        let samples = vec![1.0f32, 0.5, 0.0];
        let out = resample_to_16k(&samples, 16_000);
        assert_eq!(out, samples);
    }

    #[test]
    fn resample_halves_length_at_32k() {
        let samples: Vec<f32> = (0..64).map(|i| i as f32 / 63.0).collect();
        let out = resample_to_16k(&samples, 32_000);
        // 64 samples @ 32kHz → ~32 samples @ 16kHz
        assert!((out.len() as i32 - 32).abs() <= 1, "len was {}", out.len());
    }

    #[cfg(feature = "legacy-subprocess")]
    #[test]
    fn duration_builder() {
        let w = WhisperInput::new(None).with_duration(10);
        assert_eq!(w.duration_secs, 10);
    }

    /// Verify the full PCM pipeline against a real audio file.
    ///
    /// jfk.wav is a 16-bit mono 16 kHz PCM WAV (the canonical whisper.cpp sample).
    /// We parse its samples, run them through our DSP helpers, and re-encode — then
    /// verify the resulting WAV is structurally valid and the right length.
    #[test]
    fn jfk_wav_round_trips_through_dsp_pipeline() {
        let wav_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/blobs/jfk.wav");
        let wav_bytes = std::fs::read(&wav_path)
            .expect("tests/blobs/jfk.wav not found — run from workspace root");

        assert!(wav_bytes.len() > 44, "WAV file too short");

        // Locate "data" chunk (handles any non-standard pre-data chunks)
        let data_offset = wav_bytes
            .windows(4)
            .position(|w| w == b"data")
            .expect("no 'data' chunk in jfk.wav")
            + 8; // skip "data" tag (4) + chunk-size field (4)

        let pcm_bytes = &wav_bytes[data_offset..];
        let samples: Vec<f32> = pcm_bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32_767.0)
            .collect();

        assert!(!samples.is_empty(), "jfk.wav has no PCM samples");

        // jfk.wav is already 16 kHz → resample is a passthrough
        let resampled = resample_to_16k(&samples, 16_000);
        assert_eq!(
            resampled.len(),
            samples.len(),
            "passthrough resample changed length"
        );

        // Encode → verify WAV structure
        let encoded = encode_wav_mono_16k(&resampled);
        assert_eq!(&encoded[0..4], b"RIFF", "missing RIFF marker");
        assert_eq!(&encoded[8..12], b"WAVE", "missing WAVE marker");
        assert_eq!(
            encoded.len(),
            44 + samples.len() * 2,
            "encoded length mismatch"
        );
    }

    // ── SpeculativeVad (Q2-26) ──────────────────────────────────────────

    const SPEECH: f32 = 1.0;
    const QUIET: f32 = 0.0;
    const THRESHOLD: f32 = 0.5;

    #[test]
    fn vad_does_nothing_while_speech_continues() {
        let mut vad = SpeculativeVad::new(360, 30);
        for _ in 0..10 {
            assert_eq!(vad.on_rms(SPEECH, THRESHOLD), VadEvent::None);
        }
    }

    #[test]
    fn vad_spawns_once_on_first_silent_poll_then_goes_quiet() {
        let mut vad = SpeculativeVad::new(360, 30);
        vad.on_rms(SPEECH, THRESHOLD);
        assert_eq!(vad.on_rms(QUIET, THRESHOLD), VadEvent::SpawnSpeculative);
        // Subsequent silent polls before confirmation: no repeat spawn.
        assert_eq!(vad.on_rms(QUIET, THRESHOLD), VadEvent::None);
        assert_eq!(vad.on_rms(QUIET, THRESHOLD), VadEvent::None);
    }

    #[test]
    fn vad_confirms_after_silence_ms_elapses() {
        let mut vad = SpeculativeVad::new(90, 30); // 3 polls to confirm
        vad.on_rms(SPEECH, THRESHOLD);
        assert_eq!(vad.on_rms(QUIET, THRESHOLD), VadEvent::SpawnSpeculative); // 30ms
        assert_eq!(vad.on_rms(QUIET, THRESHOLD), VadEvent::None); // 60ms
        assert_eq!(vad.on_rms(QUIET, THRESHOLD), VadEvent::Confirmed); // 90ms
    }

    #[test]
    fn vad_discards_speculative_on_resumed_speech() {
        let mut vad = SpeculativeVad::new(360, 30);
        vad.on_rms(SPEECH, THRESHOLD);
        assert_eq!(vad.on_rms(QUIET, THRESHOLD), VadEvent::SpawnSpeculative);
        assert_eq!(vad.on_rms(QUIET, THRESHOLD), VadEvent::None);
        // False pause — speech resumes before confirmation.
        assert_eq!(vad.on_rms(SPEECH, THRESHOLD), VadEvent::DiscardSpeculative);
        assert_eq!(vad.on_rms(SPEECH, THRESHOLD), VadEvent::None);
    }

    #[test]
    fn vad_spawns_a_fresh_job_for_each_new_silence_run() {
        let mut vad = SpeculativeVad::new(360, 30);
        vad.on_rms(SPEECH, THRESHOLD);
        assert_eq!(vad.on_rms(QUIET, THRESHOLD), VadEvent::SpawnSpeculative);
        assert_eq!(vad.on_rms(SPEECH, THRESHOLD), VadEvent::DiscardSpeculative);
        // New silence run after the false pause — spawns again, independent
        // of the discarded one.
        assert_eq!(vad.on_rms(QUIET, THRESHOLD), VadEvent::SpawnSpeculative);
    }

    #[test]
    fn vad_repeated_speech_after_speech_is_a_noop() {
        let mut vad = SpeculativeVad::new(360, 30);
        assert_eq!(vad.on_rms(SPEECH, THRESHOLD), VadEvent::None);
        assert_eq!(vad.on_rms(SPEECH, THRESHOLD), VadEvent::None);
    }
}
