//! One sound test: a speaker plays the test sound while a microphone listens for it.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, StreamTrait};
use pond_audio::MicShared;
use pond_audio_out::{Claim, Floor};
use pond_core::models::domain::audio_device::{AudioDirection, PairOutcome};

use crate::probe;
use crate::tone::{self, Listener};

/// What the room sounds like before the test sound, to judge the sound against.
const ROOM: Duration = Duration::from_millis(250);
/// Listening on after the sound ends: a Bluetooth speaker, or ALSA's dmix on a USB card, can lag
/// by several hundred milliseconds.
const TAIL: Duration = Duration::from_millis(900);
/// How long a microphone may take to deliver its first frames.
const FIRST_FRAMES: Duration = Duration::from_millis(1_500);
const TONE_RATE: u32 = 48_000;
/// How long a test waits for whatever the pond is playing to finish.
const WAIT_FOR_BUS: Duration = Duration::from_secs(10);
const SPEAKER_BUSY: &str = "the speaker is busy";

fn failed(reason: impl ToString) -> PairOutcome {
    PairOutcome::Failed {
        reason: reason.to_string(),
    }
}

fn play(floor: &Floor, speaker: &str) -> Result<(), String> {
    #[cfg(test)]
    if let Ok(player) = std::env::var("GIAP_TEST_PLAYER") {
        return tests::play_with(&player, speaker);
    }
    #[cfg(test)]
    let volume = std::env::var("GIAP_TEST_GAIN")
        .ok()
        .and_then(|g| g.parse().ok())
        .unwrap_or(1.0);
    #[cfg(not(test))]
    let volume = 1.0;
    pond_audio_out::route::play_samples_exactly(
        floor,
        Some(speaker),
        tone::tone_samples(TONE_RATE),
        TONE_RATE,
        volume,
    )
    .map_err(|e| e.to_string())
}

/// The whole test holds the audio bus, so nothing the pond plays lands in it, and other audio
/// pauses for it as for every sound the pond makes; `busy` when the bus stayed taken.
fn exclusively<T>(busy: T, f: impl FnOnce(&Floor) -> T) -> T {
    let Some(floor) = pond_audio_out::bus().claim(Claim::Test, Some(WAIT_FOR_BUS), || true) else {
        return busy;
    };
    let quiet = pond_core::models::services::voice::quiet::installed();
    let _hold = quiet.as_ref().map(|q| {
        let hold = q.hold();
        q.until_quiet_blocking(Duration::from_millis(400));
        hold
    });
    f(&floor)
}

/// Test with a microphone the mic owner already has open, read from its ring.
pub fn through_open_mic(speaker: &str, shared: &MicShared) -> PairOutcome {
    exclusively(failed(SPEAKER_BUSY), |floor| {
        let start = shared.written();
        std::thread::sleep(ROOM);
        let tone_at = shared.written();
        if let Err(e) = play(floor, speaker) {
            return failed(e);
        }
        std::thread::sleep(TAIL);
        let heard = shared.written().saturating_sub(start) as usize;
        let mut listener = Listener::new(pond_audio::CAPTURE_RATE_HZ);
        let samples = shared.recent(heard);
        let missing = heard.saturating_sub(samples.len());
        let tone_from = (tone_at.saturating_sub(start) as usize).saturating_sub(missing);
        let window = (pond_audio::CAPTURE_RATE_HZ / 50) as usize;
        listener.push(samples);
        listener.verdict(tone_from / window)
    })
}

/// Test with a microphone opened for the purpose, then closed.
pub fn through_mic(speaker: &str, mic: &str) -> PairOutcome {
    match listen_while_playing(speaker, mic, TAIL) {
        Ok((listener, tone_from)) => listener.verdict(tone_from),
        Err(outcome) => outcome,
    }
}

