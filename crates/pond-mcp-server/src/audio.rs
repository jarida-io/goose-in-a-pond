//! Switching the pond's speaker or microphone when asked: what was said, matched to a device the
//! pond knows, and the replies. The switch itself is the same pin the settings pickers write.

use std::sync::{Arc, OnceLock};

use pond_core::models::domain::audio_device::{AudioChoice, AudioDevice, AudioDirection};
use pond_core::models::ports::audio_devices::AudioDeviceControl;

static AUDIO_CONTROL: OnceLock<Arc<dyn AudioDeviceControl>> = OnceLock::new();

/// Install the process's audio device control. Call once, where devices are chosen.
pub fn init_audio_control(control: Arc<dyn AudioDeviceControl>) {
    let _ = AUDIO_CONTROL.set(control);
}

pub(crate) fn audio_control() -> Option<Arc<dyn AudioDeviceControl>> {
    AUDIO_CONTROL.get().cloned()
}

pub(crate) fn direction_of(which: &str) -> Option<AudioDirection> {
    match which.trim().to_ascii_lowercase().as_str() {
        "speaker" | "speakers" | "output" | "headphones" | "headset" => {
            Some(AudioDirection::Output)
        }
        "microphone" | "mic" | "input" => Some(AudioDirection::Input),
        _ => None,
    }
}

