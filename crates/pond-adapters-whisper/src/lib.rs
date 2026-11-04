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
pub use in_process::{TranscribeOpts, WhisperRsInput};

/// How long other audio stays paused while the words after a wake word are captured, renewed as
/// capture goes on, and how long after capture for the reply's turn to take over.
const QUIET_WHILE_LISTENING: std::time::Duration = std::time::Duration::from_secs(2);
const QUIET_UNTIL_THE_TURN: std::time::Duration = std::time::Duration::from_secs(5);

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
/// A failed open releases its claim: claims keep the device open until the last one is released.
fn open_mic_and_confirm(mic: &MicHandle) -> Result<u64> {
    let generation = mic.open_session();
    let failed = |e: anyhow::Error| {
        mic.close_session(generation);
        Err(e)
    };
    if !mic.wait_for(
        |s| !matches!(s, MicState::Closed),
        std::time::Duration::from_secs(2),
    ) {
        return failed(anyhow!("microphone did not respond"));
    }
    match mic.state() {
        MicState::Open => Ok(generation),
        MicState::Denied => failed(anyhow!(
            "microphone is disabled in Settings (mic_enabled = false)"
        )),
        MicState::Failed(e) => failed(anyhow!("microphone could not be opened: {}", e)),
        MicState::Closed => unreachable!("wait_for guarantees a non-Closed state"),
    }
}

use pond_voice::dsp::VadEvent;

use pond_voice::dsp::{SpeculativeVad, SpeechDetector, Windower};

/// Samples per speech verdict: Silero's window at 16 kHz, and as good a frame as any for RMS.
const VAD_FRAME: usize = 512;
/// Audio covered by one verdict, so silence is counted in audio time rather than in polls.
const VAD_FRAME_MS: u64 = VAD_FRAME as u64 * 1000 / pond_audio::CAPTURE_RATE_HZ as u64;

/// Judge `fresh` in whole [`VAD_FRAME`] windows, carrying the remainder in `frames`, so the
/// detector hears each sample exactly once. One verdict per window, oldest first.
fn judge_frames(
    frames: &mut Windower,
    detector: &mut dyn SpeechDetector,
    fresh: &[f32],
) -> Vec<bool> {
    let mut verdicts = Vec::new();
    frames.push(fresh, |frame| verdicts.push(detector.is_speech(frame)));
    verdicts
}

/// Ends a capture whose caller went away. Scoped to its own session: by then this detached
/// thread may no longer own the mic.
fn capture_cancelled(mic: &MicHandle, session: u64) -> anyhow::Error {
    tracing::debug!("speech capture cancelled; releasing the microphone");
    mic.close_session(session);
    anyhow!("speech capture cancelled")
}

/// Record until `silence_ms` of silence or `max_record_secs`; no onset wait, unlike the VAD path.
/// `since` starts the clip back in the ring, for speech that began before the call.
/// `stop` cancels within one poll.
pub(crate) fn record_mono_f32_until_silence(
    mic: &MicHandle,
    max_record_secs: u32,
    silence_ms: u64,
    since: Option<std::time::Instant>,
    detector: &mut dyn SpeechDetector,
    stop: &AtomicBool,
) -> Result<(Vec<f32>, u32)> {
    const POLL_MS: u64 = 30;

    // Privacy gate before open, so the OS mic indicator stays dark when the mic is disabled.
    pond_core::models::domain::mic_gate::ensure_mic_enabled()?;
    // This runs detached once its turn is cancelled, so its close is scoped to its own session.
    let session = open_mic_and_confirm(mic)?;

    let sample_rate = pond_audio::CAPTURE_RATE_HZ;
    // Converted at the cursor, not by the caller, so no audio slips by in between.
    let back = since.map_or(0, |t| {
        (t.elapsed().as_millis() * u128::from(sample_rate) / 1000) as usize
    });
    let mut reader = MicReader::starting_back(mic.shared().clone(), back);
    let mut samples: Vec<f32> = Vec::new();
    let mut frames = Windower::new(VAD_FRAME);

    let max_ms = max_record_secs as u64 * 1000;
    let mut elapsed_ms: u64 = 0;
    let mut silent_for: u64 = 0;

    'capture: while elapsed_ms < max_ms {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        if stop.load(Ordering::SeqCst) {
            return Err(capture_cancelled(mic, session));
        }
        elapsed_ms += POLL_MS;
        let fresh = reader.drain();
        samples.extend_from_slice(&fresh);
        if fresh.is_empty() && !mic.state().is_open() {
            // Nothing arrives to judge, so silence would never add up: end, don't wait out the cap.
            tracing::debug!(
                "Continue-record: the microphone stopped delivering; ending the capture"
            );
            break 'capture;
        }

        for speech in judge_frames(&mut frames, detector, &fresh) {
            if speech {
                silent_for = 0;
                continue;
            }
            silent_for += VAD_FRAME_MS;
            if silent_for >= silence_ms {
                tracing::debug!(
                    "Continue-record: end-of-speech after {}ms silence ({}ms total)",
                    silent_for,
                    elapsed_ms
                );
                break 'capture;
            }
        }
    }

    // This capture's claim only: a reply already speaking may hold the mic for barge-in.
    mic.close_session(session);
    Ok((samples, sample_rate))
}

