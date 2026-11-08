//! [`AudioDeviceProber`] on cpal: CoreAudio on the Mac, ALSA (and through it PipeWire or
//! PulseAudio) on Linux.

use std::collections::HashSet;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait};
use pond_audio::MicHandle;
use pond_core::models::domain::audio_device::{
    AudioDevice, AudioDirection, LevelChange, PairOutcome, ProbeOutcome, SoundServerDefaults,
};
use pond_core::models::domain::mic_gate;
use pond_core::models::ports::audio_devices::AudioDeviceProber;

use crate::names;
use crate::probe;
use crate::sound;
use crate::sound_server::{self, SystemPactl};
use pond_audio_out::Claim;

/// How long a device's chime waits for whatever the pond is playing.
const CHIME_WAITS_FOR_BUS: Duration = Duration::from_secs(10);

/// cpal's only host on Linux is ALSA, whose listing opens every node.
const ALSA: bool = cfg!(target_os = "linux");

/// Time allowed beyond a probe's own wait for the driver to open and close the device.
const OPEN_SLACK: Duration = Duration::from_secs(6);
/// A whole sound test: opening both ends, the room, the sound, and the listening after it.
const SOUND_TEST_LIMIT: Duration = Duration::from_secs(10);

pub struct CpalProber {
    /// The process's mic owner. Input probes run on its thread, so nothing opens a microphone
    /// beside it, and the device it has open is measured rather than reopened.
    mic: Option<MicHandle>,
}

impl CpalProber {
    pub fn new(mic: Option<MicHandle>) -> Self {
        Self { mic }
    }

    fn cards() -> Vec<names::Card> {
        if !ALSA {
            return Vec::new();
        }
        std::fs::read_to_string("/proc/asound/cards")
            .map(|t| names::parse_cards(&t))
            .unwrap_or_default()
    }

    fn describe(name: &str, is_default: bool, cards: &[names::Card]) -> AudioDevice {
        AudioDevice {
            id: name.to_string(),
            label: if ALSA {
                names::alsa_label(name, cards)
            } else {
                name.to_string()
            },
            is_default,
            kind: names::kind_of(name),
            plugged_in: names::card_of(name, cards).is_some_and(names::Card::plugged_in),
        }
    }

    fn default_name(direction: AudioDirection) -> Option<String> {
        let host = cpal::default_host();
        let device = match direction {
            AudioDirection::Input => host.default_input_device(),
            AudioDirection::Output => host.default_output_device(),
        }?;
        device.name().ok()
    }

    fn open_input(&self) -> Option<String> {
        let mic = self.mic.as_ref()?;
        mic.run_exclusive(Duration::from_secs(2), |view| view.open_device.clone())
            .flatten()
    }
}

impl AudioDeviceProber for CpalProber {
    fn default_device(&self, direction: AudioDirection) -> Option<AudioDevice> {
        let name = Self::default_name(direction)?;
        Some(Self::describe(&name, true, &Self::cards()))
    }

    fn devices(&self, direction: AudioDirection) -> Result<Vec<AudioDevice>, String> {
        let host = cpal::default_host();
        let default = Self::default_name(direction);
        let cards = Self::cards();
        let _suspended = ALSA.then(|| sound_server::suspend_active(&SystemPactl, direction));
        let listed: Vec<cpal::Device> = match direction {
            AudioDirection::Input => host.input_devices().map_err(|e| e.to_string())?.collect(),
            AudioDirection::Output => host.output_devices().map_err(|e| e.to_string())?.collect(),
        };
        drop(_suspended);
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for name in listed.iter().filter_map(|d| d.name().ok()) {
            if ALSA && !names::alsa_worth_offering(&name, direction) {
                continue;
            }
            if seen.insert(name.clone()) {
                let is_default = default.as_deref() == Some(name.as_str());
                out.push(Self::describe(&name, is_default, &cards));
            }
        }
        // ALSA leaves out a node that is busy, and the one the mic owner holds always is.
        if direction == AudioDirection::Input {
            if let Some(open) = self.open_input().filter(|o| !seen.contains(o)) {
                let is_default = default.as_deref() == Some(open.as_str());
                out.push(Self::describe(&open, is_default, &cards));
            }
        }
        Ok(out)
    }

