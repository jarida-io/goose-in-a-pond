//! Which speaker this process plays on. One route for every sound it makes, so the thinking tone,
//! the wake ping and the voice never end up on different devices.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

use anyhow::{anyhow, Result};
use rodio::cpal::traits::{DeviceTrait, HostTrait};

static ROUTE: RwLock<Option<String>> = RwLock::new(None);
/// Bumped on every change, so holders of an open stream know to reopen.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// The device playback last saw stop taking audio, for the device supervisor to collect.
static STALLED: Mutex<Option<String>> = Mutex::new(None);
/// Devices the audio bus holds open, so a probe can read one as working instead of opening it a
/// second time (ALSA `hw` refuses that).
static HELD: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// The routed device, found once: on ALSA finding a named device opens every node to list them,
/// which would otherwise happen each time the thinking tone or the wake ping starts.
static RESOLVED: Mutex<Option<(String, rodio::cpal::Device)>> = Mutex::new(None);

/// Play on `device` from now on; `None` follows the system default.
pub fn set_output_device(device: Option<String>) {
    let mut route = ROUTE.write().unwrap_or_else(|e| e.into_inner());
    if *route != device {
        tracing::info!(device = ?device, "speaker route");
        *route = device;
        forget_devices();
        GENERATION.fetch_add(1, Ordering::SeqCst);
    }
}

pub fn output_device() -> Option<String> {
    ROUTE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

/// Playback found `device` (`None`: the default) no longer taking audio.
pub fn report_stall(device: Option<String>) {
    let name = device.or_else(default_output_name).unwrap_or_default();
    tracing::warn!(device = %name, "the speaker stopped taking audio");
    *STALLED.lock().unwrap_or_else(|e| e.into_inner()) = Some(name);
}

/// The device playback found stalled since the last call, if any.
pub fn take_stall() -> Option<String> {
    STALLED.lock().unwrap_or_else(|e| e.into_inner()).take()
}

pub fn is_held(device: &str) -> bool {
    HELD.lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|d| d == device)
}

pub(crate) struct Hold(String);

impl Hold {
    pub(crate) fn new(device: String) -> Self {
        HELD.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(device.clone());
        Self(device)
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = held.iter().position(|d| d == &self.0) {
            held.remove(i);
        }
    }
}

pub fn default_output_name() -> Option<String> {
    // ALSA reads the environment from C while it resolves the default; see process_env.
    pond_core::shared::services::process_env::reading(|| {
        rodio::cpal::default_host()
            .default_output_device()
            .and_then(|d| d.name().ok())
    })
}

/// Find an output device by name. ALSA's `default` is not worth listing every node for.
pub fn find_output(name: &str) -> Option<rodio::cpal::Device> {
    if let Some((cached, device)) = &*RESOLVED.lock().unwrap_or_else(|e| e.into_inner()) {
        if cached == name {
            return Some(device.clone());
        }
    }
    let found = pond_core::shared::services::process_env::reading(|| {
        let host = rodio::cpal::default_host();
        host.default_output_device()
            .filter(|d| d.name().ok().as_deref() == Some(name))
            .or_else(|| {
                host.output_devices()
                    .ok()?
                    .find(|d| d.name().ok().as_deref() == Some(name))
            })
    })?;
    *RESOLVED.lock().unwrap_or_else(|e| e.into_inner()) = Some((name.to_string(), found.clone()));
    Some(found)
}

