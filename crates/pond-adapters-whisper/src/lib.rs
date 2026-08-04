//! Whisper ASR adapter for Goose In A Pond.
//!
//! Exports:
//! - `WhisperRsInput`         — in-process `VoiceInput` port (whisper-rs, default)
//! - `WhisperKeywordDetector` — `WakeWordDetector` port: poll mic until trigger phrase heard
//! - `WhisperBackend`         — backend trait the detector uses to transcribe windows
//!
//! ## Default — in-process (`WhisperRsInput`)
//!
//! Loads a ggml `.bin` model directly via the whisper.cpp bindings. No port,
//! no subprocess, no multipart HTTP. Shares the ggml CUDA primary context with
//! `llama-cpp-2` on Jetson. The HTTP `WhisperInput` this replaced was deleted
//! in 2026-08.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use pond_core::models::ports::voice_input::SpeculativeSignal;
use pond_core::models::ports::wake_word::{StreamingWakeWordDetector, WakeWordActivation};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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

/// Whisper's non-speech annotations, stripped in the leaf crate so the
/// desktop shell shares one implementation instead of carrying a copy.
pub(crate) use pond_voice::text::strip_whisper_artifacts;

// ── WAV decoding ─────────────────────────────────────────────────────────────

/// Decode a 16-bit mono PCM WAV (as produced by `encode_wav_mono_16k`) back to
/// f32 samples.  Returns `(samples, sample_rate)`.
pub(crate) fn decode_wav_mono_f32(wav: &[u8]) -> Result<(Vec<f32>, u32)> {
    // Delegates to the real RIFF chunk walker in pond-voice. What used to be
    // here assumed `data` started at byte 44 and the payload was 16-bit mono —
    // true only of WAVs GIAP produced itself. This function backs
    // POST /api/v1/transcribe, which real phone recorders hit with LIST chunks,
    // 18/40-byte fmt chunks, EXTENSIBLE, stereo and 24-bit; all of those used
    // to decode to noise, so whisper hallucinated instead of erroring.
    let decoded = pond_voice::dsp::decode_wav(wav).map_err(|e| anyhow!("{e}"))?;
    Ok((decoded.samples, decoded.sample_rate))
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

    // Privacy gate: refuse to OPEN the device, so the OS microphone indicator
    // stays dark. Filtering samples after capture would leave it lit and make
    // the setting a lie.
    pond_core::models::domain::mic_gate::ensure_mic_enabled()?;

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
            |e| tracing::warn!("audio stream error: {}", e),
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
            |e| tracing::warn!("audio stream error: {}", e),
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
            |e| tracing::warn!("audio stream error: {}", e),
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

use pond_voice::dsp::VadEvent;

use pond_voice::dsp::SpeculativeVad;

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

    // Privacy gate: refuse to OPEN the device, so the OS microphone indicator
    // stays dark. Filtering samples after capture would leave it lit and make
    // the setting a lie.
    pond_core::models::domain::mic_gate::ensure_mic_enabled()?;

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
            |e| tracing::warn!("audio stream error: {}", e),
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
            |e| tracing::warn!("audio stream error: {}", e),
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
            |e| tracing::warn!("audio stream error: {}", e),
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
    // Abandoned jobs, kept so we can tell whether they are still burning CPU.
    //
    // Dropping a `JoinHandle` DETACHES the thread; it does not stop it. Every
    // abandoned speculative job therefore ran a full `TranscribeOpts::accurate()`
    // — six threads, beam 5 — to completion, unwatched. And a new one was
    // spawned on every micro-pause, because `SpawnSpeculative` fires on the
    // first silent poll after speech. On a six-core Orin that put three and
    // four of them on the CPU at once.
    //
    // Measured in the session log before this guard existed: isolated calls
    // took 1.3-1.9 s regardless of clip length, while clustered ones — three or
    // four starting within a second of each other, on snapshots of the SAME
    // growing recording — stretched to 3.4 s and 5.5 s. 14 of 26 calls
    // overlapped another. That contention, not the encoder and not the
    // temperature ladder, was the ~2.6 s overrun past the 800 ms silence window.
    let mut stale: Option<std::thread::JoinHandle<Result<String>>> = None;
    // Below this, a snapshot is not worth a transcription: whisper.cpp drops
    // anything under ~100 ms outright, and a fragment this short is a pause in
    // the middle of a sentence rather than the end of one.
    const MIN_SPECULATIVE_MS: usize = 400;
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
                    // Retire a finished abandoned job so it stops blocking.
                    if stale.as_ref().is_some_and(|h| h.is_finished()) {
                        stale = None;
                    }
                    let snapshot = samples.lock().unwrap().clone();
                    let snapshot_ms = snapshot.len() * 1000 / sample_rate.max(1) as usize;
                    // Skipping a spawn is correctness-neutral. If no speculative
                    // transcript exists, `listen_inner` falls through to the
                    // confirmed pass, which transcribes the FULL recording with
                    // the same settings and strictly more audio — so the worst
                    // case is the latency we had before speculation, never a
                    // worse transcript.
                    if stale.is_some() || speculative.is_some() {
                        tracing::debug!(
                            snapshot_ms,
                            "skipping speculative spawn: one is still in flight"
                        );
                    } else if snapshot_ms < MIN_SPECULATIVE_MS {
                        tracing::debug!(snapshot_ms, "skipping speculative spawn: clip too short");
                    } else {
                        speculative = Some(spawn(snapshot, sample_rate));
                        speculative_ready = None;
                    }
                }
            }
            VadEvent::DiscardSpeculative => {
                if speculative_ready.is_some() {
                    if let Some(cb) = on_speculative_event {
                        cb(SpeculativeSignal::Invalidated);
                    }
                }
                // Move it aside rather than dropping it. We cannot cancel a
                // whisper.cpp call in flight, but we can decline to start a
                // second one while it is still running — which is the whole
                // difference between one job and four.
                if let Some(h) = speculative.take() {
                    if !h.is_finished() {
                        stale = Some(h);
                    }
                }
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
pub(crate) use pond_voice::dsp::resample_to_16k;

// ── WAV encoding ──────────────────────────────────────────────────────────────

/// Encode mono 16-bit PCM at 16 kHz as a WAV byte vector.
/// Avoids any external WAV crate dependency.
pub(crate) use pond_voice::dsp::encode_wav_mono_16k;

// ── WhisperKeywordDetector ────────────────────────────────────────────────────

/// Configuration for the sliding-window wake-word detector.
#[derive(Clone)]
pub struct KeywordDetectorConfig {
    /// Width of the audio window fed to whisper on each cycle (milliseconds).
    ///
    /// A wake word is under a second. A window several times that length gives
    /// the model room to invent context around it and costs proportionally
    /// more to transcribe, so it is kept just wide enough for a two-word
    /// phrase spoken unhurriedly.
    pub window_ms: u64,
    /// How far to advance the window on each detection cycle (milliseconds).
    ///
    /// Sets the floor on reaction time: the wake word cannot be noticed sooner
    /// than the next slide, plus one transcription.
    pub slide_ms: u64,
    /// Audio captured *before* the trigger fired (milliseconds).
    ///
    /// Detection is not instant — a slide plus a transcription elapses between
    /// the user finishing the wake word and this loop noticing. Without a
    /// lookback that gap is simply lost, which is why "Goose, what's the
    /// weather" used to arrive as "the weather". Capturing backwards covers
    /// the latency; the wake word itself comes off the transcript afterwards
    /// via [`pond_voice::text::strip_leading_wake_word`].
    pub lookback_ms: u64,
    /// Ceiling on audio captured after detection fires (milliseconds).
    ///
    /// A ceiling, not a target: [`Self::silence_threshold`] normally ends the
    /// capture much sooner. It only binds when someone talks continuously.
    pub post_trigger_ms: u64,
    /// Minimum RMS energy required to spend a transcription on a window.
    ///
    /// Below this, the window is skipped without waking whisper at all — which
    /// is what keeps a quiet room from costing anything.
    pub energy_threshold: f32,
    /// RMS below which the post-trigger capture counts a poll as silent.
    ///
    /// Deliberately separate from [`Self::energy_threshold`] and lower. The
    /// two thresholds want opposite things: the gate should be high so room
    /// tone never reaches whisper, while end-of-utterance should be low so a
    /// trailing-off sentence is not clipped. One shared value cannot be right
    /// for both, and was previously tuned for the gate — so quiet endings got
    /// cut off.
    pub silence_threshold: f32,
    /// Consecutive silence (ms) that ends the post-trigger capture.
    ///
    /// Long enough to sit through the pause mid-sentence, short enough not to
    /// feel like a wait. Set to 0 to always capture the full ceiling.
    pub post_trigger_silence_ms: u64,
    /// Settling time (ms) before detection re-arms after an activation.
    ///
    /// Covers the speaker ringing out and the output device draining, so the
    /// tail of the assistant's own reply cannot re-trigger the wake word.
    pub cooldown_ms: u64,
}

impl Default for KeywordDetectorConfig {
    fn default() -> Self {
        Self {
            // ~1.4 s fits "hey goose" spoken slowly with room either side.
            window_ms: 1400,
            // Reaction floor: 200 ms + one transcription of a 1.4 s clip.
            slide_ms: 200,
            // Covers a slide plus a slow transcription, so nothing said
            // straight after the wake word is lost.
            lookback_ms: 900,
            // A ceiling for uninterrupted speech; silence ends it far sooner.
            post_trigger_ms: 12_000,
            // ~-40 dBFS. Above a quiet room, below speech.
            energy_threshold: 0.010,
            // ~-52 dBFS. Well under the gate so a fading sentence still counts
            // as speech and is not clipped.
            silence_threshold: 0.0025,
            post_trigger_silence_ms: 800,
            cooldown_ms: 600,
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
    /// Always `WhisperRsInput` since the HTTP backend was removed.
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
        let prompt = format!("say \"{}\"", raw);
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

    /// The normalized variants this detector fires on.
    ///
    /// Handed to the transcription adapter so the words that trigger a turn
    /// are exactly the words stripped from the front of the command. They are
    /// resolved here — calibration plus built-in fuzzy spellings — and
    /// re-deriving them anywhere else would let the two drift.
    pub fn triggers(&self) -> &[String] {
        &self.triggers
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

/// Normalization lives in `pond-voice`, beside the matcher that consumes it
/// and the stripper that undoes it. All three have to agree on what a word is,
/// and they only reliably agree if there is one implementation of it.
use pond_voice::text::normalize_transcript;

/// Root-mean-square energy of a mono f32 sample slice.
/// Returns 0.0 for an empty slice.
use pond_voice::dsp::rms as rms_energy;

#[async_trait]
impl StreamingWakeWordDetector for WhisperKeywordDetector {
    async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation> {
        let backend = self.backend.clone();
        let triggers = self.triggers.clone();
        let config = self.config.clone();

        // `run_loop` races this future against the turn and drops it when the
        // turn wins. Dropping a `spawn_blocking` JoinHandle DETACHES the task —
        // tokio cannot cancel a blocking thread — so without a flag the thread
        // ran forever, holding a cpal stream and firing whisper over a 2.5 s
        // window every `slide_ms`. Three turns in, an Orin was doing nothing
        // but wake-word detection. A `CancellationToken` cannot help here: it
        // is async-only and this thread never awaits.
        let stop = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = StopOnDrop(stop.clone());

        tokio::task::spawn_blocking(move || detection_loop(backend, triggers, config, stop))
            .await
            .map_err(|e| anyhow!("detection thread panicked: {}", e))?
    }

    fn activation_prompt(&self) -> &str {
        &self.prompt
    }
}

/// Sets its flag on drop, so dropping a future cancels the blocking thread it
/// spawned.
///
/// Lives at module scope rather than inside the async fn purely so the drop
/// behaviour — the whole mechanism of the leak fix — is directly testable.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Sleep `ms`, waking early if `stop` is set. Returns false when cancelled.
///
/// Every wait inside the detection thread goes through this: a `spawn_blocking`
/// task cannot be cancelled from outside, so responsiveness to the stop flag is
/// bounded by the longest uninterruptible sleep.
fn sleep_unless_stopped(ms: u64, stop: &AtomicBool) -> bool {
    const SLICE_MS: u64 = 50;
    let mut remaining = ms;
    while remaining > 0 {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let slice = remaining.min(SLICE_MS);
        std::thread::sleep(std::time::Duration::from_millis(slice));
        remaining -= slice;
    }
    !stop.load(Ordering::SeqCst)
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
    stop: Arc<AtomicBool>,
) -> Result<WakeWordActivation> {
    // ── Open continuous audio stream ─────────────────────────────────────────
    // Privacy gate: refuse to OPEN the device, so the OS microphone indicator
    // stays dark. Filtering samples after capture would leave it lit and make
    // the setting a lie.
    pond_core::models::domain::mic_gate::ensure_mic_enabled()?;

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("No audio input device found"))?;
    let stream_config = device
        .default_input_config()
        .map_err(|e| anyhow!("Failed to get input config: {}", e))?;

    let sample_rate = stream_config.sample_rate().0;
    let channels = stream_config.channels() as usize;

    // The ring must satisfy whichever reader needs more history: detection
    // reads one `window_ms`, and a capture reads `lookback_ms` before the
    // trigger plus up to `post_trigger_ms` after it.
    let history_ms = config.window_ms.max(config.lookback_ms) + config.post_trigger_ms;
    let max_samples = (history_ms * sample_rate as u64 / 1000) as usize;
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
    // Slept in slices so a cancelled turn is not stuck here for the full
    // 2 s default still holding the microphone.
    if config.cooldown_ms > 0 {
        tracing::debug!("KWS: cooldown {}ms before arming", config.cooldown_ms);
        if !sleep_unless_stopped(config.cooldown_ms, &stop) {
            return Err(anyhow!("wake-word detection cancelled"));
        }
    }

    // ── Detection loop ────────────────────────────────────────────────────────
    let window_samples = (config.window_ms * sample_rate as u64 / 1000) as usize;
    let slide_ms = config.slide_ms;

    loop {
        if !sleep_unless_stopped(slide_ms, &stop) {
            tracing::debug!("KWS: cancelled — releasing the microphone");
            return Err(anyhow!("wake-word detection cancelled"));
        }

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

        // Whole-word matching, not `contains` — see
        // `pond_voice::text::find_trigger_words` for why the substring form
        // both missed real activations and fired on "mongoose".
        let matched = triggers
            .iter()
            .any(|t| pond_voice::text::contains_trigger(&transcript, t));
        tracing::debug!(
            "KWS window: \"{}\" (triggers: {:?}, matched: {})",
            transcript,
            triggers,
            matched
        );

        if matched {
            tracing::info!(
                "Wake word confirmed: \"{}\" (matched triggers: {:?})",
                transcript,
                triggers
            );
            play_wake_ping();

            // VAD-gated post-trigger: poll every 50 ms and exit as soon as the
            // microphone goes silent for `post_trigger_silence_ms` consecutive ms.
            // Falls back to waiting the full `post_trigger_ms` if VAD is disabled
            // or the user keeps speaking past the ceiling.
            let poll_ms = 50u64;
            let mut elapsed_ms = 0u64;
            let mut silent_for_ms = 0u64;

            while elapsed_ms < config.post_trigger_ms {
                if !sleep_unless_stopped(poll_ms, &stop) {
                    return Err(anyhow!("wake-word detection cancelled"));
                }
                elapsed_ms += poll_ms;

                if config.post_trigger_silence_ms > 0 && config.silence_threshold > 0.0 {
                    let recent_rms = {
                        let r = ring.lock().unwrap();
                        let recent_samples = (sample_rate as u64 * poll_ms / 1000) as usize;
                        let start = r.len().saturating_sub(recent_samples);
                        let chunk: Vec<f32> = r.range(start..).copied().collect();
                        rms_energy(&chunk)
                    };
                    if recent_rms < config.silence_threshold {
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

            // Reach BACK past the trigger as well as forward. Detection lags
            // the wake word by a slide plus a transcription, and everything
            // spoken in that gap is already in the ring — taking only the
            // post-trigger audio threw it away, clipping the first words of
            // every one-breath request. The wake word rides along in the clip
            // and is removed from the transcript, not from the audio.
            let captured_ms = elapsed_ms + config.lookback_ms;
            let captured_samples = (captured_ms * sample_rate as u64 / 1000) as usize;
            let command_audio: Vec<f32> = {
                let r = ring.lock().unwrap();
                let start = r.len().saturating_sub(captured_samples);
                r.range(start..).copied().collect()
            };
            tracing::debug!(
                "KWS: captured {}ms ({}ms lookback + {}ms after the trigger)",
                captured_ms,
                config.lookback_ms,
                elapsed_ms
            );

            drop(stream); // stop recording

            let cmd_resampled = resample_to_16k(&command_audio, sample_rate);
            let cmd_wav = encode_wav_mono_16k(&cmd_resampled);

            return Ok(WakeWordActivation {
                captured_audio: Some(cmd_wav),
            });
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Detection tuning ──────────────────────────────────────────────────
    //
    // These assert the *relationships* between the knobs, not the numbers.
    // A number can be retuned on real hardware; a broken relationship is a
    // silent regression, and every one of these encodes a bug that shipped.

    /// The capture must be able to reach back over the detection latency, or
    /// the first words after the wake word are lost — which is what made
    /// "Goose, what's the weather" arrive as "the weather".
    #[test]
    fn the_lookback_covers_the_worst_case_detection_lag() {
        let c = KeywordDetectorConfig::default();
        assert!(
            c.lookback_ms >= c.slide_ms * 2,
            "lookback {}ms must cover a slide ({}ms) plus a slow transcription",
            c.lookback_ms,
            c.slide_ms
        );
    }

    /// The ring is the only copy of the audio. If it holds less than a reader
    /// asks for, the read silently returns a short clip — clipped speech, no
    /// error, no way to tell from the transcript.
    #[test]
    fn the_ring_holds_everything_both_readers_can_ask_for() {
        let c = KeywordDetectorConfig::default();
        let ring_ms = c.window_ms.max(c.lookback_ms) + c.post_trigger_ms;
        assert!(ring_ms >= c.window_ms, "detection reads a full window");
        assert!(
            ring_ms >= c.lookback_ms + c.post_trigger_ms,
            "a maximum-length capture must fit: need {}ms, ring holds {}ms",
            c.lookback_ms + c.post_trigger_ms,
            ring_ms
        );
    }

    /// The gate keeps room tone away from whisper; the VAD decides when a
    /// sentence ended. One value cannot serve both, and when it did, the
    /// gate's value won and quiet sentence endings were cut off.
    #[test]
    fn ending_a_sentence_is_judged_more_leniently_than_waking_whisper() {
        let c = KeywordDetectorConfig::default();
        assert!(
            c.silence_threshold < c.energy_threshold,
            "silence {} must be under the gate {} or trailing speech is clipped",
            c.silence_threshold,
            c.energy_threshold
        );
        assert!(
            c.silence_threshold > 0.0,
            "0 disables end-of-speech entirely"
        );
    }

    /// Reaction time floor. A user who says the wake word and waits should
    /// not be able to notice the wait.
    #[test]
    fn the_wake_word_is_noticed_within_a_slide_of_being_said() {
        let c = KeywordDetectorConfig::default();
        assert!(
            c.slide_ms <= 250,
            "slide {}ms is a visible delay",
            c.slide_ms
        );
        assert!(c.slide_ms >= 100, "under 100ms is duty cycle for no gain");
    }

    /// The window exists to hold a wake phrase, not a sentence. Wider means
    /// more for the model to invent context from and more to transcribe on
    /// every single cycle.
    #[test]
    fn the_detection_window_is_sized_for_a_wake_phrase() {
        let c = KeywordDetectorConfig::default();
        assert!(
            (1000..=2000).contains(&c.window_ms),
            "window {}ms: under 1s truncates 'hey goose', over 2s is waste",
            c.window_ms
        );
    }

    /// Long enough for the speaker to stop ringing, short enough that the
    /// wake word works immediately after a reply.
    #[test]
    fn re_arming_is_quick_enough_to_answer_a_follow_up() {
        let c = KeywordDetectorConfig::default();
        assert!(
            c.cooldown_ms <= 1000,
            "cooldown {}ms is dead time the user experiences as being ignored",
            c.cooldown_ms
        );
    }

    /// A ceiling, not a target — silence normally ends the capture. It has to
    /// clear a real spoken request with a pause in the middle.
    #[test]
    fn the_capture_ceiling_allows_a_full_spoken_request() {
        let c = KeywordDetectorConfig::default();
        assert!(
            c.post_trigger_ms >= 8_000,
            "ceiling {}ms truncates a long request",
            c.post_trigger_ms
        );
        assert!(c.post_trigger_silence_ms < c.post_trigger_ms);
    }

    // ── Trigger resolution ────────────────────────────────────────────────

    struct DeafBackend;
    impl WhisperBackend for DeafBackend {
        fn transcribe_pcm_blocking(&self, _: &[f32]) -> Result<String> {
            Ok(String::new())
        }
    }

    /// The transcriber strips exactly what the detector matched, so the list
    /// has to be reachable — and every entry normalized, or a variant with a
    /// capital or a comma would match but never strip.
    #[test]
    fn the_resolved_triggers_are_exposed_and_all_normalized() {
        let d = WhisperKeywordDetector::new(Arc::new(DeafBackend), "goose")
            .with_transcriptions(vec!["Hey, Goose!".into(), "  a goose  ".into()]);

        let triggers = d.triggers();
        assert!(!triggers.is_empty());
        for t in triggers {
            assert_eq!(
                *t,
                pond_voice::text::normalize_transcript(t),
                "{t:?} is not in normalized form"
            );
            assert!(!t.is_empty());
        }
    }

    /// Calibration variants must survive alongside the built-in mishearings —
    /// dropping either halves detection for someone whose accent whisper
    /// renders unusually.
    #[test]
    fn calibrated_variants_and_builtin_mishearings_both_survive() {
        let d = WhisperKeywordDetector::new(Arc::new(DeafBackend), "goose")
            .with_transcriptions(vec!["goose".into(), "Hey, Goose.".into()]);
        let t = d.triggers();
        assert!(t.iter().any(|x| x == "hey goose"), "calibrated: {t:?}");
        assert!(t.iter().any(|x| x == "goos"), "built-in mishearing: {t:?}");
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

    // ── wake-word cancellation ───────────────────────────────────────────
    //
    // The leak: `run_loop` races `wait_for_activation_with_audio` against the
    // turn and drops the losing future. `spawn_blocking` DETACHES on handle
    // drop, so the detection thread kept running — holding a cpal stream and
    // firing a 6-thread whisper every `slide_ms` — for the life of the
    // process. These cover the two halves of the fix; the end-to-end proof is
    // a flat `ps -T` thread count across turns on-device.

    #[test]
    fn stop_on_drop_sets_the_flag() {
        let flag = Arc::new(AtomicBool::new(false));
        {
            let _guard = StopOnDrop(flag.clone());
            assert!(!flag.load(Ordering::SeqCst), "not set before drop");
        }
        assert!(
            flag.load(Ordering::SeqCst),
            "dropping the guard must cancel"
        );
    }

    #[test]
    fn sleep_unless_stopped_runs_to_completion_when_not_cancelled() {
        let stop = AtomicBool::new(false);
        let t0 = std::time::Instant::now();
        assert!(sleep_unless_stopped(120, &stop));
        assert!(t0.elapsed() >= std::time::Duration::from_millis(100));
    }

    #[test]
    fn sleep_unless_stopped_returns_false_immediately_when_already_stopped() {
        let stop = AtomicBool::new(true);
        let t0 = std::time::Instant::now();
        assert!(!sleep_unless_stopped(5_000, &stop));
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(200),
            "must not serve out a 5s sleep after cancellation"
        );
    }

    /// The cooldown default is 2000 ms. Cancellation must not have to wait it
    /// out while still holding the microphone.
    #[test]
    fn a_long_wait_is_cut_short_by_cancellation_mid_sleep() {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let h = std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let finished = sleep_unless_stopped(2_000, &s2);
            (finished, t0.elapsed())
        });
        std::thread::sleep(std::time::Duration::from_millis(120));
        stop.store(true, Ordering::SeqCst);

        let (finished, elapsed) = h.join().unwrap();
        assert!(!finished, "must report cancellation");
        assert!(
            elapsed < std::time::Duration::from_millis(1_000),
            "woke after {elapsed:?}; should be within one 50ms slice of the signal"
        );
    }
}