    fn quick_list(&self, direction: AudioDirection) -> Vec<AudioDevice> {
        if !ALSA {
            return self.devices(direction).unwrap_or_default();
        }
        // Read from /proc rather than by opening nodes: the default, and each plugged-in card's
        // shared route (`sysdefault` mixes, so the voice and the thinking tone can both play).
        let cards = Self::cards();
        let pcm = std::fs::read_to_string("/proc/asound/pcm").unwrap_or_default();
        let mut out: Vec<AudioDevice> = self.default_device(direction).into_iter().collect();
        for card in cards.iter().filter(|c| c.plugged_in()) {
            if names::card_directions(&pcm, card.index, direction) {
                let name = format!("sysdefault:CARD={}", card.id);
                out.push(Self::describe(&name, false, &cards));
            }
        }
        out
    }

    fn plugged_in_first(&self) -> bool {
        ALSA
    }

    fn hears(&self, speaker: &AudioDevice, mic: &AudioDevice) -> PairOutcome {
        let off = || PairOutcome::Failed {
            reason: "the microphone is off in Settings".to_string(),
        };
        if !mic_gate::mic_enabled() {
            return off();
        }
        let (speaker, mic) = (speaker.id.clone(), mic.id.clone());
        let cards = Self::cards();
        let Some(owner) = &self.mic else {
            let _suspended = ALSA.then(|| {
                sound_server::suspend_for_devices(&SystemPactl, &cards, Some(&mic), Some(&speaker))
            });
            return sound::through_mic(&speaker, &mic);
        };
        // On the owner's thread, so nothing else opens a microphone during the test.
        owner
            .run_exclusive(SOUND_TEST_LIMIT, move |view| {
                if !view.shared.enabled() {
                    off()
                } else if view.open_device.as_deref() == Some(mic.as_str()) {
                    let _suspended = ALSA.then(|| {
                        sound_server::suspend_for_devices(
                            &SystemPactl,
                            &cards,
                            None,
                            Some(&speaker),
                        )
                    });
                    sound::through_open_mic(&speaker, view.shared)
                } else {
                    let _suspended = ALSA.then(|| {
                        sound_server::suspend_for_devices(
                            &SystemPactl,
                            &cards,
                            Some(&mic),
                            Some(&speaker),
                        )
                    });
                    sound::through_mic(&speaker, &mic)
                }
            })
            .unwrap_or_else(|| PairOutcome::Failed {
                reason: "the sound test did not finish in time".to_string(),
            })
    }

    fn probe(
        &self,
        device: &AudioDevice,
        direction: AudioDirection,
        wait: Duration,
    ) -> ProbeOutcome {
        let cards = Self::cards();
        if direction == AudioDirection::Output {
            // Held before the card is suspended: suspending it under a reply would cut the reply.
            let Some(floor) = pond_audio_out::bus().claim(Claim::Probe, Some(wait), || true) else {
                return ProbeOutcome::Untested;
            };
            let _suspended = ALSA.then(|| {
                sound_server::suspend_for_devices(&SystemPactl, &cards, None, Some(&device.id))
            });
            return probe::probe_output(&floor, &device.id, wait);
        }
        if !mic_gate::mic_enabled() {
            return ProbeOutcome::Untested;
        }
        let Some(mic) = &self.mic else {
            let _suspended = ALSA.then(|| {
                sound_server::suspend_for_devices(&SystemPactl, &cards, Some(&device.id), None)
            });
            return probe::probe_input(&device.id, wait);
        };
        let id = device.id.clone();
        // Opening can block inside the driver: an idle Continuity iPhone mic took longer than
        // three seconds on the Mac. Past this, the device is not answering, not the owner.
        mic.run_exclusive(wait + OPEN_SLACK, move |view| {
            if !view.shared.enabled() {
                ProbeOutcome::Untested
            } else if view.open_device.as_deref() == Some(id.as_str()) {
                probe::measure_open(view.shared, wait)
            } else {
                let _suspended = ALSA.then(|| {
                    sound_server::suspend_for_devices(&SystemPactl, &cards, Some(&id), None)
                });
                probe::probe_input(&id, wait)
            }
        })
        .unwrap_or(ProbeOutcome::Stalled)
    }

