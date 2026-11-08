//! Speaker-side audio for the whole process: the audio bus every sound plays on, the voice's
//! playback, the working tone and the wake ping.

use anyhow::{anyhow, Context, Result};
use pond_core::models::services::voice::quiet::WAIT_FOR_QUIET;
use pond_core::shared::domain::agent::ThrottledAudioLevelSink;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub mod announce;
pub mod bus;
pub mod route;
pub mod tap;
pub use announce::Announcer;
pub use bus::{bus, AudioBus, BusState, Claim, Floor, TurnFloor};
pub use route::{output_device, set_output_device};
pub use tap::{set_playback_tap, set_spoken_tap, spoken_tap, PlaybackTap, SpokenTap};

const TONE_RATE: u32 = 22_050;
/// One breath of the tone: a soft chime, then silence, then repeat.
const TONE_CYCLE_MS: u64 = 2_600;
/// How long the chime itself rings before the silence.
const TONE_CHIME_MS: u64 = 1_100;

/// `thinking_for` value meaning no tone; generations start at 1, so it never names a turn.
pub const TONE_OFF: u64 = 0;
/// Applied before the tap, as every level here is, so the echo canceller's reference is what plays.
const TONE_VOLUME: f32 = 0.10;

/// One tone cycle: a soft major-sixth chime (E6 over G5), then mostly silence. An octave lower,
/// the wake word model scored it 0.999 as "hey goose" and cut every turn it played in.
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

        let root = (2.0 * std::f32::consts::PI * 783.99 * t).sin(); // G5
        let sixth = (2.0 * std::f32::consts::PI * 1318.51 * t).sin(); // E6
        out.push((root * 0.6 + sixth * 0.4) * envelope * 0.5);
    }

    out.resize(cycle_samples, 0.0);
    out
}

/// Spawn the working-tone thread for turn `mine`; it exits once `thinking_for` names another.
/// A generation, not a bool: two live turns' start/stop calls would interleave. The tone is the
/// turn's first sound, so it takes the turn's hold on the bus.
pub fn start_thinking_tone_thread(thinking_for: Arc<AtomicU64>, mine: u64, turn: TurnFloor) {
    std::thread::spawn(move || {
        use rodio::Sink;

        let ours = |thinking_for: &AtomicU64| thinking_for.load(Ordering::Relaxed) == mine;
        // On a failed start, clear the claim only if a newer turn hasn't taken it.
        let release = |thinking_for: &AtomicU64| {
            let _ =
                thinking_for.compare_exchange(mine, TONE_OFF, Ordering::SeqCst, Ordering::SeqCst);
        };

        if !turn.hold(|| ours(&thinking_for)) {
            release(&thinking_for);
            return;
        }
        turn.settle(WAIT_FOR_QUIET);
        let Ok(sink) = bus()
            .speaker()
            .and_then(|s| Ok(Sink::try_new(&s.handle())?))
        else {
            release(&thinking_for);
            return;
        };

        let cycle = working_tone_cycle();
        let polls_per_cycle = TONE_CYCLE_MS / 50;
        let playing = || ours(&thinking_for) && turn.keep();

        while playing() {
            use rodio::Source;
            sink.append(tap::tapped(
                rodio::buffer::SamplesBuffer::new(1, TONE_RATE, cycle.clone()).amplify(TONE_VOLUME),
            ));
            for _ in 0..polls_per_cycle {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if !playing() {
                    sink.stop();
                    return;
                }
            }
        }
        sink.stop();
    });
}

/// The longest the ping waits for the bus, and then for other audio to pause, before it plays or
/// gives up: later than that it no longer answers the wake word.
const PING_WAITS_AT_MOST: Duration = Duration::from_millis(400);
const PING_RATE: u32 = 44_100;
const PING_VOLUME: f32 = 0.35;