/// Open `mic`, measure the room, play the test sound on `speaker`, and listen on for `tail`.
/// The listener comes back with the window the sound started at.
pub fn listen_while_playing(
    speaker: &str,
    mic: &str,
    tail: Duration,
) -> Result<(Listener, usize), PairOutcome> {
    let Some(device) = probe::find(AudioDirection::Input, mic) else {
        return Err(failed("microphone not connected"));
    };
    let config = device.default_input_config().map_err(failed)?;
    let channels = config.channels().max(1) as usize;
    let listener = Arc::new(Mutex::new(Listener::new(config.sample_rate().0)));
    let on_err = |e| tracing::debug!(error = %e, "sound test input stream error");
    macro_rules! build {
        ($t:ty, $to_f32:expr) => {{
            let listener = listener.clone();
            device.build_input_stream(
                &config.config(),
                move |data: &[$t], _: &_| {
                    let mono = data.iter().step_by(channels).map(|s| $to_f32(*s));
                    listener
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(mono);
                },
                on_err,
                None,
            )
        }};
    }
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => build!(i16, |s: i16| s as f32 / 32_768.0),
        cpal::SampleFormat::U16 => build!(u16, |s: u16| (s as f32 - 32_768.0) / 32_768.0),
        cpal::SampleFormat::I32 => build!(i32, |s: i32| s as f32 / 2_147_483_648.0),
        other => return Err(failed(format!("unsupported sample format {other:?}"))),
    }
    .map_err(failed)?;
    stream.play().map_err(failed)?;
    let windows = || listener.lock().unwrap_or_else(|e| e.into_inner()).windows();
    let opened = Instant::now();
    while windows() == 0 {
        if opened.elapsed() >= FIRST_FRAMES {
            return Err(failed("the microphone delivered nothing"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let tone_from = exclusively(Err(SPEAKER_BUSY.to_string()), |floor| {
        std::thread::sleep(ROOM);
        let tone_from = windows();
        play(floor, speaker).map(|()| {
            std::thread::sleep(tail);
            tone_from
        })
    })
    .map_err(failed)?;
    drop(stream);
    let listener = Arc::try_unwrap(listener)
        .map_err(|_| failed("the microphone stream outlived the test"))?
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());
    Ok((listener, tone_from))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use cpal::traits::HostTrait;

    /// Play the test sound through an outside player (`aplay`), to tell the pond's playback path
    /// apart from the speaker and the room.
    pub(crate) fn play_with(player: &str, speaker: &str) -> Result<(), String> {
        let samples = tone::tone_samples(TONE_RATE);
        let mut wav = Vec::new();
        let data = samples.len() as u32 * 2;
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        for v in [
            16u32,
            1 | (1 << 16),
            TONE_RATE,
            TONE_RATE * 2,
            2 | (16 << 16),
        ] {
            wav.extend_from_slice(&v.to_le_bytes());
        }
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data.to_le_bytes());
        for v in samples {
            wav.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32_767.0) as i16).to_le_bytes());
        }
        let path = std::env::temp_dir().join("giap-sound-test.wav");
        std::fs::write(&path, wav).map_err(|e| e.to_string())?;
        let status = std::process::Command::new(player)
            .args(["-q", "-D", speaker])
            .arg(&path)
            .status()
            .map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&path);
        status
            .success()
            .then_some(())
            .ok_or_else(|| format!("{player} failed"))
    }

    /// Real hardware: where in time the test sound arrives, window by window. Set
    /// GIAP_TEST_SPEAKER / GIAP_TEST_MIC to pick devices; the defaults are used otherwise.
    #[test]
    #[ignore = "plays a sound and opens this machine's microphone"]
    fn sound_test_timeline_on_this_machine() {
        let host = cpal::default_host();
        let name = |d: Option<cpal::Device>| d.and_then(|d| d.name().ok()).unwrap_or_default();
        let speaker = std::env::var("GIAP_TEST_SPEAKER")
            .unwrap_or_else(|_| name(host.default_output_device()));
        let mic =
            std::env::var("GIAP_TEST_MIC").unwrap_or_else(|_| name(host.default_input_device()));
        if let Some(d) = probe::find(AudioDirection::Output, &speaker) {
            println!("output config cpal picks: {:?}", d.default_output_config());
        }
        let (listener, tone_from) =
            listen_while_playing(&speaker, &mic, Duration::from_secs(3)).expect("ran");
        println!("{speaker} -> {mic}: sound starts at window {tone_from}");
        for line in listener.trace(tone_from) {
            println!("{line}");
        }
        println!("verdict: {:?}", listener.verdict(tone_from));
    }
}