/// Find the routed device afresh next time: one replugged under the same name is a new device.
pub fn forget_devices() {
    *RESOLVED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// An open stream on `device` (`None`: the system default), and the name of what opened. A
/// routed device that has gone falls back to the default rather than going quiet.
pub fn open_output(
    device: Option<&str>,
) -> Result<(rodio::OutputStream, rodio::OutputStreamHandle, String)> {
    if let Some(want) = device {
        match find_output(want).map(|d| {
            pond_core::shared::services::process_env::reading(|| {
                rodio::OutputStream::try_from_device(&d)
            })
        }) {
            Some(Ok((stream, handle))) => return Ok((stream, handle, want.to_string())),
            Some(Err(e)) => {
                forget_devices();
                tracing::warn!(device = %want, error = %e, "speaker failed to open; using the default")
            }
            None => tracing::warn!(device = %want, "speaker not found; using the default"),
        }
    }
    let (stream, handle) =
        pond_core::shared::services::process_env::reading(rodio::OutputStream::try_default)
            .map_err(|e| anyhow!("audio output device unavailable: {e}"))?;
    Ok((stream, handle, default_output_name().unwrap_or_default()))
}

/// [`open_output`] on the current route.
pub(crate) fn open_routed() -> Result<(rodio::OutputStream, rodio::OutputStreamHandle, String)> {
    open_output(output_device().as_deref())
}

/// A warm jazz Fender Rhodes chord (Ebmaj9) with bell tines, acoustic chorusing,
/// cabinet filtering, and room bloom.
pub fn chime_samples(rate: u32) -> Vec<f32> {
    let duration_secs = 1.1f32;
    let total_samples = (rate as f32 * duration_secs) as usize;
    if total_samples == 0 {
        return Vec::new();
    }
    let mut raw = vec![0.0f32; total_samples];

    // Rolled Ebmaj9 voicing: Eb3, Bb3, G4, D5, F5
    let notes: [(f32, f32, f32, f32); 5] = [
        (0.00, 155.56, 0.32, 0.85),
        (0.04, 233.08, 0.26, 0.80),
        (0.08, 392.00, 0.24, 0.75),
        (0.12, 587.33, 0.25, 0.80),
        (0.16, 698.46, 0.28, 0.85),
    ];

    for (start_sec, freq, amp, tau) in notes {
        let start_idx = (start_sec * rate as f32) as usize;
        let detune = 2.0f32.powf(1.2 / 1200.0);
        let f1 = freq * detune;
        let f2 = freq / detune;

        for i in start_idx..total_samples {
            let t = (i - start_idx) as f32 / rate as f32;
            let env = (-t / tau).exp();
            let thud = (-t / 0.015).exp() * 0.20 * (2.0 * std::f32::consts::PI * 95.0 * t).sin();
            let tine =
                (-t / 0.035).exp() * 0.18 * (2.0 * std::f32::consts::PI * freq * 4.31 * t).sin();
            let p1 = 2.0 * std::f32::consts::PI * f1 * t;
            let p2 = 2.0 * std::f32::consts::PI * f2 * t;
            let body = 0.5 * p1.sin()
                + 0.5 * p2.sin()
                + 0.26 * (2.0 * p1).sin()
                + 0.08 * (3.0 * p1).sin()
                + 0.02 * (4.0 * p1).sin();
            let tremolo = 1.0 + 0.07 * (2.0 * std::f32::consts::PI * 5.0 * t).sin();
            raw[i] += (body + thud + tine) * amp * env * tremolo;
        }
    }

    let mut saturated: Vec<f32> = raw.iter().map(|&x| (x * 1.25).tanh() / 1.25).collect();

    let cutoff = 3400.0f32;
    let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
    let dt = 1.0 / rate as f32;
    let alpha = dt / (rc + dt);
    let mut prev = 0.0f32;
    for x in &mut saturated {
        prev += alpha * (*x - prev);
        *x = prev;
    }

    let reverb_mix = 0.20f32;
    let delays = [
        (0.029 * rate as f32) as usize,
        (0.037 * rate as f32) as usize,
        (0.043 * rate as f32) as usize,
    ];
    let extra = (0.35 * rate as f32) as usize;
    let mut reverb_buf = vec![0.0f32; total_samples + extra];
    for d in delays {
        let feedback = 0.60f32;
        for i in 0..total_samples {
            if i + d < reverb_buf.len() {
                reverb_buf[i + d] += saturated[i] * 0.33;
                if i >= d {
                    let prev_val = reverb_buf[i];
                    reverb_buf[i + d] += prev_val * feedback * 0.33;
                }
            }
        }
    }

    let mut final_out = vec![0.0f32; total_samples];
    for i in 0..total_samples {
        final_out[i] = saturated[i] * (1.0 - reverb_mix) + reverb_buf[i] * reverb_mix;
    }

    let fade_in = (0.010 * rate as f32) as usize;
    for i in 0..fade_in.min(total_samples) {
        final_out[i] *= i as f32 / fade_in as f32;
    }
    let fade_out = (0.080 * rate as f32) as usize;
    for i in 0..fade_out.min(total_samples) {
        let idx = total_samples - 1 - i;
        final_out[idx] *= i as f32 / fade_out as f32;
    }

    let peak = final_out.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    if peak > 0.0 {
        let scale = 0.35 / peak;
        for x in &mut final_out {
            *x *= scale;
        }
    }
    final_out[0] = 0.0;
    final_out
}

/// Play [`chime_samples`] on `device` while `floor` holds the bus, and wait for it to finish.
pub fn play_chime(floor: &crate::Floor, device: Option<&str>) -> Result<()> {
    let rate = 44_100;
    play_samples_exactly(floor, device, chime_samples(rate), rate, 0.5)
}

/// Play mono `samples` on exactly `device` and wait for them to finish. Unlike [`open_output`]
/// this never falls back to the default: a sound test of one speaker must not sound on another.
/// It needs the bus held, like every sound, and plays on the bus's own stream when that is open on
/// `device`, which ALSA `hw` would not open twice.
pub fn play_samples_exactly(
    floor: &crate::Floor,
    device: Option<&str>,
    samples: Vec<f32>,
    rate: u32,
    volume: f32,
) -> Result<()> {
    if crate::bus().plays_on(device) {
        return floor.play(samples, rate, volume);
    }
    let (_stream, handle) = match device {
        Some(name) => {
            let found = find_output(name).ok_or_else(|| anyhow!("speaker '{name}' not found"))?;
            pond_core::shared::services::process_env::reading(|| {
                rodio::OutputStream::try_from_device(&found)
            })
            .map_err(|e| {
                forget_devices();
                anyhow!("speaker '{name}' failed to open: {e}")
            })?
        }
        None => pond_core::shared::services::process_env::reading(rodio::OutputStream::try_default)
            .map_err(|e| anyhow!("audio output device unavailable: {e}"))?,
    };
    let sink = rodio::Sink::try_new(&handle).map_err(|e| anyhow!("no sink: {e}"))?;
    use rodio::Source;
    let samples = rodio::buffer::SamplesBuffer::new(1, rate, samples).amplify(volume);
    sink.append(crate::tap::tapped(samples));
    let mut stall = crate::bus::Stall::new(&sink);
    while !sink.empty() {
        if stall.stuck(&sink) {
            sink.stop();
            anyhow::bail!("the speaker took no audio");
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The route is process-wide, so its tests take turns.
    pub(crate) static ROUTE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn changing_the_route_bumps_the_generation_and_repeating_it_does_not() {
        let _g = ROUTE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = output_device();
        set_output_device(Some("Test Speaker A".into()));
        let after_a = generation();
        set_output_device(Some("Test Speaker A".into()));
        assert_eq!(generation(), after_a, "same route, no reopen");
        set_output_device(Some("Test Speaker B".into()));
        assert_eq!(generation(), after_a + 1);
        assert_eq!(output_device().as_deref(), Some("Test Speaker B"));
        set_output_device(saved);
    }

    #[test]
    fn a_stall_is_collected_once() {
        report_stall(Some("Test Speaker".into()));
        assert_eq!(take_stall().as_deref(), Some("Test Speaker"));
        assert_eq!(take_stall(), None);
    }

    #[test]
    fn a_hold_is_released_on_drop() {
        let hold = Hold::new("Held Speaker".into());
        assert!(is_held("Held Speaker"));
        drop(hold);
        assert!(!is_held("Held Speaker"));
    }

    #[test]
    fn the_chime_is_short_soft_and_never_clips() {
        let s = chime_samples(44_100);
        assert!(
            s.len() <= (44_100.0 * 1.5) as usize,
            "longer than 1.5 seconds"
        );
        let peak = s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.05 && peak <= 0.4, "peak {peak}");
        assert_eq!(s[0], 0.0, "starts from silence");
    }
}
