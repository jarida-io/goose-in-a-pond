//! Speaker-side audio for every GIAP TTS engine: output device, playback and the working tone.

use anyhow::{anyhow, Context, Result};
use pond_core::shared::domain::agent::ThrottledAudioLevelSink;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub mod bus;
pub mod route;
pub use bus::AudioBus;
pub use route::{output_device, set_output_device};

// ── Working tone ──────────────────────────────────────────────────────────────

const TONE_RATE: u32 = 22_050;
/// One breath of the tone: a soft chime, then silence, then repeat.
const TONE_CYCLE_MS: u64 = 2_600;
/// How long the chime itself rings before the silence.
const TONE_CHIME_MS: u64 = 1_100;

/// `thinking_for` value meaning no tone; generations start at 1, so it never names a turn.
pub const TONE_OFF: u64 = 0;

/// One tone cycle: a soft major-sixth chime (E5 over G4), then mostly silence.
fn working_tone_cycle() -> Vec<f32> {
    let chime_samples = (TONE_RATE as u64 * TONE_CHIME_MS / 1000) as usize;
    let cycle_samples = (TONE_RATE as u64 * TONE_CYCLE_MS / 1000) as usize;
    let mut out = Vec::with_capacity(cycle_samples);

    for i in 0..chime_samples {
        let t = i as f32 / TONE_RATE as f32;
        let progress = i as f32 / chime_samples as f32;

        // Fade in over the first 8% so the chime never clicks, then decay.
        let attack = (progress / 0.08).min(1.0);
        let decay = (-3.2 * progress).exp();
        let envelope = attack * decay;

        let root = (2.0 * std::f32::consts::PI * 392.00 * t).sin(); // G4
        let sixth = (2.0 * std::f32::consts::PI * 659.25 * t).sin(); // E5
        out.push((root * 0.6 + sixth * 0.4) * envelope * 0.5);
    }

    out.resize(cycle_samples, 0.0);
    out
}

/// Spawn the working-tone thread for turn `mine`; it exits once `thinking_for` names another.
/// A generation, not a bool: two live turns' start/stop calls would interleave.
pub fn start_thinking_tone_thread(thinking_for: Arc<AtomicU64>, mine: u64) {
    std::thread::spawn(move || {
        use rodio::Sink;

        // On a failed start, clear the claim only if a newer turn hasn't taken it.
        let release = |thinking_for: &AtomicU64| {
            let _ =
                thinking_for.compare_exchange(mine, TONE_OFF, Ordering::SeqCst, Ordering::SeqCst);
        };

        let Ok((_stream, handle, _)) = route::open_routed() else {
            release(&thinking_for);
            return;
        };
        let Ok(sink) = Sink::try_new(&handle) else {
            release(&thinking_for);
            return;
        };
        sink.set_volume(0.10);

        let cycle = working_tone_cycle();
        let polls_per_cycle = TONE_CYCLE_MS / 50;
        let ours = |thinking_for: &AtomicU64| thinking_for.load(Ordering::Relaxed) == mine;

        while ours(&thinking_for) {
            sink.append(rodio::buffer::SamplesBuffer::new(
                1,
                TONE_RATE,
                cycle.clone(),
            ));
            for _ in 0..polls_per_cycle {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if !ours(&thinking_for) {
                    sink.stop();
                    return;
                }
            }
        }
        sink.stop();
    });
}

// ── Persistent audio output ───────────────────────────────────────────────────

/// `rodio::OutputStream` is `!Send` because of cpal's CoreAudio property listeners.
#[allow(dead_code)] // kept alive for its Drop (closes the audio device); never read
struct SendableStream(rodio::OutputStream);
// SAFETY: moved into the keeper thread once and only ever touched there.
unsafe impl Send for SendableStream {}

