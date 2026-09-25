//! Whisper ASR: `WhisperRsInput` (voice input) and `WhisperKeywordDetector` (wake word).
//! Must build without ONNX Runtime (CI fast set): the composition root picks the detector.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_audio::{MicHandle, MicReader, MicState};
use pond_core::models::ports::voice_input::SpeculativeSignal;
use pond_core::models::ports::wake_word::{StreamingWakeWordDetector, WakeWordActivation};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod in_process;
pub use in_process::WhisperRsInput;

/// Play a short two-tone wake-word confirmation ping (C6→E6, ~220ms).
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

/// Synchronous transcription backend for the wake-word loop; always called in `spawn_blocking`.
pub trait WhisperBackend: Send + Sync {
    /// Transcribe 16 kHz mono PCM: artifacts stripped, `""` on silence, never panics.
    fn transcribe_pcm_blocking(&self, samples: &[f32]) -> Result<String>;
}

pub(crate) use pond_voice::text::strip_whisper_artifacts;

// ── WAV decoding ─────────────────────────────────────────────────────────────

/// Decode a WAV to mono f32 samples; returns `(samples, sample_rate)`.
pub(crate) fn decode_wav_mono_f32(wav: &[u8]) -> Result<(Vec<f32>, u32)> {
    // Phone uploads carry LIST chunks, odd fmt sizes, stereo and 24-bit; never assume byte 44.
    let decoded = pond_voice::dsp::decode_wav(wav).map_err(|e| anyhow!("{e}"))?;
    Ok((decoded.samples, decoded.sample_rate))
}

// ── Audio capture ─────────────────────────────────────────────────────────────

/// Open the shared mic and block until `Open`, `Denied` or `Failed`: `open()` is fire-and-forget.
fn open_mic_and_confirm(mic: &MicHandle) -> Result<u64> {
    let generation = mic.open_session();
    if !mic.wait_for(
        |s| !matches!(s, MicState::Closed),
        std::time::Duration::from_secs(2),
    ) {
        return Err(anyhow!("microphone did not respond"));
    }
    match mic.state() {
        MicState::Open => Ok(generation),
        MicState::Denied => Err(anyhow!(
            "microphone is disabled in Settings (mic_enabled = false)"
        )),
        MicState::Failed(e) => Err(anyhow!("microphone could not be opened: {}", e)),
        MicState::Closed => unreachable!("wait_for guarantees a non-Closed state"),
    }
}