    fn fingerprint(&self) -> String {
        if ALSA {
            // A card coming or going changes these; PipeWire follows its own default behind
            // ALSA's `default`, so a Bluetooth headset there needs no reselect from us.
            let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
            return format!(
                "{}\n{}",
                read("/proc/asound/cards"),
                read("/proc/asound/pcm")
            );
        }
        let host = cpal::default_host();
        let mut all: Vec<String> = host
            .devices()
            .map(|ds| ds.filter_map(|d| d.name().ok()).collect())
            .unwrap_or_default();
        all.sort();
        format!(
            "{}|in={}|out={}",
            all.join(","),
            Self::default_name(AudioDirection::Input).unwrap_or_default(),
            Self::default_name(AudioDirection::Output).unwrap_or_default()
        )
    }

    fn chime(&self, device: &AudioDevice) -> Result<(), String> {
        let floor = pond_audio_out::bus()
            .claim(Claim::Chime, Some(CHIME_WAITS_FOR_BUS), || true)
            .ok_or_else(|| "the speaker is busy".to_string())?;
        let cards = Self::cards();
        let _suspended = ALSA.then(|| {
            sound_server::suspend_for_devices(&SystemPactl, &cards, None, Some(&device.id))
        });
        pond_audio_out::route::play_chime(&floor, Some(&device.id)).map_err(|e| e.to_string())
    }

    // Linux only: on a Mac the person sets the volume and the defaults.
    fn raise_levels(&self, speaker: &AudioDevice, mic: &AudioDevice) -> Vec<LevelChange> {
        if !ALSA {
            return Vec::new();
        }
        sound_server::raise_levels(&SystemPactl, &Self::cards(), speaker, mic)
    }

    fn restore_levels(&self, changes: &[LevelChange]) {
        sound_server::restore_levels(&SystemPactl, changes);
    }

    fn set_system_defaults(
        &self,
        input: Option<&AudioDevice>,
        output: Option<&AudioDevice>,
    ) -> Option<SoundServerDefaults> {
        if !ALSA {
            return None;
        }
        sound_server::set_defaults(&SystemPactl, &Self::cards(), input, output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::services::voice::audio_devices::{
        AudioDeviceSelector, ProbeMode, SelectRequest,
    };
    use std::sync::Arc;

    /// Real hardware: `cargo test -p pond-adapters-audio-devices -- --ignored --nocapture`.
    #[test]
    #[ignore = "opens this machine's real microphones and speakers"]
    fn live_probe_on_this_machine() {
        let prober = Arc::new(CpalProber::new(None));
        println!("fingerprint: {}", prober.fingerprint());
        for direction in [AudioDirection::Input, AudioDirection::Output] {
            for device in prober.devices(direction).unwrap() {
                let t = std::time::Instant::now();
                let outcome = prober.probe(&device, direction, Duration::from_millis(1_200));
                println!(
                    "{direction:?} {device:?} -> {outcome:?} in {:?}",
                    t.elapsed()
                );
            }
            let selector = AudioDeviceSelector::new(prober.clone());
            let t = std::time::Instant::now();
            let choice = selector.select(&SelectRequest {
                direction,
                pinned: None,
                mode: ProbeMode::Lazy,
                exclude: &[],
                mic_enabled: true,
            });
            println!("{direction:?} lazy choice: {choice} in {:?}", t.elapsed());
            assert!(choice.device.is_some());
        }
        // Every pair of what is known without listing: on a board, the default and the USB card.
        for speaker in prober.quick_list(AudioDirection::Output) {
            for mic in prober.quick_list(AudioDirection::Input) {
                let t = std::time::Instant::now();
                let heard = prober.hears(&speaker, &mic);
                println!(
                    "sound test {} -> {}: {heard:?} in {:?}",
                    speaker.id,
                    mic.id,
                    t.elapsed()
                );
            }
        }
        let t = std::time::Instant::now();
        let pair = AudioDeviceSelector::new(prober.clone()).select_by_sound(
            &pond_core::models::services::voice::audio_devices::PairRequest {
                input_pin: None,
                output_pin: None,
                mode: ProbeMode::Lazy,
                exclude: &[],
                // Changes the board's levels, and keeps them if that is what got the sound heard.
                raise_levels: std::env::var("GIAP_TEST_RAISE").is_ok(),
            },
        );
        match &pair {
            pond_core::models::services::voice::audio_devices::SoundTest::Heard {
                input,
                output,
                raised,
            } => {
                println!(
                    "by sound in {:?}: speaker {output} / mic {input}; raised {raised:?}",
                    t.elapsed()
                )
            }
            other => println!("by sound in {:?}: {other:?}", t.elapsed()),
        }
    }
}