/// Keeps the `!Send` output stream alive on its own thread and exposes its `Send` handle.
/// Reusing one avoids macOS CoreAudio AudioUnit open/close churn, which degrades audio over turns.
/// Opens the routed speaker; for one held across a route change, use [`Speaker`].
pub struct AudioKeeper {
    pub handle: rodio::OutputStreamHandle,
    stop: Arc<AtomicBool>,
}

impl AudioKeeper {
    /// Open the routed output device on a keeper thread; `thread_name` should name the engine.
    pub fn try_new(thread_name: &str) -> Result<Self> {
        let (stream, handle, opened) = route::open_routed()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        let sendable = SendableStream(stream);
        std::thread::Builder::new()
            .name(thread_name.to_string())
            .spawn(move || {
                let _stream = sendable; // keep OutputStream alive on this thread
                let _hold = route::Hold::new(opened);
                while !stop_clone.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            })
            .context("audio keeper thread spawn failed")?;
        Ok(Self { handle, stop })
    }
}

impl Drop for AudioKeeper {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// How often a [`Speaker`]'s thread looks for a route change.
const FOLLOW_POLL: Duration = Duration::from_millis(200);

struct SpeakerState {
    handle: rodio::OutputStreamHandle,
    /// The route generation the stream was opened for.
    generation: u64,
    device: String,
}

/// The routed speaker, held open across utterances and reopened when the route moves, so
/// connecting headphones takes effect on the next sentence. Cheap to clone.
#[derive(Clone)]
pub struct Speaker {
    state: Arc<Mutex<SpeakerState>>,
    _stop: Arc<StopOnDrop>,
}

struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Speaker {
    /// Fails only when no output device opens at all.
    pub fn try_new(thread_name: &str) -> Result<Self> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<Arc<Mutex<SpeakerState>>>>();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        std::thread::Builder::new()
            .name(thread_name.to_string())
            .spawn(move || {
                // Opened here, not on the caller's thread: a CoreAudio stream must close on the
                // thread that opened it, and this thread replaces it.
                let generation = route::generation();
                let (stream, handle, device) = match route::open_routed() {
                    Ok(opened) => opened,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let mut stream = Some(SendableStream(stream));
                let mut hold = Some(route::Hold::new(device.clone()));
                let state = Arc::new(Mutex::new(SpeakerState {
                    handle,
                    generation,
                    device,
                }));
                let _ = ready_tx.send(Ok(state.clone()));
                while !stopping.load(Ordering::Relaxed) {
                    std::thread::sleep(FOLLOW_POLL);
                    let wanted = route::generation();
                    if state.lock().unwrap_or_else(|e| e.into_inner()).generation == wanted {
                        continue;
                    }
                    // Release the old PCM before opening the new one so ALSA never sees two
                    // streams on the same card at once.
                    drop((stream.take(), hold.take()));
                    match route::open_routed() {
                        Ok((next, handle, device)) => {
                            tracing::info!(device = %device, "speaker reopened on the new route");
                            stream = Some(SendableStream(next));
                            hold = Some(route::Hold::new(device.clone()));
                            *state.lock().unwrap_or_else(|e| e.into_inner()) = SpeakerState {
                                handle,
                                generation: wanted,
                                device,
                            };
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "no speaker opened on the new route");
                            state.lock().unwrap_or_else(|e| e.into_inner()).generation = wanted;
                        }
                    }
                }
                drop((stream, hold));
            })
            .context("speaker thread spawn failed")?;
        let state = ready_rx
            .recv()
            .map_err(|_| anyhow!("speaker thread exited before opening"))??;
        Ok(Self {
            state,
            _stop: Arc::new(StopOnDrop(stop)),
        })
    }

    pub fn handle(&self) -> rodio::OutputStreamHandle {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .handle
            .clone()
    }

    /// Changes whenever the stream is replaced; audio queued on the old one will never finish.
    pub fn generation(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .generation
    }

    /// The device this speaker has open.
    pub fn device(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .device
            .clone()
    }
}

// ── Playback ──────────────────────────────────────────────────────────────────