/// The wake word was heard: C6 then E6, about 220 ms, rising.
fn wake_ping_samples() -> Vec<f32> {
    let tone = |freq: f32, ms: u64| -> Vec<f32> {
        let n = (PING_RATE as u64 * ms / 1000) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / PING_RATE as f32;
                let fade_out = 1.0 - (i as f32 / n as f32);
                (2.0 * std::f32::consts::PI * freq * t).sin() * fade_out * 0.6
            })
            .collect()
    };
    let mut samples = tone(1047.0, 100);
    samples.extend(tone(1319.0, 120));
    samples
}

/// Play the wake ping on its own thread: first in the bus's queue, and the holder it outranks
/// yields to it.
pub fn play_wake_ping() {
    std::thread::spawn(move || {
        let Some(floor) = bus().claim(Claim::Ping, Some(PING_WAITS_AT_MOST), || true) else {
            tracing::debug!("wake ping skipped: the speaker stayed busy");
            return;
        };
        floor.settle(PING_WAITS_AT_MOST);
        if let Err(e) = floor.play(wake_ping_samples(), PING_RATE, PING_VOLUME) {
            tracing::debug!(error = %e, "wake ping did not play");
        }
    });
}

/// `rodio::OutputStream` is `!Send` because of cpal's CoreAudio property listeners.
#[allow(dead_code)] // kept alive for its Drop (closes the audio device); never read
struct SendableStream(rodio::OutputStream);
// SAFETY: moved into the speaker thread once and only ever touched there.
unsafe impl Send for SendableStream {}

/// How often a [`Speaker`]'s thread looks for a route change.
const FOLLOW_POLL: Duration = Duration::from_millis(200);

struct SpeakerState {
    handle: rodio::OutputStreamHandle,
    /// The route generation the stream was opened for.
    generation: u64,
    device: String,
}

