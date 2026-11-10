//! The real capture device; all testable logic lives in [`crate::owner`] and [`crate::ring`].

use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::owner::{CaptureDevice, MicShared, MicState};
use crate::ring::{i16_to_f32, to_mono_f32, u16_to_f32};

/// `cpal::Stream` is `!Send` (CoreAudio ties listeners to its thread); it never leaves the owner.
/// Holding it keeps the device open.
struct SendableStream(#[allow(dead_code)] cpal::Stream);
unsafe impl Send for SendableStream {}

/// The default system input device.
#[derive(Default)]
pub struct CpalCapture {
    stream: Option<SendableStream>,
    /// Preferred device name; `None` uses the host default.
    preferred: Option<String>,
    /// What the open stream is on, which is the default when `preferred` was not found.
    opened: Option<String>,
    /// `preferred`, found once: on ALSA finding a named device opens every node to list them
    /// (and wakes the jack plugin), which would otherwise happen on every reopen of the mic.
    resolved: Option<(String, cpal::Device)>,
}

impl CpalCapture {
    pub fn new() -> Self {
        Self::default()
    }

    /// Capture from a named device instead of the host default.
    pub fn with_device(name: impl Into<String>) -> Self {
        Self {
            preferred: Some(name.into()),
            ..Self::default()
        }
    }

    fn pick(&mut self) -> Result<cpal::Device, String> {
        let host = cpal::default_host();
        if let Some(want) = self.preferred.clone() {
            if let Some((name, device)) = &self.resolved {
                if *name == want {
                    return Ok(device.clone());
                }
            }
            let default = host
                .default_input_device()
                .filter(|d| d.name().ok().as_deref() == Some(want.as_str()));
            let found = match default {
                Some(d) => Some(d),
                None => host
                    .input_devices()
                    .map_err(|e| format!("cannot enumerate input devices: {e}"))?
                    .find(|d| d.name().ok().as_deref() == Some(want.as_str())),
            };
            if let Some(d) = found {
                self.resolved = Some((want, d.clone()));
                return Ok(d);
            }
            tracing::warn!(
                device = %want,
                "configured input device not found; falling back to the system default"
            );
        }
        host.default_input_device()
            .ok_or_else(|| "no audio input device found".to_string())
    }
}

impl CaptureDevice for CpalCapture {
    fn start(&mut self, shared: Arc<MicShared>) -> Result<(), String> {
        self.stop();

        // ALSA reads the environment from C while it finds and opens a device; see process_env.
        let opened = pond_core::shared::services::process_env::reading(|| {
            let device = self.pick()?;
            self.open(&device, shared)
        });
        if opened.is_err() {
            // The device may have gone; find it afresh next time instead of reusing the handle.
            self.resolved = None;
        }
        opened
    }

    fn stop(&mut self) {
        // Dropping the stream closes the device, which turns the OS mic indicator off.
        self.stream = None;
        self.opened = None;
    }

    fn retarget(&mut self, device: Option<String>) -> bool {
        let changed = self.preferred != device;
        if changed {
            self.resolved = None;
        }
        self.preferred = device;
        changed
    }

    fn opened(&self) -> Option<String> {
        self.opened.clone()
    }
}

impl CpalCapture {
    fn open(&mut self, device: &cpal::Device, shared: Arc<MicShared>) -> Result<(), String> {
        let name = device.name().ok();
        let supported = device
            .default_input_config()
            .map_err(|e| format!("no usable input config: {e}"))?;
        let sample_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        // A lost device stops calling back, so nothing downstream would ever notice; the state
        // is what the device supervisor watches to move to another.
        let lost = shared.clone();
        let on_err = move |e: cpal::StreamError| {
            tracing::warn!(error = %e, "input stream error");
            if matches!(e, cpal::StreamError::DeviceNotAvailable) {
                lost.set_state(MicState::Failed(format!("input device lost: {e}")));
            }
        };

        // Resample once, here, so subscribers only ever see 16 kHz mono f32.
        let push = move |mono: Vec<f32>| {
            let at_16k = pond_voice::dsp::resample_to_16k(&mono, sample_rate);
            let mut ring = shared.ring.lock().unwrap_or_else(|e| e.into_inner());
            ring.push(&at_16k);
            drop(ring);
            shared.bump();
        };

        let stream = match format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |d: &[f32], _: &_| push(to_mono_f32(d, channels, |s| s)),
                on_err,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |d: &[i16], _: &_| push(to_mono_f32(d, channels, i16_to_f32)),
                on_err,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config,
                move |d: &[u16], _: &_| push(to_mono_f32(d, channels, u16_to_f32)),
                on_err,
                None,
            ),
            other => return Err(format!("unsupported sample format {other:?}")),
        }
        .map_err(|e| format!("could not open input stream: {e}"))?;

        stream
            .play()
            .map_err(|e| format!("could not start input stream: {e}"))?;

        tracing::debug!(
            sample_rate,
            channels,
            ?format,
            "microphone open (normalised to 16 kHz mono)"
        );
        self.stream = Some(SendableStream(stream));
        self.opened = name;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stale device setting must not make the assistant deaf.
    #[test]
    fn an_unknown_preferred_device_falls_back_to_the_default() {
        let mut cap = CpalCapture::with_device("no-such-device-8f3a2b");
        match cap.pick() {
            Ok(_) => {}
            Err(e) => assert!(e.contains("no audio input device"), "{e}"),
        }
    }

    #[test]
    fn stop_is_idempotent() {
        let mut cap = CpalCapture::new();
        cap.stop();
        cap.stop();
    }
}