/// Per-`window_ms` RMS of an `encode_wav_pcm16` buffer (44-byte header, 16-bit LE mono).
/// rodio has no per-sample hook after `append()`, so the caller replays one value per poll tick.
fn compute_audio_envelope(wav: &[u8], window_ms: u64) -> Vec<f32> {
    const HEADER_LEN: usize = 44;
    if wav.len() <= HEADER_LEN {
        return Vec::new();
    }
    let sample_rate = u32::from_le_bytes(wav[24..28].try_into().unwrap_or([0x56, 0x22, 0, 0]));
    let pcm = &wav[HEADER_LEN..];
    let samples_per_window = ((sample_rate as u64 * window_ms) / 1000).max(1) as usize;
    let bytes_per_window = samples_per_window * 2;

    pcm.chunks(bytes_per_window)
        .map(|chunk| {
            let mut sum_sq = 0f32;
            let mut n = 0usize;
            for pair in chunk.chunks_exact(2) {
                let s = i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0;
                sum_sq += s * s;
                n += 1;
            }
            if n > 0 {
                (sum_sq / n as f32).sqrt()
            } else {
                0.0
            }
        })
        .collect()
}

/// No progress for this long while audio is queued means the device stopped taking it. Generous,
/// because a Bluetooth speaker can take a second to wake.
const STALL_AFTER: Duration = Duration::from_secs(3);

/// Interruptibly play WAV on an open handle; `audio_level_sink` gets one level per poll tick.
pub fn play_wav(
    wav: Vec<u8>,
    handle: &rodio::OutputStreamHandle,
    interrupted: &AtomicBool,
    utterance: &AtomicU64,
    audio_level_sink: Option<&ThrottledAudioLevelSink>,
) -> Result<()> {
    play(
        wav,
        handle,
        || true,
        interrupted,
        utterance,
        audio_level_sink,
    )
}

/// [`play_wav`] on a [`Speaker`]. Stops when the speaker reopens elsewhere: what was queued on
/// the old stream would never finish.
pub fn play_wav_on(
    wav: Vec<u8>,
    speaker: &Speaker,
    interrupted: &AtomicBool,
    utterance: &AtomicU64,
    audio_level_sink: Option<&ThrottledAudioLevelSink>,
) -> Result<()> {
    let generation = speaker.generation();
    let handle = speaker.handle();
    play(
        wav,
        &handle,
        || speaker.generation() == generation,
        interrupted,
        utterance,
        audio_level_sink,
    )
}

/// USB DACs ramp analog output over ~150 ms from idle squelch; leading silence keeps speech from clipping.
static LAST_PLAYBACK_AT: Mutex<Option<Instant>> = Mutex::new(None);
const HARDWARE_WAKE_PREROLL_MS: u64 = 300;
const HARDWARE_IDLE_GAP_MS: u64 = 500;