/// The bus's stream on the routed speaker, held open across sounds and reopened when the route
/// moves, so connecting headphones takes effect on the next sentence. Cheap to clone.
#[derive(Clone)]
pub(crate) struct Speaker {
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
    pub(crate) fn try_new(thread_name: &str) -> Result<Self> {
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

    pub(crate) fn handle(&self) -> rodio::OutputStreamHandle {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .handle
            .clone()
    }

    /// Changes whenever the stream is replaced; audio queued on the old one will never finish.
    pub(crate) fn generation(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .generation
    }

    /// The device this speaker has open.
    pub(crate) fn device(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .device
            .clone()
    }
}

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
pub(crate) const STALL_AFTER: Duration = Duration::from_secs(3);

/// Interruptibly play a turn's WAV once the turn holds the bus; `audio_level_sink` gets one level
/// per poll tick. Stops when the speaker reopens elsewhere (what was queued on the old stream would
/// never finish) or the turn loses the bus.
pub fn play_turn_wav(
    wav: Vec<u8>,
    turn: &TurnFloor,
    interrupted: &AtomicBool,
    utterance: &AtomicU64,
    audio_level_sink: Option<&ThrottledAudioLevelSink>,
) -> Result<()> {
    let mine = utterance.load(Ordering::SeqCst);
    let wanted =
        || !interrupted.load(Ordering::Relaxed) && utterance.load(Ordering::Relaxed) == mine;
    if !turn.hold(wanted) {
        return Ok(());
    }
    turn.settle(WAIT_FOR_QUIET);
    let speaker = bus().speaker()?;
    let generation = speaker.generation();
    play(
        wav,
        &speaker.handle(),
        || turn.keep() && speaker.generation() == generation,
        interrupted,
        utterance,
        audio_level_sink,
    )
}

/// [`play_turn_wav`] for words said outside a turn, on a bus `floor` already held.
pub fn play_wav(
    wav: Vec<u8>,
    floor: &Floor,
    interrupted: &AtomicBool,
    utterance: &AtomicU64,
    audio_level_sink: Option<&ThrottledAudioLevelSink>,
) -> Result<()> {
    floor.settle(WAIT_FOR_QUIET);
    let speaker = bus().speaker()?;
    let generation = speaker.generation();
    play(
        wav,
        &speaker.handle(),
        || floor.keep() && speaker.generation() == generation,
        interrupted,
        utterance,
        audio_level_sink,
    )
}

/// USB DACs ramp analog output over ~150 ms from idle squelch; leading silence keeps speech from clipping.
/// The last moment audio played, and the utterance it belonged to.
static LAST_PLAYBACK_AT: Mutex<Option<(Instant, u64)>> = Mutex::new(None);
const HARDWARE_WAKE_PREROLL_MS: u64 = 300;
const HARDWARE_IDLE_GAP_MS: u64 = 500;

/// Whether to lead with the wake pre-roll: only the first audio after idle. Inside one reply the
/// units' pauses are timed by the voice, and a slow synthesis must not be stretched by 300 ms more.
fn needs_wake_preroll(last: Option<(Instant, u64)>, now: Instant, utterance: u64) -> bool {
    match last {
        Some((_, previous)) if previous == utterance => false,
        Some((at, _)) => now.duration_since(at) >= Duration::from_millis(HARDWARE_IDLE_GAP_MS),
        None => true,
    }
}

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
        let now = Instant::now();
        let wake = needs_wake_preroll(*last, now, mine);
        *last = Some((now, mine));
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
        sink.append(tap::tapped(silence));
    }
    sink.append(tap::tapped(decoder));

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
            tracing::info!("TTS playback cut: the speaker changed or the turn lost the bus");
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
            *last = Some((Instant::now(), mine));
        }
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
    }

    if let Ok(mut last) = LAST_PLAYBACK_AT.lock() {
        *last = Some((Instant::now(), mine));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_the_first_audio_after_idle_gets_the_wake_preroll() {
        use super::{needs_wake_preroll, HARDWARE_IDLE_GAP_MS};
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let later = t0 + Duration::from_millis(HARDWARE_IDLE_GAP_MS + 300);
        assert!(needs_wake_preroll(None, t0, 1), "nothing has played yet");
        assert!(
            !needs_wake_preroll(Some((t0, 1)), later, 1),
            "a unit of the same reply after a slow synthesis"
        );
        assert!(
            needs_wake_preroll(Some((t0, 1)), later, 2),
            "a new reply after idle"
        );
        assert!(
            !needs_wake_preroll(Some((t0, 1)), t0 + Duration::from_millis(100), 2),
            "a new reply straight after the last is still awake"
        );
    }

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

    #[test]
    fn the_wake_ping_is_short_and_starts_and_ends_near_silence() {
        let ping = wake_ping_samples();
        assert!(
            ping.len() <= PING_RATE as usize / 4,
            "{} samples",
            ping.len()
        );
        assert_eq!(ping[0], 0.0);
        assert!(ping.last().is_some_and(|v| v.abs() < 0.01));
        assert!(ping.iter().all(|v| v.abs() <= 0.6));
    }

    /// The ear listens for the wake word while these play, so none may sound like it.
    #[test]
    fn the_wake_word_model_never_takes_the_ponds_own_sounds_for_the_wake_word() {
        use pond_ear::wake::{detector::Detector, hey_goose, HEY_GOOSE_THRESHOLD};
        let most = |samples: &[f32], rate: u32| {
            let mut pcm = Vec::new();
            pond_ear::dsp::Resampler::new(rate, 16_000).process(samples, &mut pcm);
            let mut wake = Detector::new(
                hey_goose().unwrap(),
                pond_ear::Rung::fastest(),
                HEY_GOOSE_THRESHOLD,
            )
            .unwrap();
            let quiet = vec![0f32; 16_000];
            [&quiet[..], &pcm, &pcm, &pcm, &quiet]
                .iter()
                .flat_map(|part| wake.push(part).0)
                .map(|score| score.p)
                .fold(0f32, f32::max)
        };
        let sounds = [
            ("thinking tone", working_tone_cycle(), TONE_RATE),
            ("wake ping", wake_ping_samples(), PING_RATE),
            ("startup chime", route::chime_samples(44_100), 44_100),
        ];
        for (name, samples, rate) in sounds {
            for gain in [1.0, 0.1, 0.01] {
                let scaled: Vec<f32> = samples.iter().map(|v| v * gain).collect();
                let p = most(&scaled, rate);
                assert!(p < 0.5, "{name} at gain {gain} scores {p} as the wake word");
            }
        }
    }

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