pub(crate) fn pin_key(direction: AudioDirection) -> &'static str {
    match direction {
        AudioDirection::Input => "audio_input_device",
        AudioDirection::Output => "audio_output_device",
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum Pick {
    Automatic,
    One(AudioDevice),
    Several(Vec<String>),
    Nothing,
}

/// Words that say which kind of device, not which device.
const FILLER: &[&str] = &[
    "a",
    "an",
    "the",
    "my",
    "to",
    "use",
    "please",
    "one",
    "device",
    "speaker",
    "speakers",
    "microphone",
    "mic",
    "audio",
];

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Match what was said against the devices, best-ordered. Devices that share one name (ALSA
/// offers a card under several routes) are one device, and the first route is the one to use.
pub(crate) fn resolve(devices: &[AudioDevice], said: &str) -> Pick {
    let asked: Vec<String> = words(said)
        .into_iter()
        .filter(|w| !FILLER.contains(&w.as_str()))
        .collect();
    if matches!(
        asked.as_slice(),
        [w] if w == "automatic" || w == "auto" || w == "automatically"
    ) {
        return Pick::Automatic;
    }
    let said_lower = said.trim().to_lowercase();
    if let Some(d) = devices
        .iter()
        .find(|d| d.id.to_lowercase() == said_lower || d.label.to_lowercase() == said_lower)
    {
        return Pick::One(d.clone());
    }
    if asked.is_empty() {
        return Pick::Nothing;
    }
    let named = |d: &AudioDevice| {
        let mut known = words(&d.label);
        known.extend(words(&d.id));
        known
    };
    let every: Vec<&AudioDevice> = devices
        .iter()
        .filter(|d| {
            let known = named(d);
            asked.iter().all(|w| known.contains(w))
        })
        .collect();
    let found = if every.is_empty() {
        devices
            .iter()
            .filter(|d| {
                let known = named(d);
                asked.iter().any(|w| known.contains(w))
            })
            .collect()
    } else {
        every
    };
    let mut labels: Vec<String> = Vec::new();
    for d in &found {
        if !labels.contains(&d.label) {
            labels.push(d.label.clone());
        }
    }
    match (found.first(), labels.len()) {
        (Some(d), 1) => Pick::One((*d).clone()),
        (_, 0) => Pick::Nothing,
        _ => Pick::Several(labels),
    }
}

/// Each device once, by name, the one in use marked.
pub(crate) fn listing(choice: &AudioChoice) -> String {
    let noun = choice.direction.noun();
    let in_use = choice.device.as_ref().map(|d| d.label.as_str());
    let mut seen: Vec<&str> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for p in &choice.devices {
        let label = p.device.label.as_str();
        if seen.contains(&label) {
            continue;
        }
        seen.push(label);
        names.push(if Some(label) == in_use {
            format!("{label} (in use)")
        } else {
            label.to_string()
        });
    }
    let pinned = match &choice.pinned {
        Some(pin) => format!("Pinned: {pin}."),
        None => "Chosen automatically.".to_string(),
    };
    if names.is_empty() {
        return format!("No {noun} is known to the pond. {pinned}");
    }
    format!("{}s: {}. {pinned}", capitalized(noun), names.join(", "))
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// What happened after a switch, as the pond's own choice reports it.
pub(crate) fn outcome(choice: &AudioChoice, asked_for: Option<&AudioDevice>) -> String {
    let noun = choice.direction.noun();
    let now = format!("The {noun} is now {choice}.");
    let problem = choice
        .problem()
        .map(|p| format!(" Problem: {p}."))
        .unwrap_or_default();
    match asked_for {
        Some(d) if choice.device_id() != Some(d.id.as_str()) => format!(
            "{} did not work, so it stays pinned and the pond uses another for now. {now}{problem}",
            d.label
        ),
        _ => format!("{now}{problem}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::audio_device::{choose, DeviceKind, ProbeOutcome, ProbedDevice};

    fn dev(id: &str, label: &str) -> AudioDevice {
        let plugged = id.contains("CARD=Device");
        let mut d = AudioDevice::new(id, plugged, DeviceKind::Hardware);
        d.label = label.to_string();
        d
    }

    /// The nano's speakers, as the pond lists them.
    fn nano() -> Vec<AudioDevice> {
        vec![
            dev("sysdefault:CARD=Device", "USB PnP Audio Device"),
            dev("plughw:CARD=Device,DEV=0", "USB PnP Audio Device"),
            dev("default", "System default"),
            dev("sysdefault:CARD=APE", "NVIDIA Jetson Orin Nano APE"),
        ]
    }

    #[test]
    fn a_spoken_name_finds_its_device_and_the_first_route_to_it() {
        let Pick::One(d) = resolve(&nano(), "the USB speaker") else {
            panic!("expected the USB card");
        };
        assert_eq!(d.id, "sysdefault:CARD=Device");
        let Pick::One(d) = resolve(&nano(), "jetson") else {
            panic!("expected the onboard path");
        };
        assert_eq!(d.id, "sysdefault:CARD=APE");
    }

    #[test]
    fn automatic_goes_back_to_letting_the_pond_choose() {
        assert_eq!(resolve(&nano(), "automatic"), Pick::Automatic);
        assert_eq!(resolve(&nano(), "Auto"), Pick::Automatic);
    }

    #[test]
    fn an_exact_id_from_the_listing_is_taken_as_it_is() {
        let Pick::One(d) = resolve(&nano(), "plughw:CARD=Device,DEV=0") else {
            panic!("expected the exact route");
        };
        assert_eq!(d.id, "plughw:CARD=Device,DEV=0");
    }

    #[test]
    fn an_unknown_or_vague_name_is_not_guessed() {
        assert_eq!(resolve(&nano(), "the headphones"), Pick::Nothing);
        assert_eq!(resolve(&nano(), "speaker"), Pick::Nothing);
        let mac = vec![
            dev("MacBook Air Speakers", "MacBook Air Speakers"),
            dev("AirPods Pro", "AirPods Pro"),
            dev("Studio Display Speakers", "Studio Display Speakers"),
        ];
        assert_eq!(
            resolve(&mac, "display or macbook"),
            Pick::Several(vec![
                "MacBook Air Speakers".to_string(),
                "Studio Display Speakers".to_string()
            ])
        );
        let Pick::One(d) = resolve(&mac, "airpods") else {
            panic!("expected the AirPods");
        };
        assert_eq!(d.id, "AirPods Pro");
    }

    #[test]
    fn the_listing_names_each_device_once_and_marks_the_one_in_use() {
        let probed: Vec<ProbedDevice> = nano()
            .into_iter()
            .map(|d| ProbedDevice::new(d, ProbeOutcome::Accepting))
            .collect();
        let choice = choose(AudioDirection::Output, probed, None, true);
        assert_eq!(
            listing(&choice),
            "Speakers: USB PnP Audio Device (in use), System default, NVIDIA Jetson Orin Nano \
             APE. Chosen automatically."
        );
    }

    #[test]
    fn a_pin_that_did_not_work_is_said_plainly() {
        let probed = vec![
            ProbedDevice::new(dev("default", "System default"), ProbeOutcome::Accepting),
            ProbedDevice::new(
                dev("sysdefault:CARD=APE", "NVIDIA Jetson Orin Nano APE"),
                ProbeOutcome::Failed {
                    reason: "busy".into(),
                },
            ),
        ];
        let choice = choose(
            AudioDirection::Output,
            probed,
            Some("sysdefault:CARD=APE"),
            false,
        );
        let asked = dev("sysdefault:CARD=APE", "NVIDIA Jetson Orin Nano APE");
        let said = outcome(&choice, Some(&asked));
        assert!(
            said.starts_with("NVIDIA Jetson Orin Nano APE did not work"),
            "{said}"
        );
    }
}