fn play(
    wav: Vec<u8>,
    handle: &rodio::OutputStreamHandle,
    still_current: impl Fn() -> bool,
    interrupted: &AtomicBool,
    utterance: &AtomicU64,
    audio_level_sink: Option<&ThrottledAudioLevelSink>,
) -> Result<()> {
    use rodio::{Decoder, Sink, Source};
    use std::io::Cursor;

    // Which turn this audio belongs to, fixed at the moment playback starts.
    let mine = utterance.load(Ordering::SeqCst);

    const POLL_MS: u64 = 50;

    let cursor = Cursor::new(wav.clone());
    let decoder = Decoder::new(cursor).context("Failed to decode WAV for playback")?;
    let sink = Sink::try_new(handle).context("Failed to create audio sink")?;

    let needs_preroll = {
        let mut last = LAST_PLAYBACK_AT.lock().unwrap_or_else(|e| e.into_inner());
        let wake = match *last {
            Some(t) => t.elapsed() >= Duration::from_millis(HARDWARE_IDLE_GAP_MS),
            None => true,
        };
        *last = Some(Instant::now());
        wake
    };

    let envelope = audio_level_sink.map(|_| {
        let mut env = compute_audio_envelope(&wav, POLL_MS);
        if needs_preroll {
            let pad_ticks = (HARDWARE_WAKE_PREROLL_MS / POLL_MS) as usize;
            let mut padded = vec![0.0f32; pad_ticks];
            padded.append(&mut env);
            padded
        } else {
            env
        }
    });

    if needs_preroll {
        let sample_rate = decoder.sample_rate();
        let channels = decoder.channels();
        let samples =
            (sample_rate as usize * channels as usize * HARDWARE_WAKE_PREROLL_MS as usize) / 1000;
        let silence =
            rodio::buffer::SamplesBuffer::new(channels, sample_rate, vec![0.0f32; samples]);
        sink.append(silence);
    }
    sink.append(decoder);

    let mut tick: usize = 0;
    let mut position = sink.get_pos();
    let mut moved_at = Instant::now();
    while !sink.empty() {
        if interrupted.load(Ordering::Relaxed) {
            sink.stop();
            tracing::debug!("TTS playback interrupted by barge-in");
            return Ok(());
        }
        // A newer turn began. The interrupt flag can't cover this: `begin_utterance` clears it,
        // wiping a cancel raised within the poll window; a generation can't be cleared.
        if utterance.load(Ordering::Relaxed) != mine {
            sink.stop();
            tracing::debug!("TTS playback dropped: it belongs to a superseded turn");
            return Ok(());
        }
        if !still_current() {
            sink.stop();
            tracing::info!("TTS playback cut: the speaker changed mid-sentence");
            return Ok(());
        }
        let now = sink.get_pos();
        if now != position {
            position = now;
            moved_at = Instant::now();
        } else if moved_at.elapsed() >= STALL_AFTER {
            sink.stop();
            route::report_stall(route::output_device());
            return Err(anyhow!("the speaker stopped taking audio"));
        }
        if let (Some(level_sink), Some(env)) = (audio_level_sink, &envelope) {
            if let Some(&level) = env.get(tick) {
                level_sink.maybe_emit(level);
            }
        }
        tick += 1;
        if let Ok(mut last) = LAST_PLAYBACK_AT.lock() {
            *last = Some(Instant::now());
        }
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
    }

    if let Ok(mut last) = LAST_PLAYBACK_AT.lock() {
        *last = Some(Instant::now());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn wav_header_is_correct() {
        // 2 bytes of PCM (one 16-bit sample at 22050 Hz mono)
        let pcm = vec![0x01u8, 0x00u8];
        let wav = pond_voice::dsp::encode_wav_pcm16(&pcm, 22_050);

        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_len as usize, pcm.len());
        assert_eq!(&wav[44..], pcm.as_slice());
    }

    #[test]
    fn f32_to_pcm_bytes_clamps_and_encodes_le() {
        // -2.0 clamps to -1.0, which maps to -i16::MAX (symmetric), not i16::MIN.
        let bytes = pond_voice::dsp::f32_to_pcm16(&[0.0, 1.0, -2.0]);
        assert_eq!(bytes.len(), 6);
        assert_eq!(i16::from_le_bytes([bytes[0], bytes[1]]), 0);
        assert_eq!(i16::from_le_bytes([bytes[2], bytes[3]]), i16::MAX);
        assert_eq!(i16::from_le_bytes([bytes[4], bytes[5]]), -i16::MAX);
    }
}

#[cfg(test)]
mod working_tone_tests {
    use super::*;

    /// Mostly silence, so a thirty-second answer is bearable to sit through.
    #[test]
    fn the_tone_rests_for_most_of_its_cycle() {
        let cycle = working_tone_cycle();
        let silent = cycle.iter().filter(|s| **s == 0.0).count();
        let ratio = silent as f32 / cycle.len() as f32;
        assert!(
            ratio > 0.5,
            "only {:.0}% silence — a tone that never rests is an alarm",
            ratio * 100.0
        );
    }