/// Record until `silence_ms` of silence or `max_record_secs`; no onset wait, unlike the VAD path.
pub(crate) fn record_mono_f32_until_silence(
    mic: &MicHandle,
    max_record_secs: u32,
    silence_ms: u64,
    detector: &mut dyn SpeechDetector,
) -> Result<(Vec<f32>, u32)> {
    const POLL_MS: u64 = 30;

    // Privacy gate before open, so the OS mic indicator stays dark when the mic is disabled.
    pond_core::models::domain::mic_gate::ensure_mic_enabled()?;
    open_mic_and_confirm(mic)?;

    let sample_rate = pond_audio::CAPTURE_RATE_HZ;
    let mut reader = MicReader::new(mic.shared().clone());
    let mut samples: Vec<f32> = Vec::new();

    let max_ms = max_record_secs as u64 * 1000;
    let mut elapsed_ms: u64 = 0;
    let mut silent_for: u64 = 0;

    while elapsed_ms < max_ms {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        elapsed_ms += POLL_MS;
        samples.extend(reader.drain());

        let recent = (sample_rate as u64 * POLL_MS / 1000) as usize;
        let start = samples.len().saturating_sub(recent);

        if !detector.is_speech(&samples[start..]) {
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

    mic.close();
    Ok((samples, sample_rate))
}

use pond_voice::dsp::VadEvent;

use pond_voice::dsp::{SpeculativeVad, SpeechDetector};

/// Starts a background transcription, joined once end-of-speech is confirmed.
pub(crate) type SpeculativeSpawn =
    dyn Fn(Vec<f32>, u32) -> std::thread::JoinHandle<Result<String>> + Send + Sync;

/// Wait for onset, record until `silence_ms` of pause or the cap; empty samples = no speech.
/// `speculative_spawn` runs once per silence run; `on_speculative_event` gets Ready/Invalidated.
pub(crate) fn record_mono_f32_vad(
    mic: &MicHandle,
    max_wait_secs: u32,
    max_record_secs: u32,
    silence_ms: u64,
    speculative_spawn: Option<&SpeculativeSpawn>,
    on_speculative_event: Option<&(dyn Fn(SpeculativeSignal) + Send + Sync)>,
    audio_level_sink: Option<&ThrottledAudioLevelSink>,
    detector: &mut dyn SpeechDetector,
) -> Result<(Vec<f32>, u32, Option<String>)> {
    // Onset only, and energy-based on purpose: a model detector needs a window or two of
    // context, so it under-reports at onset and would clip the first word.
    const SPEECH_RMS: f32 = 0.010;
    const POLL_MS: u64 = 30;

    // Privacy gate before open, so the OS mic indicator stays dark when the mic is disabled.
    pond_core::models::domain::mic_gate::ensure_mic_enabled()?;
    open_mic_and_confirm(mic)?;

    let sample_rate = pond_audio::CAPTURE_RATE_HZ;
    let mut reader = MicReader::new(mic.shared().clone());
    let mut samples: Vec<f32> = Vec::new();

    // ── Wait for speech onset ───────────────────────────────────────────────
    let max_wait_ms = max_wait_secs as u64 * 1000;
    let mut waited_ms: u64 = 0;
    let mut speech_detected = false;

    while waited_ms < max_wait_ms {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        waited_ms += POLL_MS;
        samples.extend(reader.drain());

        let recent = (sample_rate as u64 * POLL_MS / 1000) as usize;
        let start = samples.len().saturating_sub(recent);
        let rms = rms_energy(&samples[start..]);
        if let Some(sink) = audio_level_sink {
            sink.maybe_emit(rms);
        }

        if rms >= SPEECH_RMS {
            speech_detected = true;
            break;
        }
    }

    if !speech_detected {
        mic.close();
        return Ok((samples, sample_rate, None)); // empty or just noise
    }

    // ── Record until end-of-speech ──────────────────────────────────────────
    let max_record_ms = max_record_secs as u64 * 1000;
    let mut recorded_ms: u64 = 0;
    let mut vad = SpeculativeVad::new(silence_ms, POLL_MS);
    let mut speculative: Option<std::thread::JoinHandle<Result<String>>> = None;
    // Set after `Ready`: reused on `Confirmed`; tells `DiscardSpeculative` to send `Invalidated`.
    let mut speculative_ready: Option<String> = None;
    let mut confirmed = false;

    while recorded_ms < max_record_ms {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        recorded_ms += POLL_MS;
        samples.extend(reader.drain());

        let recent = (sample_rate as u64 * POLL_MS / 1000) as usize;
        let start = samples.len().saturating_sub(recent);
        let frame = &samples[start..];
        if let Some(sink) = audio_level_sink {
            sink.maybe_emit(rms_energy(frame));
        }

        match vad.on_speech(detector.is_speech(frame)) {
            VadEvent::SpawnSpeculative => {
                if let Some(spawn) = speculative_spawn {
                    let snapshot = samples.clone();
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

        // Notify as soon as the job finishes, so the LLM can start before silence is confirmed.
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

    mic.close();

    let speculative_transcript = if confirmed {
        speculative_ready.or_else(|| speculative.and_then(|h| h.join().ok().and_then(|r| r.ok())))
    } else {
        None
    };

    Ok((samples, sample_rate, speculative_transcript))
}

// ── DSP helpers ───────────────────────────────────────────────────────────────

/// Linear interpolation resample to 16 000 Hz (whisper's expected rate).
pub(crate) use pond_voice::dsp::resample_to_16k;

// ── WAV encoding ──────────────────────────────────────────────────────────────

/// Encode mono 16-bit 16 kHz PCM as WAV bytes.
pub(crate) use pond_voice::dsp::encode_wav_mono_16k;

// ── WhisperKeywordDetector ────────────────────────────────────────────────────

/// Configuration for the sliding-window wake-word detector.
#[derive(Clone)]
pub struct KeywordDetectorConfig {
    /// Window per cycle (ms): a two-word phrase; wider costs more and lets whisper invent context.
    pub window_ms: u64,
    /// Window advance per cycle (ms); reaction time is at least this plus one transcription.
    pub slide_ms: u64,
    /// Audio kept from *before* the trigger (ms), covering the slide plus transcription lag.
    pub lookback_ms: u64,
    /// Ceiling on post-trigger capture (ms); silence normally ends it much sooner.
    pub post_trigger_ms: u64,
    /// Min window RMS worth a transcription; keeps a quiet room from costing anything.
    pub energy_threshold: f32,
    /// Post-trigger silence RMS; below [`Self::energy_threshold`] so quiet endings aren't clipped.
    pub silence_threshold: f32,
    /// Silence (ms) ending the post-trigger capture; 0 always captures the full ceiling.
    pub post_trigger_silence_ms: u64,
    /// Re-arm delay (ms) after an activation, so the reply's own tail can't re-trigger detection.
    pub cooldown_ms: u64,
}

impl Default for KeywordDetectorConfig {
    fn default() -> Self {
        Self {
            // ~1.4 s fits "hey goose" spoken slowly with room either side.
            window_ms: 1400,
            // Reaction floor: 200 ms + one transcription of a 1.4 s clip.
            slide_ms: 200,
            // A slide plus a slow transcription, so speech right after the wake word isn't lost.
            lookback_ms: 900,
            // A ceiling for uninterrupted speech; silence ends it far sooner.
            post_trigger_ms: 12_000,
            // ~-40 dBFS. Above a quiet room, below speech.
            energy_threshold: 0.010,
            // ~-52 dBFS: well under the gate, so a fading sentence isn't clipped.
            silence_threshold: 0.0025,
            post_trigger_silence_ms: 800,
            cooldown_ms: 600,
        }
    }
}

/// Sliding-window wake-word detector; returns the command audio so no second recording is needed.
pub struct WhisperKeywordDetector {
    backend: Arc<dyn WhisperBackend>,
    /// All normalized trigger variants. A transcript matching *any* of these fires detection.
    triggers: Vec<String>,
    prompt: String,
    config: KeywordDetectorConfig,
    /// Optional live mic-level reporter, fed from the detection loop's RMS.
    audio_level_sink: Option<Arc<ThrottledAudioLevelSink>>,
    /// Shared mic owner; all captures go through it so none races the follow-up VAD listen.
    mic: MicHandle,
}

impl WhisperKeywordDetector {
    /// Detector for wake phrase `trigger`; `mic` must be the process's one shared mic owner.
    pub fn new(
        backend: Arc<dyn WhisperBackend>,
        trigger: impl Into<String>,
        mic: MicHandle,
    ) -> Self {
        let raw = trigger.into();
        let prompt = format!("say \"{}\"", raw);
        Self {
            backend,
            triggers: vec![normalize_transcript(&raw)],
            prompt,
            config: KeywordDetectorConfig::default(),
            audio_level_sink: None,
            mic,
        }
    }

    /// Report live mic RMS through `sink` while listening and capturing the command.
    pub fn with_audio_level_sink(mut self, sink: Arc<ThrottledAudioLevelSink>) -> Self {
        self.audio_level_sink = Some(sink);
        self
    }

    /// Trigger on onboarding's calibrated variants; empty keeps `new()`'s trigger.
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
        // Always add the built-in mishearings of the primary trigger.
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

    /// The normalized variants this detector fires on; also what the transcriber strips.
    pub fn triggers(&self) -> &[String] {
        &self.triggers
    }
}

/// Common whisper mishearings of short wake words (worst on tiny/base models).
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

/// One implementation, shared with the matcher and stripper so all agree on what a word is.
use pond_voice::text::normalize_transcript;

/// RMS energy of a mono f32 slice; 0.0 when empty.
use pond_voice::dsp::rms as rms_energy;

pub use pond_core::shared::domain::agent::ThrottledAudioLevelSink;

#[async_trait]
impl StreamingWakeWordDetector for WhisperKeywordDetector {
    async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation> {
        let backend = self.backend.clone();
        let triggers = self.triggers.clone();
        let config = self.config.clone();
        let audio_level_sink = self.audio_level_sink.clone();
        let mic = self.mic.clone();

        // Dropping a `spawn_blocking` handle only detaches it; this flag is what stops the thread.
        // A `CancellationToken` can't help: the thread never awaits.
        let stop = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = StopOnDrop(stop.clone());

        tokio::task::spawn_blocking(move || {
            detection_loop(backend, triggers, config, stop, audio_level_sink, mic)
        })
        .await
        .map_err(|e| anyhow!("detection thread panicked: {}", e))?
    }

    fn activation_prompt(&self) -> &str {
        &self.prompt
    }
}

/// Sets its flag on drop, so dropping the future stops the blocking thread it spawned.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Sleep `ms`; false if `stop` cut it short. All detection waits use it to stay cancellable.
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

/// Blocking loop over the shared mic ring until a trigger is confirmed.
/// `pond_audio::spawn` must size the ring for `window_ms.max(lookback_ms) + post_trigger_ms`.
fn detection_loop(
    backend: Arc<dyn WhisperBackend>,
    triggers: Vec<String>,
    config: KeywordDetectorConfig,
    stop: Arc<AtomicBool>,
    audio_level_sink: Option<Arc<ThrottledAudioLevelSink>>,
    mic: MicHandle,
) -> Result<WakeWordActivation> {
    // Privacy gate before open, so the OS mic indicator stays dark when the mic is disabled.
    pond_core::models::domain::mic_gate::ensure_mic_enabled()?;
    // Scope every close to this generation: after a cancelled turn this detached thread may
    // no longer own the mic, and closing the follow-up capture's session would deafen it.
    let session = open_mic_and_confirm(&mic)?;

    let sample_rate = pond_audio::CAPTURE_RATE_HZ;

    // ── Cooldown — wait before re-arming (prevents TTS echo re-trigger) ───────
    // Slept in slices so a cancelled turn doesn't sit out the cooldown holding the mic.
    if config.cooldown_ms > 0 {
        tracing::debug!("KWS: cooldown {}ms before arming", config.cooldown_ms);
        if !sleep_unless_stopped(config.cooldown_ms, &stop) {
            mic.close_session(session);
            return Err(anyhow!("wake-word detection cancelled"));
        }
    }

    // ── Detection loop ────────────────────────────────────────────────────────
    let window_samples = (config.window_ms * sample_rate as u64 / 1000) as usize;
    let slide_ms = config.slide_ms;

    loop {
        if !sleep_unless_stopped(slide_ms, &stop) {
            tracing::debug!("KWS: cancelled — releasing the microphone");
            mic.close_session(session);
            return Err(anyhow!("wake-word detection cancelled"));
        }

        let snapshot = mic.shared().recent(window_samples);

        if snapshot.len() < window_samples / 2 {
            continue; // buffer not yet full enough — keep waiting
        }

        // ── Energy gate — skip silent windows before hitting whisper ──────────
        // Computed even with the gate off (threshold 0), so the level sink still gets readings.
        let window_rms = rms_energy(&snapshot);
        if let Some(sink) = &audio_level_sink {
            sink.maybe_emit(window_rms);
        }
        if config.energy_threshold > 0.0 && window_rms < config.energy_threshold {
            tracing::trace!("KWS: silent window skipped (rms={:.4})", window_rms);
            continue;
        }

        // The shared ring is already normalised 16 kHz mono f32 — no resample.
        let transcript = match backend.transcribe_pcm_blocking(&snapshot) {
            Ok(t) if !t.is_empty() => {
                // Re-strip: a stray bracketed tag must never reach trigger matching.
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

        // Whole-word match, not `contains` (which fired on "mongoose").
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

            // Capture until `post_trigger_silence_ms` of silence or the `post_trigger_ms` ceiling.
            let poll_ms = 50u64;
            let mut elapsed_ms = 0u64;
            let mut silent_for_ms = 0u64;

            while elapsed_ms < config.post_trigger_ms {
                if !sleep_unless_stopped(poll_ms, &stop) {
                    mic.close();
                    return Err(anyhow!("wake-word detection cancelled"));
                }
                elapsed_ms += poll_ms;

                // Computed even with the silence gate off, so the level sink reports throughout.
                let recent_samples = (sample_rate as u64 * poll_ms / 1000) as usize;
                let recent_rms = rms_energy(&mic.shared().recent(recent_samples));
                if let Some(sink) = &audio_level_sink {
                    sink.maybe_emit(recent_rms);
                }

                if config.post_trigger_silence_ms > 0 && config.silence_threshold > 0.0 {
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

            // Add `lookback_ms`: detection lags, so post-trigger audio alone loses the first words.
            let captured_ms = elapsed_ms + config.lookback_ms;
            let captured_samples = (captured_ms * sample_rate as u64 / 1000) as usize;
            let command_audio = mic.shared().recent(captured_samples);
            tracing::debug!(
                "KWS: captured {}ms ({}ms lookback + {}ms after the trigger)",
                captured_ms,
                config.lookback_ms,
                elapsed_ms
            );

            mic.close();

            let cmd_wav = encode_wav_mono_16k(&command_audio);

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
    use pond_voice::dsp::RmsDetector;

    // ── Detection tuning ──────────────────────────────────────────────────
    // These check relationships between the knobs, not their (retunable) values.

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

    /// An undersized ring silently returns short clips, with no error.
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

    #[test]
    fn the_detection_window_is_sized_for_a_wake_phrase() {
        let c = KeywordDetectorConfig::default();
        assert!(
            (1000..=2000).contains(&c.window_ms),
            "window {}ms: under 1s truncates 'hey goose', over 2s is waste",
            c.window_ms
        );
    }

    #[test]
    fn re_arming_is_quick_enough_to_answer_a_follow_up() {
        let c = KeywordDetectorConfig::default();
        assert!(
            c.cooldown_ms <= 1000,
            "cooldown {}ms is dead time the user experiences as being ignored",
            c.cooldown_ms
        );
    }

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

    /// A no-hardware `MicHandle`, for tests that only need one to construct, not to capture.
    fn test_mic() -> MicHandle {
        let (mic, _join) = pond_audio::spawn(
            Box::new(pond_audio::testing::ScriptedCapture::silence(0, 20)),
            pond_audio::CAPTURE_RATE_HZ,
            5_000,
            true,
        );
        mic
    }

    /// An unnormalized variant would match but never be stripped.
    #[test]
    fn the_resolved_triggers_are_exposed_and_all_normalized() {
        let d = WhisperKeywordDetector::new(Arc::new(DeafBackend), "goose", test_mic())
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

    #[test]
    fn calibrated_variants_and_builtin_mishearings_both_survive() {
        let d = WhisperKeywordDetector::new(Arc::new(DeafBackend), "goose", test_mic())
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

    /// jfk.wav is whisper.cpp's canonical sample: 16-bit mono 16 kHz PCM.
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

    // ── SpeculativeVad ──────────────────────────────────────────────────

    const SPEECH: f32 = 1.0;
    const QUIET: f32 = 0.0;
    const THRESHOLD: f32 = 0.5;

    #[test]
    fn vad_does_nothing_while_speech_continues() {
        let mut vad = SpeculativeVad::new(360, 30);
        for _ in 0..10 {
            assert_eq!(vad.on_speech(SPEECH >= THRESHOLD), VadEvent::None);
        }
    }

    #[test]
    fn vad_spawns_once_on_first_silent_poll_then_goes_quiet() {
        let mut vad = SpeculativeVad::new(360, 30);
        vad.on_speech(SPEECH >= THRESHOLD);
        assert_eq!(
            vad.on_speech(QUIET >= THRESHOLD),
            VadEvent::SpawnSpeculative
        );
        // Subsequent silent polls before confirmation: no repeat spawn.
        assert_eq!(vad.on_speech(QUIET >= THRESHOLD), VadEvent::None);
        assert_eq!(vad.on_speech(QUIET >= THRESHOLD), VadEvent::None);
    }

    #[test]
    fn vad_confirms_after_silence_ms_elapses() {
        let mut vad = SpeculativeVad::new(90, 30); // 3 polls to confirm
        vad.on_speech(SPEECH >= THRESHOLD);
        assert_eq!(
            vad.on_speech(QUIET >= THRESHOLD),
            VadEvent::SpawnSpeculative
        ); // 30ms
        assert_eq!(vad.on_speech(QUIET >= THRESHOLD), VadEvent::None); // 60ms
        assert_eq!(vad.on_speech(QUIET >= THRESHOLD), VadEvent::Confirmed); // 90ms
    }

    #[test]
    fn vad_discards_speculative_on_resumed_speech() {
        let mut vad = SpeculativeVad::new(360, 30);
        vad.on_speech(SPEECH >= THRESHOLD);
        assert_eq!(
            vad.on_speech(QUIET >= THRESHOLD),
            VadEvent::SpawnSpeculative
        );
        assert_eq!(vad.on_speech(QUIET >= THRESHOLD), VadEvent::None);
        // False pause — speech resumes before confirmation.
        assert_eq!(
            vad.on_speech(SPEECH >= THRESHOLD),
            VadEvent::DiscardSpeculative
        );
        assert_eq!(vad.on_speech(SPEECH >= THRESHOLD), VadEvent::None);
    }

    #[test]
    fn vad_spawns_a_fresh_job_for_each_new_silence_run() {
        let mut vad = SpeculativeVad::new(360, 30);
        vad.on_speech(SPEECH >= THRESHOLD);
        assert_eq!(
            vad.on_speech(QUIET >= THRESHOLD),
            VadEvent::SpawnSpeculative
        );
        assert_eq!(
            vad.on_speech(SPEECH >= THRESHOLD),
            VadEvent::DiscardSpeculative
        );
        // A new silence run after the false pause spawns again.
        assert_eq!(
            vad.on_speech(QUIET >= THRESHOLD),
            VadEvent::SpawnSpeculative
        );
    }

    #[test]
    fn vad_repeated_speech_after_speech_is_a_noop() {
        let mut vad = SpeculativeVad::new(360, 30);
        assert_eq!(vad.on_speech(SPEECH >= THRESHOLD), VadEvent::None);
        assert_eq!(vad.on_speech(SPEECH >= THRESHOLD), VadEvent::None);
    }

    // ── wake-word cancellation ───────────────────────────────────────────

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

    // ── Shared mic owner: the wake-word/VAD handoff race ────────────────────

    #[tokio::test]
    async fn wake_word_then_follow_up_capture_share_the_mic_without_racing() {
        struct AlwaysMatches;
        impl WhisperBackend for AlwaysMatches {
            fn transcribe_pcm_blocking(&self, _: &[f32]) -> Result<String> {
                Ok("goose".to_string())
            }
        }

        let (mic, _join) = pond_audio::spawn(
            Box::new(pond_audio::testing::ScriptedCapture::utterance(
                2_000, 2_000, 20,
            )),
            pond_audio::CAPTURE_RATE_HZ,
            15_000,
            true,
        );

        let detector = WhisperKeywordDetector::new(Arc::new(AlwaysMatches), "goose", mic.clone())
            .with_config(KeywordDetectorConfig {
                cooldown_ms: 0,
                window_ms: 200,
                slide_ms: 20,
                lookback_ms: 100,
                post_trigger_ms: 100,
                post_trigger_silence_ms: 0,
                ..KeywordDetectorConfig::default()
            });

        let activation = detector
            .wait_for_activation_with_audio()
            .await
            .expect("wake word must fire against a scripted utterance");
        assert!(
            activation.captured_audio.is_some(),
            "a confirmed activation must carry captured command audio"
        );

        // Races the detector's close against this open, as `run_loop` does between turns.
        let result = tokio::task::spawn_blocking(move || {
            let mut detector = RmsDetector::new(0.005);
            record_mono_f32_vad(&mic, 1, 1, 200, None, None, None, &mut detector)
        })
        .await
        .expect("capture thread must not panic");

        assert!(
            result.is_ok(),
            "the follow-up capture must not fail from a device race: {:?}",
            result.err()
        );
    }
}
