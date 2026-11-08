//! Opening one device briefly to see whether it works. A microphone's samples are folded into a
//! level as they arrive and dropped; nothing it hears outlives the callback.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use pond_audio::MicShared;
use pond_core::models::domain::audio_device::{classify_input, AudioDirection, ProbeOutcome};

/// How much audio a microphone is measured over once it starts delivering.
const MEASURE: Duration = Duration::from_millis(400);
const POLL: Duration = Duration::from_millis(20);

pub fn find(direction: AudioDirection, name: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    let default = match direction {
        AudioDirection::Input => host.default_input_device(),
        AudioDirection::Output => host.default_output_device(),
    };
    if let Some(d) = default {
        if d.name().ok().as_deref() == Some(name) {
            return Some(d);
        }
    }
    let mut all = match direction {
        AudioDirection::Input => {
            Box::new(host.input_devices().ok()?) as Box<dyn Iterator<Item = _>>
        }
        AudioDirection::Output => Box::new(host.output_devices().ok()?),
    };
    all.find(|d| d.name().ok().as_deref() == Some(name))
}

#[derive(Default)]
struct Level {
    frames: u64,
    sum_sq: f64,
    peak: f32,
}

impl Level {
    fn add(&mut self, mono: impl Iterator<Item = f32>) {
        for s in mono {
            self.frames += 1;
            self.sum_sq += (s as f64) * (s as f64);
            self.peak = self.peak.max(s.abs());
        }
    }
    fn outcome(&self) -> ProbeOutcome {
        let rms = if self.frames == 0 {
            0.0
        } else {
            (self.sum_sq / self.frames as f64).sqrt() as f32
        };
        classify_input(self.frames, self.peak, rms)
    }
}

fn failed(reason: impl ToString) -> ProbeOutcome {
    ProbeOutcome::Failed {
        reason: reason.to_string(),
    }
}

/// Open a microphone for at most `wait` plus [`MEASURE`], and read its level.
pub fn probe_input(name: &str, wait: Duration) -> ProbeOutcome {
    let Some(device) = find(AudioDirection::Input, name) else {
        return failed("not connected");
    };
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => return failed(e),
    };
    let rate = config.sample_rate().0 as u64;
    let channels = config.channels().max(1) as usize;
    let level = Arc::new(Mutex::new(Level::default()));
    let on_err = |e| tracing::debug!(error = %e, "probe input stream error");

    macro_rules! build {
        ($t:ty, $to_f32:expr) => {{
            let level = level.clone();
            device.build_input_stream(
                &config.config(),
                move |data: &[$t], _: &_| {
                    // The first channel is enough to tell sound from silence.
                    let mono = data.iter().step_by(channels).map(|s| $to_f32(*s));
                    level.lock().unwrap_or_else(|e| e.into_inner()).add(mono);
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
        other => return failed(format!("unsupported sample format {other:?}")),
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    if let Err(e) = stream.play() {
        return failed(e);
    }

    let want = rate * MEASURE.as_millis() as u64 / 1000;
    let started = Instant::now();
    let mut first_at: Option<Instant> = None;
    loop {
        std::thread::sleep(POLL);
        let frames = level.lock().unwrap_or_else(|e| e.into_inner()).frames;
        if frames > 0 && first_at.is_none() {
            first_at = Some(Instant::now());
        }
        if frames >= want {
            break;
        }
        let deadline = match first_at {
            Some(t) => t + MEASURE * 2,
            None => started + wait,
        };
        if Instant::now() >= deadline {
            break;
        }
    }
    drop(stream);
    let outcome = level.lock().unwrap_or_else(|e| e.into_inner()).outcome();
    outcome
}

/// The level of the device the mic owner already has open, read from its ring instead of
/// opening it twice. The ring must also be growing: old audio is no proof the device still works.
pub fn measure_open(shared: &MicShared, wait: Duration) -> ProbeOutcome {
    let window = (pond_audio::CAPTURE_RATE_HZ as u64 * MEASURE.as_millis() as u64 / 1000) as usize;
    let started = Instant::now();
    let written_before = shared.written();
    loop {
        std::thread::sleep(POLL);
        let grew = shared.written() > written_before;
        let recent = shared.recent(window);
        let timed_out = started.elapsed() >= wait;
        if (grew && recent.len() >= window) || timed_out {
            if !grew {
                return ProbeOutcome::Stalled;
            }
            let mut level = Level::default();
            level.add(recent.into_iter());
            return level.outcome();
        }
    }
}

/// Open a speaker, hand it silence, and see whether it takes it, while `_held` holds the audio bus.
pub fn probe_output(_held: &pond_audio_out::Floor, name: &str, wait: Duration) -> ProbeOutcome {
    // Already open and playing here; on ALSA `hw` a second open would fail as busy.
    if pond_audio_out::route::is_held(name) {
        return ProbeOutcome::Accepting;
    }
    let Some(device) = find(AudioDirection::Output, name) else {
        return failed("not connected");
    };
    let config = match device.default_output_config() {
        Ok(c) => c,
        Err(e) => return failed(e),
    };
    let pulled = Arc::new(Mutex::new(0u64));
    let on_err = |e| tracing::debug!(error = %e, "probe output stream error");

    macro_rules! build {
        ($t:ty, $silence:expr) => {{
            let pulled = pulled.clone();
            device.build_output_stream(
                &config.config(),
                move |data: &mut [$t], _: &_| {
                    data.fill($silence);
                    *pulled.lock().unwrap_or_else(|e| e.into_inner()) += data.len() as u64;
                },
                on_err,
                None,
            )
        }};
    }
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build!(f32, 0.0),
        cpal::SampleFormat::I16 => build!(i16, 0),
        cpal::SampleFormat::U16 => build!(u16, 32_768),
        cpal::SampleFormat::I32 => build!(i32, 0),
        other => return failed(format!("unsupported sample format {other:?}")),
    };
    let stream = match stream {
        Ok(s) => s,
        Err(e) => return failed(e),
    };
    if let Err(e) = stream.play() {
        return failed(e);
    }
    let deadline = Instant::now() + wait;
    let outcome = loop {
        std::thread::sleep(POLL);
        if *pulled.lock().unwrap_or_else(|e| e.into_inner()) > 0 {
            break ProbeOutcome::Accepting;
        }
        if Instant::now() >= deadline {
            break ProbeOutcome::Stalled;
        }
    };
    drop(stream);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_of_digital_zeros_is_silence_and_of_noise_is_hearing() {
        let mut zeros = Level::default();
        zeros.add(std::iter::repeat_n(0.0, 4_800));
        assert_eq!(zeros.outcome(), ProbeOutcome::Silent);

        let mut room = Level::default();
        room.add((0..4_800).map(|i| if i % 2 == 0 { 0.001 } else { -0.001 }));
        match room.outcome() {
            ProbeOutcome::Hearing { level_dbfs } => assert!((level_dbfs + 60.0).abs() < 0.5),
            other => panic!("{other:?}"),
        }

        assert_eq!(Level::default().outcome(), ProbeOutcome::Stalled);
    }

    #[test]
    fn an_absent_device_fails_fast_without_hanging() {
        let t = Instant::now();
        assert_eq!(
            probe_input("no-such-device-8f3a2b", Duration::from_secs(5)),
            failed("not connected")
        );
        let held = pond_audio_out::bus()
            .claim(pond_audio_out::Claim::Probe, None, || true)
            .unwrap();
        assert_eq!(
            probe_output(&held, "no-such-device-8f3a2b", Duration::from_secs(5)),
            failed("not connected")
        );
        assert!(t.elapsed() < Duration::from_secs(3));
    }
}