    #[test]
    fn the_chime_fades_in_rather_than_clicking() {
        let cycle = working_tone_cycle();
        assert_eq!(cycle[0], 0.0, "must start from silence");

        let attack = (TONE_RATE as usize) / 100; // first 10 ms
        let early = cycle[..attack].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let peak = cycle.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            early < peak * 0.8,
            "amplitude jumps to {early} within 10ms of a {peak} peak"
        );
    }

    #[test]
    fn the_chime_decays_and_finishes_inside_its_cycle() {
        let cycle = working_tone_cycle();
        let chime = (TONE_RATE as u64 * TONE_CHIME_MS / 1000) as usize;

        let loudest = |r: &[f32]| r.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let first_third = loudest(&cycle[..chime / 3]);
        let last_third = loudest(&cycle[chime * 2 / 3..chime]);
        assert!(
            last_third < first_third * 0.5,
            "no decay: {first_third} then {last_third}"
        );

        assert!(
            cycle[chime..].iter().all(|s| *s == 0.0),
            "the rest of the cycle must be true silence"
        );
    }

    /// The playback sink adds its own gain on top, so this needs headroom.
    #[test]
    fn the_tone_never_clips() {
        let peak = working_tone_cycle()
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak <= 1.0, "clipped at {peak}");
        assert!(peak < 0.85, "no headroom for the sink's gain: {peak}");
        assert!(peak > 0.05, "inaudible at {peak}");
    }

    /// The 50 ms stop poll must divide the cycle, or stopping waits out an extra cycle.
    #[test]
    fn the_tone_can_be_stopped_promptly() {
        assert_eq!(
            TONE_CYCLE_MS % 50,
            0,
            "the 50ms stop poll must divide the cycle"
        );
        assert!(
            TONE_CYCLE_MS / 50 >= 4,
            "too few polls per cycle to stop responsively"
        );
    }

    #[test]
    fn the_chime_is_shorter_than_the_rest_that_follows_it() {
        assert!(
            TONE_CHIME_MS < TONE_CYCLE_MS - TONE_CHIME_MS,
            "{}ms of chime against {}ms of silence",
            TONE_CHIME_MS,
            TONE_CYCLE_MS - TONE_CHIME_MS
        );
    }
}

/// Turn-generation rules, tested on the atomics alone: both stop decisions depend only on them.
#[cfg(test)]
mod utterance_generation_tests {
    use super::*;

    /// Mirrors `play_wav`'s decision on each 50 ms poll.
    fn should_keep_playing(interrupted: &AtomicBool, utterance: &AtomicU64, mine: u64) -> bool {
        !interrupted.load(Ordering::Relaxed) && utterance.load(Ordering::Relaxed) == mine
    }

    /// `begin_utterance` clears the flag; only the generation keeps the cancelled audio dead.
    #[test]
    fn cancelled_audio_stays_cancelled_when_the_next_turn_begins() {
        let interrupted = AtomicBool::new(false);
        let utterance = AtomicU64::new(1);

        let speculative = utterance.load(Ordering::SeqCst);
        assert!(should_keep_playing(&interrupted, &utterance, speculative));

        // Its transcript did not match; cancel it.
        interrupted.store(true, Ordering::SeqCst);
        assert!(!should_keep_playing(&interrupted, &utterance, speculative));

        utterance.fetch_add(1, Ordering::SeqCst);
        interrupted.store(false, Ordering::SeqCst);

        assert!(
            !should_keep_playing(&interrupted, &utterance, speculative),
            "the superseded turn's audio must not resume when the next turn clears the flag"
        );
        let confirmed = utterance.load(Ordering::SeqCst);
        assert!(
            should_keep_playing(&interrupted, &utterance, confirmed),
            "the new turn must be free to speak"
        );
    }