/// Starts a background transcription that gives up once its flag is raised.
pub(crate) type SpeculativeSpawn =
    dyn Fn(Vec<f32>, u32, Arc<AtomicBool>) -> std::thread::JoinHandle<Result<String>> + Send + Sync;

/// A speculative transcription in flight. Dropping it abandons the job, which then frees the
/// engine for the transcription that superseded it instead of making that one queue.
struct SpeculativeJob {
    handle: std::thread::JoinHandle<Result<String>>,
    abandon: StopOnDrop,
}

impl SpeculativeJob {
    fn start(spawn: &SpeculativeSpawn, samples: Vec<f32>, sample_rate: u32) -> Self {
        let abandoned = Arc::new(AtomicBool::new(false));
        let handle = spawn(samples, sample_rate, abandoned.clone());
        Self {
            handle,
            abandon: StopOnDrop(abandoned),
        }
    }

    fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// The transcript, waiting for it if need be; `None` if the job failed.
    fn join(self) -> Option<String> {
        let Self { handle, abandon } = self;
        let transcript = handle.join().ok().and_then(|r| r.ok());
        // Only now: raised any earlier, it would abort the job being waited on.
        drop(abandon);
        transcript
    }
}

/// Wait for onset, record until `silence_ms` of pause or the cap; empty samples = no speech.
/// `speculative_spawn` runs once per silence run; `on_speculative_event` gets Ready/Invalidated.
/// `stop` cancels within one poll.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_mono_f32_vad(
    mic: &MicHandle,
    max_wait_secs: u32,
    max_record_secs: u32,
    silence_ms: u64,
    speculative_spawn: Option<&SpeculativeSpawn>,
    on_speculative_event: Option<&(dyn Fn(SpeculativeSignal) + Send + Sync)>,
    audio_level_sink: Option<&ThrottledAudioLevelSink>,
    detector: &mut dyn SpeechDetector,
    stop: &AtomicBool,
) -> Result<(Vec<f32>, u32, Option<String>)> {
    // Onset only, and energy-based on purpose: a model detector needs a window or two of
    // context, so it under-reports at onset and would clip the first word.
    const SPEECH_RMS: f32 = 0.010;
    const POLL_MS: u64 = 30;

    // Privacy gate before open, so the OS mic indicator stays dark when the mic is disabled.
    pond_core::models::domain::mic_gate::ensure_mic_enabled()?;
    // This runs detached once its turn is cancelled, so its close is scoped to its own session.
    let session = open_mic_and_confirm(mic)?;

    let sample_rate = pond_audio::CAPTURE_RATE_HZ;
    let mut reader = MicReader::new(mic.shared().clone());
    let mut samples: Vec<f32> = Vec::new();

    // ── Wait for speech onset ───────────────────────────────────────────────
    let max_wait_ms = max_wait_secs as u64 * 1000;
    let mut waited_ms: u64 = 0;
    let mut speech_detected = false;

    while waited_ms < max_wait_ms {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        if stop.load(Ordering::SeqCst) {
            return Err(capture_cancelled(mic, session));
        }
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
        mic.close_session(session);
        return Ok((samples, sample_rate, None)); // empty or just noise
    }

    // ── Record until end-of-speech ──────────────────────────────────────────
    let max_record_ms = max_record_secs as u64 * 1000;
    let mut recorded_ms: u64 = 0;
    let mut vad = SpeculativeVad::new(silence_ms, VAD_FRAME_MS);
    let mut frames = Windower::new(VAD_FRAME);
    let mut speculative: Option<SpeculativeJob> = None;
    // Set after `Ready`: reused on `Confirmed`; tells `DiscardSpeculative` to send `Invalidated`.
    let mut speculative_ready: Option<String> = None;
    let mut confirmed = false;

    'capture: while recorded_ms < max_record_ms {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        if stop.load(Ordering::SeqCst) {
            return Err(capture_cancelled(mic, session));
        }
        recorded_ms += POLL_MS;
        let fresh = reader.drain();
        samples.extend_from_slice(&fresh);
        if fresh.is_empty() && !mic.state().is_open() {
            // Nothing arrives to judge, so silence would never add up: end, don't wait out the cap.
            tracing::debug!("VAD record: the microphone stopped delivering; ending the capture");
            break 'capture;
        }

        if let Some(sink) = audio_level_sink {
            if !fresh.is_empty() {
                sink.maybe_emit(rms_energy(&fresh));
            }
        }

        for speech in judge_frames(&mut frames, detector, &fresh) {
            match vad.on_speech(speech) {
                VadEvent::SpawnSpeculative => {
                    if let Some(spawn) = speculative_spawn {
                        speculative =
                            Some(SpeculativeJob::start(spawn, samples.clone(), sample_rate));
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
                    break 'capture;
                }
                VadEvent::None => {}
            }
        }

        // Notify as soon as the job finishes, so the LLM can start before silence is confirmed.
        if speculative_ready.is_none() && speculative.as_ref().is_some_and(|j| j.is_finished()) {
            if let Some(transcript) = speculative.take().and_then(SpeculativeJob::join) {
                speculative_ready = Some(transcript.clone());
                if let Some(cb) = on_speculative_event {
                    cb(SpeculativeSignal::Ready(transcript));
                }
            }
        }
    }

    // A speculative reply may already be speaking and holding the mic for barge-in.
    mic.close_session(session);

    let speculative_transcript = if confirmed {
        speculative_ready.or_else(|| speculative.and_then(SpeculativeJob::join))
    } else {
        // Abandoned before returning, so the full transcription doesn't queue behind it.
        drop(speculative);
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
/// The voice loop replaces `energy_threshold`, `post_trigger_silence_ms` and `cooldown_ms` with
/// the `voice_kws_*` settings, so those three defaults are not what runs there.
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
    /// The configured wake phrase, normalized; always among `triggers`.
    wake_word: String,
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
        let wake_word = normalize_transcript(&raw);
        Self {
            backend,
            triggers: vec![wake_word.clone()],
            wake_word,
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

    /// Also trigger on calibrated mishearings. The wake word itself always stays a trigger, and
    /// a stored variant is reduced to the phrase as heard: older calibrations stored whole
    /// sentences, which fired only when that exact sentence was said again.
    pub fn with_transcriptions(mut self, variants: Vec<String>) -> Self {
        self.triggers = pond_voice::text::wake_triggers(&self.wake_word, &variants);
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

/// Sets its flag on drop, so dropping its owner stops the thread watching the flag.
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
    // Another claim may have kept the device open, so the ring was not cleared: never read
    // back past this session's start, where the last command (wake word and all) still sits.
    let armed_at = mic.shared().written();
    let since_armed = || (mic.shared().written() - armed_at) as usize;

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

        let snapshot = mic.shared().recent(window_samples.min(since_armed()));

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
                // The backend has logged the failure with its cause; the next window retries.
                tracing::debug!("KWS: window not transcribed, retrying: {}", e);
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
            // Spotify's Developer Policy III.7: from the ping on, other audio stays paused, through
            // the words said after it and until the reply's turn holds it (see `quiet`).
            let quiet = pond_core::models::services::voice::quiet::installed();
            let keep_quiet = |span: std::time::Duration| {
                if let Some(quiet) = &quiet {
                    quiet.linger(span);
                }
            };
            keep_quiet(QUIET_WHILE_LISTENING);
            pond_audio_out::play_wake_ping(quiet.clone());

            // Capture until `post_trigger_silence_ms` of silence or the `post_trigger_ms` ceiling.
            let poll_ms = 50u64;
            let mut elapsed_ms = 0u64;
            let mut silent_for_ms = 0u64;

            while elapsed_ms < config.post_trigger_ms {
                if !sleep_unless_stopped(poll_ms, &stop) {
                    mic.close_session(session);
                    return Err(anyhow!("wake-word detection cancelled"));
                }
                elapsed_ms += poll_ms;
                if elapsed_ms.is_multiple_of(500) {
                    keep_quiet(QUIET_WHILE_LISTENING);
                }

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
            let command_audio = mic.shared().recent(captured_samples.min(since_armed()));
            tracing::debug!(
                "KWS: captured {}ms ({}ms lookback + {}ms after the trigger)",
                captured_ms,
                config.lookback_ms,
                elapsed_ms
            );

            mic.close_session(session);
            // Transcribing comes next, then the turn; a false wake has no turn, and this runs out.
            keep_quiet(QUIET_UNTIL_THE_TURN);

            let cmd_wav = encode_wav_mono_16k(&command_audio);

            return Ok(WakeWordActivation {
                captured_audio: Some(cmd_wav),
                ..Default::default()
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
    // Code defaults only; the voice loop overrides three of them from `voice_kws_*` settings.

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

    /// Code default only: the voice loop uses `voice_kws_cooldown_ms`, which this does not bound.
    /// Follow-ups are heard by the conversational listen, not here; this delays the next wake.
    #[test]
    fn the_code_default_cooldown_re_arms_within_a_second() {
        let c = KeywordDetectorConfig::default();
        assert!(
            c.cooldown_ms <= 1000,
            "cooldown {}ms is dead time in which the wake word does nothing",
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
    fn a_calibrated_sentence_triggers_on_the_phrase_not_the_whole_sentence() {
        let d = WhisperKeywordDetector::new(Arc::new(DeafBackend), "jarvis", test_mic())
            .with_transcriptions(vec![
                "ok jarvis what time is it".into(),
                "hey jarv good morning".into(),
            ]);
        let t = d.triggers();
        assert!(
            !t.iter().any(|x| x.split(' ').count() > 2),
            "no sentence survives: {t:?}"
        );
        assert!(
            t.iter().any(|x| x == "jarv"),
            "the mishearing is kept: {t:?}"
        );
    }

    #[test]
    fn the_wake_word_stays_a_trigger_whatever_calibration_stored() {
        for variants in [
            vec![],
            vec!["gus".to_string()],
            vec!["!!!".to_string(), "".to_string()],
        ] {
            let d = WhisperKeywordDetector::new(Arc::new(DeafBackend), "Hey Goose", test_mic())
                .with_transcriptions(variants.clone());
            assert_eq!(d.triggers()[0], "hey goose", "{variants:?}");
            assert!(
                !d.triggers().iter().any(|t| t.starts_with("say")),
                "{:?}",
                d.triggers()
            );
        }
    }

    #[test]
    fn goose_wake_triggers_are_exactly_hey_goose_and_goose() {
        let d = WhisperKeywordDetector::new(Arc::new(DeafBackend), "goose", test_mic())
            .with_transcriptions(vec!["goose".into(), "Hey, Goose.".into()]);
        let t = d.triggers();
        assert_eq!(t, vec!["hey goose", "goose"]);
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

    // ── Speech capture cancellation ─────────────────────────────────────────

    #[test]
    fn a_cancelled_capture_ends_within_a_poll_and_releases_the_mic() {
        let (mic, _join) = pond_audio::spawn(
            Box::new(pond_audio::testing::ScriptedCapture::silence(30_000, 20)),
            pond_audio::CAPTURE_RATE_HZ,
            15_000,
            true,
        );
        let stop = Arc::new(AtomicBool::new(false));
        let capture = std::thread::spawn({
            let mic = mic.clone();
            let stop = stop.clone();
            move || {
                let mut detector = RmsDetector::new(0.005);
                let t0 = std::time::Instant::now();
                // Room tone, so without the flag this serves out the whole 10 s onset wait.
                let result =
                    record_mono_f32_vad(&mic, 10, 10, 200, None, None, None, &mut detector, &stop);
                (result, t0.elapsed())
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(150));
        stop.store(true, Ordering::SeqCst);

        let (result, elapsed) = capture.join().expect("capture thread must not panic");
        let err = result.expect_err("a cancelled capture must not report a recording");
        assert!(err.to_string().contains("cancelled"), "{err}");
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "took {elapsed:?}; cancellation should land within one 30 ms poll"
        );
        assert!(
            mic.wait_for(|s| !s.is_open(), std::time::Duration::from_secs(2)),
            "the cancelled capture must release the microphone"
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
            record_mono_f32_vad(
                &mic,
                1,
                1,
                200,
                None,
                None,
                None,
                &mut detector,
                &AtomicBool::new(false),
            )
        })
        .await
        .expect("capture thread must not panic");

        assert!(
            result.is_ok(),
            "the follow-up capture must not fail from a device race: {:?}",
            result.err()
        );
    }

    // ── Session-scoped closes ────────────────────────────────────────────────

    use pond_audio::testing::ScriptedCapture;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Hears `reply` in every window, counting the calls.
    struct Hears {
        reply: &'static str,
        calls: Arc<AtomicUsize>,
    }

    impl WhisperBackend for Hears {
        fn transcribe_pcm_blocking(&self, _: &[f32]) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.reply.to_string())
        }
    }

    fn scripted_mic(device: ScriptedCapture) -> MicHandle {
        let (mic, _join) =
            pond_audio::spawn(Box::new(device), pond_audio::CAPTURE_RATE_HZ, 15_000, true);
        mic
    }

    /// A 16 kHz tone `ms` long, well above every speech threshold here.
    fn tone(ms: u64) -> Vec<f32> {
        (0..16 * ms as usize)
            .map(|i| (i as f32 * 0.05).sin() * 0.5)
            .collect()
    }

    fn quick_config() -> KeywordDetectorConfig {
        KeywordDetectorConfig {
            cooldown_ms: 0,
            window_ms: 200,
            slide_ms: 20,
            lookback_ms: 100,
            post_trigger_silence_ms: 0,
            ..KeywordDetectorConfig::default()
        }
    }

    /// Runs the detector on its own thread, as `spawn_blocking` does, hearing "goose".
    fn run_detector(
        mic: &MicHandle,
        config: KeywordDetectorConfig,
        calls: &Arc<AtomicUsize>,
        stop: &Arc<AtomicBool>,
    ) -> std::thread::JoinHandle<Result<WakeWordActivation>> {
        let backend = Arc::new(Hears {
            reply: "goose",
            calls: calls.clone(),
        });
        let (mic, stop) = (mic.clone(), stop.clone());
        std::thread::spawn(move || {
            detection_loop(backend, vec!["goose".into()], config, stop, None, mic)
        })
    }

    fn wait_until(what: &str, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Gives a close already sent time to land, then requires `session` to have survived it.
    fn assert_still_open_then_close(mic: &MicHandle, session: u64) {
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            mic.state().is_open(),
            "a stale close shut the device under a newer session"
        );
        // Vacuity control: the session that owns the device can still close it.
        mic.close_session(session);
        assert!(mic.wait_for(|s| *s == MicState::Closed, Duration::from_secs(2)));
    }

    #[test]
    fn a_detector_cancelled_mid_capture_spares_a_newer_session() {
        let mic = scripted_mic(ScriptedCapture::new(tone(10_000), 20));
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let config = KeywordDetectorConfig {
            post_trigger_ms: 10_000,
            ..quick_config()
        };
        let detector = run_detector(&mic, config, &calls, &stop);

        // Heard: from here on the detector is in its post-trigger capture.
        wait_until("the wake word", || calls.load(Ordering::SeqCst) > 0);
        let follow_up = mic.open_session();
        stop.store(true, Ordering::SeqCst);
        let outcome = detector.join().expect("the detector must not panic");
        assert!(outcome.is_err(), "it was cancelled");

        assert_still_open_then_close(&mic, follow_up);
    }

    #[test]
    fn a_detector_finishing_its_capture_spares_a_newer_session() {
        let mic = scripted_mic(ScriptedCapture::new(tone(10_000), 20));
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let config = KeywordDetectorConfig {
            post_trigger_ms: 400,
            ..quick_config()
        };
        let detector = run_detector(&mic, config, &calls, &stop);

        wait_until("the wake word", || calls.load(Ordering::SeqCst) > 0);
        let follow_up = mic.open_session();
        let activation = detector
            .join()
            .expect("the detector must not panic")
            .expect("the wake word fires");
        assert!(activation.captured_audio.is_some());

        assert_still_open_then_close(&mic, follow_up);
    }

    /// A capture keeps running after its turn is cancelled; its close must not reach the
    /// session the next turn opened meanwhile.
    fn assert_a_late_capture_spares_the_next_session(
        capture: impl FnOnce(&MicHandle) + Send + 'static,
    ) {
        let mic = scripted_mic(ScriptedCapture::new(tone(10_000), 20));
        let running = {
            let mic = mic.clone();
            std::thread::spawn(move || capture(&mic))
        };
        // Open means the capture has claimed its generation, so the next claim is newer.
        assert!(mic.wait_for(|s| s.is_open(), Duration::from_secs(2)));
        let next = mic.open_session();
        running.join().expect("the capture must not panic");

        assert_still_open_then_close(&mic, next);
    }

    #[test]
    fn a_late_vad_capture_spares_the_next_session() {
        assert_a_late_capture_spares_the_next_session(|mic| {
            let mut detector = RmsDetector::new(0.005);
            record_mono_f32_vad(
                mic,
                1,
                1,
                200,
                None,
                None,
                None,
                &mut detector,
                &AtomicBool::new(false),
            )
            .expect("captured");
        });
    }

    #[test]
    fn a_late_continued_capture_spares_the_next_session() {
        assert_a_late_capture_spares_the_next_session(|mic| {
            let mut detector = RmsDetector::new(0.005);
            record_mono_f32_until_silence(
                mic,
                1,
                200,
                None,
                &mut detector,
                &AtomicBool::new(false),
            )
            .expect("captured");
        });
    }

    // ── What the speech detector is fed ──────────────────────────────────────

    /// Keeps every frame it judges; speech is energy, as for `RmsDetector`.
    #[derive(Default)]
    struct Judged {
        frames: Vec<Vec<f32>>,
    }

    impl SpeechDetector for Judged {
        fn is_speech(&mut self, frame: &[f32]) -> bool {
            self.frames.push(frame.to_vec());
            rms_energy(frame) >= 0.005
        }
    }

    #[test]
    fn the_speech_detector_hears_every_sample_once_in_whole_windows() {
        // Every sample distinct, so a repeat or a gap shows; 20 ms slices polled every 30 ms
        // never line up with a 512-sample window.
        let speech: Vec<f32> = (0..16_000).map(|i| 0.1 + i as f32 * 1e-6).collect();
        let mic = scripted_mic(ScriptedCapture::new(speech, 20));
        let mut judged = Judged::default();
        let (samples, _, _) = record_mono_f32_vad(
            &mic,
            1,
            5,
            300,
            None,
            None,
            None,
            &mut judged,
            &AtomicBool::new(false),
        )
        .expect("captured");

        assert!(!judged.frames.is_empty());
        for frame in &judged.frames {
            assert_eq!(frame.len(), VAD_FRAME, "Silero needs exact windows");
        }
        let heard = judged.frames.concat();
        let start = samples
            .iter()
            .position(|s| *s == heard[0])
            .expect("the first judged sample is in the capture");
        assert_eq!(
            Some(&heard[..]),
            samples.get(start..start + heard.len()),
            "the detector must hear the capture in order, each sample once"
        );
        assert!(
            samples.len() - (start + heard.len()) < VAD_FRAME,
            "only the remainder short of a window may go unjudged"
        );
    }

    #[test]
    fn end_of_speech_waits_for_silence_ms_of_audio_not_of_polls() {
        // 20 ms of audio every 60 ms: a device delivering at a third of real time.
        let mic = scripted_mic(ScriptedCapture::new(tone(300), 20).paced(60));
        let silence_ms = 300;
        let mut detector = RmsDetector::new(0.005);
        let (samples, _, _) = record_mono_f32_vad(
            &mic,
            2,
            10,
            silence_ms,
            None,
            None,
            None,
            &mut detector,
            &AtomicBool::new(false),
        )
        .expect("captured");

        let trailing_silence = samples.iter().rev().take_while(|s| **s == 0.0).count();
        let needed = silence_ms as usize * 16;
        assert!(
            trailing_silence + VAD_FRAME > needed,
            "ended after {trailing_silence} silent samples; {silence_ms} ms is {needed}"
        );
    }

    // ── Abandoned speculative jobs ───────────────────────────────────────────

    /// Stands in for the engine: a job holds `slot` while it decodes, until its flag is raised
    /// or the decode runs its course, as a whisper decode with an abort hook does.
    fn slot_holding_spawn(
        slot: &Arc<Mutex<()>>,
        started: &Arc<AtomicUsize>,
    ) -> Box<SpeculativeSpawn> {
        const DECODE: Duration = Duration::from_secs(10);
        let (slot, started) = (slot.clone(), started.clone());
        Box::new(move |_samples, _rate, abandoned| {
            let (slot, started) = (slot.clone(), started.clone());
            std::thread::spawn(move || {
                let _held = slot.lock().unwrap();
                started.fetch_add(1, Ordering::SeqCst);
                let t0 = Instant::now();
                while t0.elapsed() < DECODE {
                    if abandoned.load(Ordering::SeqCst) {
                        return Err(anyhow!("abandoned"));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok("a guess".to_string())
            })
        })
    }

    #[test]
    fn an_abandoned_speculative_job_does_not_hold_up_the_full_transcription() {
        let mut resumed = tone(300);
        resumed.extend(vec![0.0; 16 * 150]);
        resumed.extend(tone(3_000));
        let mut capped = tone(300);
        capped.extend(vec![0.0; 16 * 3_000]);

        // Speech resumes after a short pause; or the cap ends the capture mid-pause.
        for (case, script, silence_ms) in [
            ("speech resumed", resumed, 400),
            ("capped mid-pause", capped, 5_000),
        ] {
            let slot = Arc::new(Mutex::new(()));
            let started = Arc::new(AtomicUsize::new(0));
            let spawn = slot_holding_spawn(&slot, &started);
            let mic = scripted_mic(ScriptedCapture::new(script, 20));
            let mut detector = RmsDetector::new(0.005);

            let (_, _, transcript) = record_mono_f32_vad(
                &mic,
                1,
                1,
                silence_ms,
                Some(&*spawn),
                None,
                None,
                &mut detector,
                &AtomicBool::new(false),
            )
            .expect("captured");
            assert!(transcript.is_none(), "{case}: never confirmed");
            assert!(
                started.load(Ordering::SeqCst) > 0,
                "{case}: the pause must have started a speculative job"
            );

            // The full transcription comes next and needs the same slot.
            let deadline = Instant::now() + Duration::from_secs(1);
            while slot.try_lock().is_err() {
                assert!(
                    Instant::now() < deadline,
                    "{case}: the full transcription queued behind an abandoned job"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    // ── Stale audio ──────────────────────────────────────────────────────────

    #[test]
    fn a_reopened_detector_never_hears_the_last_session() {
        let mic = scripted_mic(ScriptedCapture::silence(0, 20));
        // The last session's tail, wake word and all, still buffered while the mic was closed.
        mic.shared().ring.lock().unwrap().push(&tone(1_000));
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let detector = run_detector(&mic, quick_config(), &calls, &stop);

        assert!(mic.wait_for(|s| s.is_open(), Duration::from_secs(2)));
        // Twenty slides of room tone.
        std::thread::sleep(Duration::from_millis(400));
        stop.store(true, Ordering::SeqCst);
        assert!(detector.join().expect("no panic").is_err(), "cancelled");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the detector transcribed audio from before it opened"
        );
    }

    #[test]
    fn a_detector_never_hears_audio_from_before_its_session_while_the_mic_stays_open() {
        let mic = scripted_mic(ScriptedCapture::silence(0, 20));
        // Another claim (a reply's barge-in hold) keeps the device open, so nothing clears
        // the ring between the last command and this detector.
        let hold = mic.open_session();
        assert!(mic.wait_for(|s| s.is_open(), Duration::from_secs(2)));
        mic.shared().ring.lock().unwrap().push(&tone(1_000));
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let detector = run_detector(&mic, quick_config(), &calls, &stop);

        std::thread::sleep(Duration::from_millis(400));
        stop.store(true, Ordering::SeqCst);
        assert!(detector.join().expect("no panic").is_err(), "cancelled");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the detector transcribed the last session's audio"
        );
        assert!(
            mic.state().is_open(),
            "the hold's claim must outlive the detector's"
        );
        mic.close_session(hold);
    }

    /// Hears speech in every frame, so silence alone would never end a capture.
    struct AlwaysSpeech;

    impl SpeechDetector for AlwaysSpeech {
        fn is_speech(&mut self, _frame: &[f32]) -> bool {
            true
        }
    }

    #[test]
    fn a_capture_ends_when_the_microphone_stops_delivering() {
        let mic = scripted_mic(ScriptedCapture::silence(0, 20));
        let closer = mic.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            closer.close();
        });
        let started = Instant::now();
        let _ = record_mono_f32_until_silence(
            &mic,
            30,
            200,
            None,
            &mut AlwaysSpeech,
            &AtomicBool::new(false),
        )
        .expect("captured");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the capture waited for its cap after the device stopped ({:?})",
            started.elapsed()
        );
    }

    // ── Barge-in: the follow-up capture starts where the user started talking ──

    /// Each speech sample encodes its own position in the script, so a clip shows where it began.
    fn numbered_speech(ms: u64) -> Vec<f32> {
        (0..16 * ms as usize)
            .map(|i| 0.25 + i as f32 * 1e-5)
            .collect()
    }

    fn position(sample: f32) -> Option<u64> {
        (sample != 0.0).then(|| ((sample - 0.25) / 1e-5).round() as u64)
    }

    #[test]
    fn a_barge_in_capture_starts_at_the_onset_without_a_gap() {
        let (mic, _join) = pond_audio::spawn(
            Box::new(pond_audio::testing::ScriptedCapture::new(
                numbered_speech(1_500),
                20,
            )),
            pond_audio::CAPTURE_RATE_HZ,
            15_000,
            true,
        );
        // The reply's barge-in hold keeps the mic open from before the onset.
        let hold = mic.open_session();
        assert!(mic.wait_for(|s| s.is_open(), std::time::Duration::from_secs(2)));
        std::thread::sleep(std::time::Duration::from_millis(100));

        let onset = std::time::Instant::now();
        let at_onset = mic.shared().written();
        // The debounce, and the turn being torn down, before the follow-up capture starts.
        std::thread::sleep(std::time::Duration::from_millis(400));
        let at_capture = mic.shared().written();

        let mut detector = RmsDetector::new(0.005);
        let (clip, rate) = record_mono_f32_until_silence(
            &mic,
            5,
            200,
            Some(onset),
            &mut detector,
            &AtomicBool::new(false),
        )
        .unwrap();
        mic.close_session(hold);
        assert_eq!(rate, pond_audio::CAPTURE_RATE_HZ);

        let heard: Vec<u64> = clip.iter().filter_map(|s| position(*s)).collect();
        let first = *heard.first().expect("the capture heard nothing");
        assert!(
            first < at_capture,
            "the words said before the capture started are missing: clip opens at {first}, \
             capture began at {at_capture}"
        );
        // Loose bounds: the scripted device runs a little slower than the clock.
        assert!(
            first <= at_onset + 800 && first + 3_200 >= at_onset,
            "the clip should open at the onset ({at_onset}); it opens at {first}"
        );
        assert!(
            heard.windows(2).all(|w| w[1] == w[0] + 1),
            "the pre-roll and the live audio must join without a hole"
        );
        assert_eq!(
            *heard.last().unwrap() + 1,
            16 * 1_500,
            "the capture must run to the end of the speech"
        );
    }
}