    #[test]
    fn the_interrupt_flag_still_stops_the_current_turn() {
        let interrupted = AtomicBool::new(false);
        let utterance = AtomicU64::new(7);
        let mine = 7;

        assert!(should_keep_playing(&interrupted, &utterance, mine));
        interrupted.store(true, Ordering::SeqCst);
        assert!(!should_keep_playing(&interrupted, &utterance, mine));
    }

    /// Sentences of one reply share a generation.
    #[test]
    fn every_sentence_of_one_reply_plays() {
        let interrupted = AtomicBool::new(false);
        let utterance = AtomicU64::new(3);
        let mine = 3;
        for sentence in 0..5 {
            assert!(
                should_keep_playing(&interrupted, &utterance, mine),
                "sentence {sentence} was dropped mid-reply"
            );
        }
    }

    // ── the working tone ──────────────────────────────────────────────────

    /// Exactly the decision the tone thread makes on each poll.
    fn tone_is_ours(thinking_for: &AtomicU64, mine: u64) -> bool {
        thinking_for.load(Ordering::Relaxed) == mine
    }

    #[test]
    fn a_superseded_tone_thread_exits_instead_of_adopting_the_next_turn() {
        let thinking_for = AtomicU64::new(TONE_OFF);

        let first = 1u64;
        thinking_for.store(first, Ordering::SeqCst);
        assert!(tone_is_ours(&thinking_for, first));

        // Turn 1 stops it, and turn 2 starts its own before thread 1 polls.
        thinking_for.store(TONE_OFF, Ordering::SeqCst);
        let second = 2u64;
        thinking_for.store(second, Ordering::SeqCst);

        assert!(
            !tone_is_ours(&thinking_for, first),
            "thread 1 must exit, not adopt turn 2's tone and play alongside thread 2"
        );
        assert!(tone_is_ours(&thinking_for, second));
    }

    #[test]
    fn stopping_the_tone_silences_every_generation() {
        let thinking_for = AtomicU64::new(5);
        thinking_for.store(TONE_OFF, Ordering::SeqCst);
        for generation in [1u64, 5, 9] {
            assert!(!tone_is_ours(&thinking_for, generation));
        }
    }

    /// The swap returns the same generation, so `start_thinking_tone` bails.
    #[test]
    fn asking_twice_within_a_turn_starts_one_thread() {
        let thinking_for = AtomicU64::new(TONE_OFF);
        let mine = 4u64;

        assert_ne!(
            thinking_for.swap(mine, Ordering::SeqCst),
            mine,
            "first claim"
        );
        assert_eq!(
            thinking_for.swap(mine, Ordering::SeqCst),
            mine,
            "second claim must be recognised as already ours"
        );
    }

    #[test]
    fn no_real_turn_can_collide_with_the_off_sentinel() {
        assert_eq!(TONE_OFF, 0);
        let first_ever = AtomicU64::new(1);
        assert_ne!(
            first_ever.load(Ordering::SeqCst),
            TONE_OFF,
            "generations start at 1"
        );
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::*;

    #[test]
    fn envelope_is_empty_for_a_header_only_wav() {
        assert!(compute_audio_envelope(&[0u8; 44], 50).is_empty());
        assert!(compute_audio_envelope(&[], 50).is_empty());
    }

    /// Drives the UI's speaking-amplitude readout, so full-scale PCM must read full-scale.
    #[test]
    fn envelope_tracks_amplitude() {
        let mut wav = pond_voice::dsp::encode_wav_pcm16(&[], 24_000);
        let one_window = 24_000 * 50 / 1000;
        wav.extend((0..one_window).flat_map(|_| i16::MAX.to_le_bytes()));
        let env = compute_audio_envelope(&wav, 50);
        assert_eq!(env.len(), 1);
        assert!(
            env[0] > 0.9,
            "full-scale PCM should read near 1.0, got {}",
            env[0]
        );
    }
}
