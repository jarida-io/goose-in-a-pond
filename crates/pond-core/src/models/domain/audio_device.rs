//! Which microphone and speaker the pond uses, what a brief test of each found, and why one was
//! chosen. Pure: opening devices is the job of [`crate::models::ports::audio_devices`].

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioDirection {
    Input,
    Output,
}

impl AudioDirection {
    pub fn noun(self) -> &'static str {
        match self {
            Self::Input => "microphone",
            Self::Output => "speaker",
        }
    }
}

/// How a device reaches sound, as far as the adapter can tell. Decides who is tried first when
/// neither the household's pin nor the system default works.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// A sound server's shared endpoint (PipeWire, PulseAudio, ALSA `default`): it follows the
    /// household's own default and never locks another program out of the hardware.
    SoundServer,
    /// A physical device opened directly.
    #[default]
    Hardware,
    /// A Bluetooth headset or speaker. The sound server carries it, so it is reached through the
    /// server's default route, and it is worn or carried, so no room microphone can vouch for it.
    Bluetooth,
    /// A hardware node opened directly (ALSA `hw:`, `plughw:`): it locks the card while open, so
    /// nothing else can play or record on it meanwhile.
    Raw,
    /// Software that carries no sound until something is routed into it: loopbacks, meeting-app
    /// drivers, aggregates. Chosen last because it opens fine and then delivers silence.
    Virtual,
}

impl DeviceKind {
    fn rank(self) -> u8 {
        match self {
            Self::SoundServer => 0,
            Self::Hardware | Self::Bluetooth => 1,
            Self::Raw => 2,
            Self::Virtual => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    /// What the platform calls it, and what a pin is stored as.
    pub id: String,
    /// What a person sees; the same as `id` where the platform names devices for people.
    pub label: String,
    pub is_default: bool,
    pub kind: DeviceKind,
    /// Plugged in by the household (a USB card), as opposed to built into the board.
    #[serde(default)]
    pub plugged_in: bool,
}

impl AudioDevice {
    pub fn new(id: impl Into<String>, is_default: bool, kind: DeviceKind) -> Self {
        let id = id.into();
        Self {
            label: id.clone(),
            id,
            is_default,
            kind,
            plugged_in: false,
        }
    }

    pub fn plugged_in(mut self) -> Self {
        self.plugged_in = true;
        self
    }

    pub fn with_kind(mut self, kind: DeviceKind) -> Self {
        self.kind = kind;
        self
    }
}

/// What briefly opening a device found. Nothing a microphone hears is kept: only its level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ProbeOutcome {
    /// Microphone: frames arrived carrying sound, at this RMS level.
    Hearing { level_dbfs: f32 },
    /// Microphone: frames arrived, all of them digital silence. A mute switch, a withheld
    /// microphone permission, or a virtual device nothing feeds.
    Silent,
    /// Speaker: it opened and took a buffer of silence.
    Accepting,
    /// It opened, but no frames moved within the time allowed.
    Stalled,
    /// It could not be opened.
    Failed { reason: String },
    /// Not tried: an earlier device already worked, or the microphone is off.
    Untested,
}

impl ProbeOutcome {
    /// Opened and moved frames. A silent microphone works; whether it hears is another matter.
    pub fn works(&self) -> bool {
        matches!(self, Self::Hearing { .. } | Self::Silent | Self::Accepting)
    }

    /// Shown to be doing its job: a microphone hearing sound, a speaker taking audio.
    pub fn proven(&self) -> bool {
        matches!(self, Self::Hearing { .. } | Self::Accepting)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbedDevice {
    #[serde(flatten)]
    pub device: AudioDevice,
    pub outcome: ProbeOutcome,
}

impl ProbedDevice {
    pub fn new(device: AudioDevice, outcome: ProbeOutcome) -> Self {
        Self { device, outcome }
    }
}

/// Why the chosen device is the one in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChoiceBasis {
    /// The household pinned it, and it works.
    Pinned,
    /// Nothing is pinned, and the system's own default works.
    SystemDefault,
    /// Nothing is pinned, and a device the household plugged in works. Tried ahead of the
    /// default where a sound server's priority, not a person, chose the default.
    PluggedIn,
    /// Nothing is pinned, and this speaker played a test sound that this microphone heard: the
    /// first pair, in order of preference, shown to work together.
    Heard,
    /// The pin or the default did not work, so the best device that does was taken instead.
    Fallback,
    /// No device proved itself; this one is used anyway so it can start working, and the
    /// problem is reported.
    BestEffort,
    /// There is no device in this direction at all.
    NoDevice,
    /// The microphone is off in Settings, so no input was opened, not even to test it.
    MicOff,
    /// Not tested yet: this process does not open the microphone until voice starts or the
    /// household asks for a test.
    NotTested,
}

/// The device a direction uses, why, and everything that was considered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioChoice {
    pub direction: AudioDirection,
    pub device: Option<AudioDevice>,
    pub outcome: ProbeOutcome,
    pub basis: ChoiceBasis,
    /// The household's pin, whether or not it is connected.
    pub pinned: Option<String>,
    /// Every device considered, in the order tried; untried ones read `Untested`.
    pub devices: Vec<ProbedDevice>,
    /// The device at the other end of the sound test that proved this one, by label.
    #[serde(default)]
    pub heard_with: Option<String>,
    /// A sound test ran and no microphone heard any speaker, so this is a silent check's pick.
    #[serde(default)]
    pub unheard: bool,
}

/// What one sound test found: a speaker played a short sound while a microphone listened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PairOutcome {
    /// The microphone heard it, `margin_db` above the room, which measured `room_dbfs`.
    Heard {
        margin_db: f32,
        room_dbfs: f32,
    },
    /// Both opened, and the microphone did not hear it: nothing plugged in, muted, too quiet.
    NotHeard,
    Failed {
        reason: String,
    },
}

/// The order to try speaker and microphone pairs in, as indexes into each one's own order: both
/// first choices together, then each first choice with the other's next best, and so on.
pub fn pair_order(speakers: usize, mics: usize) -> Vec<(usize, usize)> {
    let mut pairs: Vec<(usize, usize)> = (0..speakers)
        .flat_map(|s| (0..mics).map(move |m| (s, m)))
        .collect();
    pairs.sort_by_key(|&(s, m)| (s + m, s));
    pairs
}

impl AudioChoice {
    pub fn mic_off(pinned: Option<&str>) -> Self {
        Self {
            direction: AudioDirection::Input,
            device: None,
            outcome: ProbeOutcome::Untested,
            basis: ChoiceBasis::MicOff,
            pinned: pinned.map(str::to_string),
            devices: Vec::new(),
            heard_with: None,
            unheard: false,
        }
    }

    /// What is known without opening anything, for a process that must not touch the device.
    pub fn not_tested(
        direction: AudioDirection,
        devices: Vec<AudioDevice>,
        pinned: Option<&str>,
        plugged_in_first: bool,
    ) -> Self {
        let pinned = pinned
            .map(str::trim)
            .filter(|p| !p.is_empty() && !p.eq_ignore_ascii_case("auto"));
        let ordered: Vec<ProbedDevice> = probe_order(&devices, pinned, plugged_in_first)
            .into_iter()
            .map(|d| ProbedDevice::new(d.clone(), ProbeOutcome::Untested))
            .collect();
        Self {
            direction,
            device: ordered.first().map(|p| p.device.clone()),
            outcome: ProbeOutcome::Untested,
            basis: ChoiceBasis::NotTested,
            pinned: pinned.map(str::to_string),
            devices: ordered,
            heard_with: None,
            unheard: false,
        }
    }

    pub fn device_id(&self) -> Option<&str> {
        self.device.as_ref().map(|d| d.id.as_str())
    }

    /// A problem worth telling the household about, or `None` when the device is doing its job.
    pub fn problem(&self) -> Option<String> {
        let noun = self.direction.noun();
        let label = self.device.as_ref().map(|d| d.label.as_str()).unwrap_or("");
        match (self.basis, &self.outcome) {
            (ChoiceBasis::MicOff | ChoiceBasis::NotTested, _) => None,
            _ if self.unheard && self.direction == AudioDirection::Output => Some(format!(
                "no microphone heard the speaker '{label}' play its test sound: check that it is \
                 switched on and turned up"
            )),
            (ChoiceBasis::NoDevice, _) => Some(format!("no {noun} was found")),
            (_, ProbeOutcome::Silent) => Some(format!(
                "the {noun} '{label}' hears only silence: check its mute switch and the \
                 microphone permission"
            )),
            (ChoiceBasis::BestEffort, ProbeOutcome::Failed { reason }) => Some(format!(
                "no {noun} works; '{label}' failed to open ({reason})"
            )),
            (ChoiceBasis::BestEffort, _) => Some(format!(
                "no {noun} answered a test; trying '{label}' anyway"
            )),
            _ => None,
        }
    }
}

impl std::fmt::Display for AudioChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let noun = self.direction.noun();
        let Some(device) = &self.device else {
            return match self.basis {
                ChoiceBasis::MicOff => write!(f, "microphone off in Settings"),
                ChoiceBasis::NotTested => write!(f, "{noun} not tested yet"),
                _ => write!(f, "no {noun} found"),
            };
        };
        let why = match self.basis {
            ChoiceBasis::Pinned => "pinned".to_string(),
            ChoiceBasis::SystemDefault => "system default".to_string(),
            ChoiceBasis::PluggedIn => "plugged in".to_string(),
            ChoiceBasis::Heard => "heard together".to_string(),
            ChoiceBasis::Fallback => match &self.pinned {
                Some(pin) if pin != &device.id => format!("'{pin}' unavailable"),
                _ => "default unavailable".to_string(),
            },
            ChoiceBasis::BestEffort => "nothing passed a test".to_string(),
            ChoiceBasis::NotTested => "not tested yet".to_string(),
            ChoiceBasis::NoDevice | ChoiceBasis::MicOff => String::new(),
        };
        let found = match &self.outcome {
            ProbeOutcome::Hearing { level_dbfs } => {
                format!("hearing sound at {level_dbfs:.0} dBFS")
            }
            ProbeOutcome::Silent => "silent".to_string(),
            ProbeOutcome::Accepting => "accepting audio".to_string(),
            ProbeOutcome::Stalled => "stalled".to_string(),
            ProbeOutcome::Failed { reason } => format!("failed: {reason}"),
            ProbeOutcome::Untested => "untested".to_string(),
        };
        match &self.heard_with {
            Some(partner) => write!(f, "{} ({why}; {found}; with {partner})", device.label),
            None => write!(f, "{} ({why}; {found})", device.label),
        }
    }
}

/// A software level the pond raised so its test sound was heard: a sound server's volume or
/// mute, never a speaker's own knob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LevelChange {
    /// The device, as the sound server names it.
    pub device: String,
    /// `volume` or `mute`.
    pub what: String,
    pub from: String,
    pub to: String,
    /// The adapter's way back, if raising it did not help; it never leaves the process.
    #[serde(skip)]
    pub undo: Vec<String>,
}

impl std::fmt::Display for LevelChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} from {} to {}",
            self.device, self.what, self.from, self.to
        )
    }
}

/// What the pond told the board's sound server, so every program on the board shares the pair
/// the pond chose. Linux boards only: on a Mac the person picks the defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoundServerDefaults {
    /// The server, as it names itself (`pulseaudio`).
    pub server: String,
    /// The server's own names for the speaker and microphone that are now its defaults.
    pub speaker: Option<String>,
    pub mic: Option<String>,
    #[serde(default)]
    pub problems: Vec<String>,
}

/// The device name to open for live audio: the sound server's default route when the server now
/// sends this direction to the chosen card (so the pond shares the card with other programs
/// instead of holding it through ALSA), or the chosen device itself.
pub fn stream_route<'a>(
    choice: &'a AudioChoice,
    sound_server: Option<&SoundServerDefaults>,
    exclude: &[String],
) -> Option<&'a str> {
    let chosen = choice.device.as_ref()?;
    let on_server = match choice.direction {
        AudioDirection::Output => sound_server.and_then(|s| s.speaker.as_ref()).is_some(),
        AudioDirection::Input => sound_server.and_then(|s| s.mic.as_ref()).is_some(),
    };
    if on_server && chosen.kind != DeviceKind::Raw {
        if let Some(server) = choice.devices.iter().find(|p| {
            p.device.kind == DeviceKind::SoundServer
                && p.device.is_default
                && !exclude.iter().any(|e| e == &p.device.id)
                && !matches!(
                    p.outcome,
                    ProbeOutcome::Failed { .. } | ProbeOutcome::Stalled
                )
        }) {
            return Some(&server.device.id);
        }
    }
    Some(&chosen.id)
}

/// What the pond is using right now, as written to disk for the other process and the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioDeviceStatus {
    pub input: AudioChoice,
    pub output: AudioChoice,
    /// Unix milliseconds of the selection this describes.
    pub updated_at_ms: u64,
    /// `voice` when the voice session chose, which owns the microphone; `serve` otherwise.
    pub source: String,
    pub pid: u32,
    /// Levels this choice raised so the test sound was heard.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raised: Vec<LevelChange>,
    /// Set when the household lets the pond make its choice the board's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound_server: Option<SoundServerDefaults>,
}

impl AudioDeviceStatus {
    /// A microphone and speaker shown to work together: a pair the sound test heard, or, where
    /// no sound test ran, a speaker that took audio and a microphone that heard the room.
    pub fn pair_works(&self) -> bool {
        if self.input.heard_with.is_some() && self.output.heard_with.is_some() {
            return true;
        }
        let chosen = |c: &AudioChoice| {
            c.device.is_some()
                && !matches!(
                    c.basis,
                    ChoiceBasis::NoDevice
                        | ChoiceBasis::MicOff
                        | ChoiceBasis::NotTested
                        | ChoiceBasis::BestEffort
                )
        };
        !self.output.unheard
            && chosen(&self.input)
            && chosen(&self.output)
            && self.input.outcome.proven()
            && self.output.outcome.works()
    }
}

fn capability_priority(d: &AudioDevice) -> u8 {
    let lower = format!("{} {}", d.label, d.id).to_ascii_lowercase();
    if lower.contains("array")
        || lower.contains("respeaker")
        || lower.contains("c920")
        || lower.contains("c922")
        || lower.contains("c930")
        || lower.contains("brio")
        || lower.contains("conference")
        || lower.contains("jabra")
        || lower.contains("anker")
        || lower.contains("poly")
        || lower.contains("yeti")
        || lower.contains("quadcast")
        || lower.contains("shure")
        || lower.contains("rode")
        || lower.contains("webcam")
    {
        0
    } else if lower.contains("pnp") || lower.contains("generic") || lower.contains("dongle") {
        2
    } else {
        1
    }
}

/// The order to try devices in: the pin, then the system default, then the rest best-first.
/// With `plugged_in_first`, plugged-in devices go ahead of the default: on a board the sound
/// server picks its default by priority, while the household plugged the USB card in on purpose.
/// Enumeration order breaks ties, so a platform's own ordering survives.
pub fn probe_order<'a>(
    devices: &'a [AudioDevice],
    pinned: Option<&str>,
    plugged_in_first: bool,
) -> Vec<&'a AudioDevice> {
    let clean_pin = pinned
        .map(str::trim)
        .filter(|p| !p.is_empty() && !p.eq_ignore_ascii_case("auto"));
    let mut order: Vec<(usize, &AudioDevice)> = devices.iter().enumerate().collect();
    order.sort_by_key(|(i, d)| {
        let tier = if clean_pin == Some(d.id.as_str()) {
            0
        } else if plugged_in_first && d.plugged_in && d.kind != DeviceKind::Virtual {
            1 + d.kind.rank()
        } else if d.is_default {
            5
        } else {
            6 + d.kind.rank()
        };
        (tier, capability_priority(d), *i)
    });
    order.into_iter().map(|(_, d)| d).collect()
}

/// Whether trying further devices can change the choice: a working pin, or any proven device.
pub fn settles(device: &AudioDevice, outcome: &ProbeOutcome, pinned: Option<&str>) -> bool {
    let clean_pin = pinned
        .map(str::trim)
        .filter(|p| !p.is_empty() && !p.eq_ignore_ascii_case("auto"));
    if clean_pin == Some(device.id.as_str()) {
        return outcome.works();
    }
    outcome.proven()
}

/// Choose a device from what was probed. A working pin always wins, even one that hears only
/// silence, because the household chose it; otherwise the first proven device in
/// [`probe_order`], then the first that merely works, then the pin or default anyway.
pub fn choose(
    direction: AudioDirection,
    devices: Vec<ProbedDevice>,
    pinned: Option<&str>,
    plugged_in_first: bool,
) -> AudioChoice {
    let pinned = pinned
        .map(str::trim)
        .filter(|p| !p.is_empty() && !p.eq_ignore_ascii_case("auto"));
    let plain: Vec<AudioDevice> = devices.iter().map(|p| p.device.clone()).collect();
    let ordered: Vec<&ProbedDevice> = probe_order(&plain, pinned, plugged_in_first)
        .into_iter()
        .filter_map(|d| devices.iter().find(|p| p.device.id == d.id))
        .collect();

    let pick = |p: &ProbedDevice, basis: ChoiceBasis| AudioChoice {
        direction,
        device: Some(p.device.clone()),
        outcome: p.outcome.clone(),
        basis,
        pinned: pinned.map(str::to_string),
        devices: ordered.iter().map(|p| (*p).clone()).collect(),
        heard_with: None,
        unheard: false,
    };

    if let Some(pin) = ordered
        .iter()
        .find(|p| Some(p.device.id.as_str()) == pinned && p.outcome.works())
    {
        return pick(pin, ChoiceBasis::Pinned);
    }
    if let Some(best) = ordered.iter().find(|p| p.outcome.proven()) {
        let basis = if pinned.is_some() {
            ChoiceBasis::Fallback
        } else if plugged_in_first && best.device.plugged_in {
            ChoiceBasis::PluggedIn
        } else if best.device.is_default {
            ChoiceBasis::SystemDefault
        } else {
            ChoiceBasis::Fallback
        };
        return pick(best, basis);
    }
    if let Some(working) = ordered.iter().find(|p| p.outcome.works()) {
        return pick(working, ChoiceBasis::BestEffort);
    }
    // Nothing opened. Keep the pin or default anyway: the capture retries it, and a device that
    // was merely slow to wake answers there.
    if let Some(first) = ordered.first() {
        return pick(first, ChoiceBasis::BestEffort);
    }
    AudioChoice {
        direction,
        device: None,
        outcome: ProbeOutcome::Untested,
        basis: ChoiceBasis::NoDevice,
        pinned: pinned.map(str::to_string),
        devices: Vec::new(),
        heard_with: None,
        unheard: false,
    }
}

/// The dBFS reading of an RMS level, floored so digital silence does not print as infinity.
pub fn dbfs(rms: f32) -> f32 {
    if rms <= 1e-10 {
        -200.0
    } else {
        20.0 * rms.log10()
    }
}

/// Below this peak a microphone is delivering digital silence, not a quiet room: a real
/// converter's own noise sits far above it.
pub const SILENCE_PEAK: f32 = 1e-5;

/// Read a probe's measurements as an outcome. `frames == 0` means nothing ever arrived.
pub fn classify_input(frames: u64, peak: f32, rms: f32) -> ProbeOutcome {
    if frames == 0 {
        ProbeOutcome::Stalled
    } else if peak < SILENCE_PEAK {
        ProbeOutcome::Silent
    } else {
        ProbeOutcome::Hearing {
            level_dbfs: dbfs(rms),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IN: AudioDirection = AudioDirection::Input;
    const OUT: AudioDirection = AudioDirection::Output;

    fn dev(id: &str, is_default: bool, kind: DeviceKind) -> AudioDevice {
        AudioDevice::new(id, is_default, kind)
    }
    fn hearing() -> ProbeOutcome {
        ProbeOutcome::Hearing { level_dbfs: -58.0 }
    }
    fn failed() -> ProbeOutcome {
        ProbeOutcome::Failed {
            reason: "device busy".into(),
        }
    }
    fn probed(id: &str, is_default: bool, outcome: ProbeOutcome) -> ProbedDevice {
        ProbedDevice::new(dev(id, is_default, DeviceKind::Hardware), outcome)
    }

    fn status(input: AudioChoice, output: AudioChoice) -> AudioDeviceStatus {
        AudioDeviceStatus {
            input,
            output,
            updated_at_ms: 0,
            source: "serve".into(),
            pid: 1,
            raised: Vec::new(),
            sound_server: None,
        }
    }

    #[test]
    fn a_room_needs_a_pair_that_works_together() {
        let mic = choose(IN, vec![probed("USB mic", true, hearing())], None, false);
        let speaker = choose(
            OUT,
            vec![probed("USB speaker", true, ProbeOutcome::Accepting)],
            None,
            false,
        );
        assert!(
            status(mic.clone(), speaker.clone()).pair_works(),
            "no sound test ran: two devices that each work"
        );

        let mut unheard = speaker.clone();
        unheard.unheard = true;
        assert!(
            !status(mic.clone(), unheard).pair_works(),
            "the sound test ran and nothing heard the speaker"
        );

        let silent = choose(
            IN,
            vec![probed("USB mic", true, ProbeOutcome::Silent)],
            None,
            false,
        );
        assert!(!status(silent, speaker.clone()).pair_works());

        let untested = AudioChoice::not_tested(
            IN,
            vec![dev("USB mic", true, DeviceKind::Hardware)],
            None,
            false,
        );
        assert!(!status(untested, speaker.clone()).pair_works());
        assert!(!status(AudioChoice::mic_off(None), speaker.clone()).pair_works());

        let (mut heard_in, mut heard_out) = (mic, speaker);
        heard_in.heard_with = Some("USB speaker".into());
        heard_out.heard_with = Some("USB mic".into());
        assert!(status(heard_in, heard_out).pair_works());
    }

    #[test]
    fn a_working_default_is_used_when_nothing_is_pinned() {
        let c = choose(
            IN,
            vec![
                probed("USB mic", false, hearing()),
                probed("Built-in", true, hearing()),
            ],
            None,
            false,
        );
        assert_eq!(c.device_id(), Some("Built-in"));
        assert_eq!(c.basis, ChoiceBasis::SystemDefault);
        assert_eq!(c.problem(), None);
    }

    #[test]
    fn a_working_pin_beats_the_default() {
        let c = choose(
            IN,
            vec![
                probed("Built-in", true, hearing()),
                probed("USB mic", false, hearing()),
            ],
            Some("USB mic"),
            false,
        );
        assert_eq!(c.device_id(), Some("USB mic"));
        assert_eq!(c.basis, ChoiceBasis::Pinned);
    }

    /// The household chose it; a mute switch is theirs to flip, so keep it and say so.
    #[test]
    fn a_silent_pin_still_wins_and_reports_the_silence() {
        let c = choose(
            IN,
            vec![
                probed("Built-in", true, hearing()),
                probed("Headset", false, ProbeOutcome::Silent),
            ],
            Some("Headset"),
            false,
        );
        assert_eq!(c.device_id(), Some("Headset"));
        assert_eq!(c.basis, ChoiceBasis::Pinned);
        let problem = c.problem().expect("silence is a problem worth saying");
        assert!(problem.contains("mute"), "{problem}");
    }

    #[test]
    fn an_unplugged_pin_falls_back_to_the_default() {
        let c = choose(
            IN,
            vec![probed("Built-in", true, hearing())],
            Some("USB mic"),
            false,
        );
        assert_eq!(c.device_id(), Some("Built-in"));
        assert_eq!(c.basis, ChoiceBasis::Fallback);
        assert_eq!(c.pinned.as_deref(), Some("USB mic"));
        assert!(c.to_string().contains("'USB mic' unavailable"), "{c}");
    }

    #[test]
    fn a_pin_that_fails_to_open_falls_back() {
        let c = choose(
            OUT,
            vec![
                probed("Speakers", true, ProbeOutcome::Accepting),
                probed("AirPods", false, failed()),
            ],
            Some("AirPods"),
            false,
        );
        assert_eq!(c.device_id(), Some("Speakers"));
        assert_eq!(c.basis, ChoiceBasis::Fallback);
    }

    /// The case that made voice "just stop": a default that opens and hears nothing.
    #[test]
    fn a_silent_default_gives_way_to_a_device_that_hears() {
        let c = choose(
            IN,
            vec![
                probed("Built-in", true, ProbeOutcome::Silent),
                probed("USB mic", false, hearing()),
            ],
            None,
            false,
        );
        assert_eq!(c.device_id(), Some("USB mic"));
        assert_eq!(c.basis, ChoiceBasis::Fallback);
        assert_eq!(c.problem(), None);
    }

    /// A withheld macOS microphone permission silences every input at once.
    #[test]
    fn when_every_mic_is_silent_the_default_is_kept_and_the_silence_reported() {
        let c = choose(
            IN,
            vec![
                probed("Phone mic", false, ProbeOutcome::Silent),
                probed("Built-in", true, ProbeOutcome::Silent),
            ],
            None,
            false,
        );
        assert_eq!(c.device_id(), Some("Built-in"));
        assert_eq!(c.basis, ChoiceBasis::BestEffort);
        assert!(c.problem().unwrap().contains("permission"));
    }

    #[test]
    fn when_nothing_opens_the_default_is_kept_for_the_capture_to_retry() {
        let c = choose(
            IN,
            vec![
                probed("Other", false, failed()),
                probed("Built-in", true, failed()),
            ],
            None,
            false,
        );
        assert_eq!(c.device_id(), Some("Built-in"));
        assert_eq!(c.basis, ChoiceBasis::BestEffort);
        assert!(c.problem().unwrap().contains("failed to open"));
    }

    #[test]
    fn no_devices_means_no_choice_and_a_clear_problem() {
        let c = choose(OUT, vec![], None, false);
        assert_eq!(c.device, None);
        assert_eq!(c.basis, ChoiceBasis::NoDevice);
        assert_eq!(c.problem().as_deref(), Some("no speaker was found"));
    }

    #[test]
    fn a_stalled_speaker_is_passed_over_for_one_that_accepts() {
        let c = choose(
            OUT,
            vec![
                probed("HDMI", true, ProbeOutcome::Stalled),
                probed("USB speaker", false, ProbeOutcome::Accepting),
            ],
            None,
            false,
        );
        assert_eq!(c.device_id(), Some("USB speaker"));
    }

    #[test]
    fn untested_devices_are_never_chosen_over_a_proven_one() {
        let c = choose(
            IN,
            vec![
                probed("Built-in", true, ProbeOutcome::Untested),
                probed("USB mic", false, hearing()),
            ],
            None,
            false,
        );
        assert_eq!(c.device_id(), Some("USB mic"));
    }

    #[test]
    fn a_blank_pin_means_automatic() {
        let c = choose(
            IN,
            vec![probed("Built-in", true, hearing())],
            Some("  "),
            false,
        );
        assert_eq!(c.basis, ChoiceBasis::SystemDefault);
        assert_eq!(c.pinned, None);
    }

    #[test]
    fn the_order_is_pin_then_default_then_sound_servers_then_virtual_last() {
        let devices = vec![
            dev("BlackHole 2ch", false, DeviceKind::Virtual),
            dev("hw:CARD=Device,DEV=0", false, DeviceKind::Raw),
            dev("plughw:CARD=Device,DEV=0", false, DeviceKind::Hardware),
            dev("pipewire", false, DeviceKind::SoundServer),
            dev("default", true, DeviceKind::SoundServer),
            dev("USB mic", false, DeviceKind::Hardware),
        ];
        let ids: Vec<&str> = probe_order(&devices, Some("USB mic"), false)
            .into_iter()
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "USB mic",
                "default",
                "pipewire",
                "plughw:CARD=Device,DEV=0",
                "hw:CARD=Device,DEV=0",
                "BlackHole 2ch",
            ]
        );
    }

    #[test]
    fn a_virtual_device_that_hears_nothing_loses_to_real_hardware() {
        let c = choose(
            IN,
            vec![
                ProbedDevice::new(
                    dev("ZoomAudioDevice", false, DeviceKind::Virtual),
                    ProbeOutcome::Silent,
                ),
                ProbedDevice::new(dev("Built-in", true, DeviceKind::Hardware), failed()),
                ProbedDevice::new(dev("USB mic", false, DeviceKind::Hardware), hearing()),
            ],
            None,
            false,
        );
        assert_eq!(c.device_id(), Some("USB mic"));
    }

    #[test]
    fn a_working_pin_settles_the_search_and_a_silent_default_does_not() {
        let pin = dev("Headset", false, DeviceKind::Hardware);
        let default = dev("Built-in", true, DeviceKind::Hardware);
        assert!(settles(&pin, &ProbeOutcome::Silent, Some("Headset")));
        assert!(!settles(&default, &ProbeOutcome::Silent, Some("Headset")));
        assert!(!settles(&default, &ProbeOutcome::Silent, None));
        assert!(settles(&default, &hearing(), None));
        assert!(settles(&default, &ProbeOutcome::Accepting, None));
    }

    #[test]
    fn a_probe_is_read_without_keeping_any_audio() {
        assert_eq!(classify_input(0, 0.0, 0.0), ProbeOutcome::Stalled);
        assert_eq!(classify_input(4_800, 0.0, 0.0), ProbeOutcome::Silent);
        assert_eq!(classify_input(4_800, 4e-6, 1e-6), ProbeOutcome::Silent);
        match classify_input(4_800, 0.01, 0.001) {
            ProbeOutcome::Hearing { level_dbfs } => assert!((level_dbfs + 60.0).abs() < 0.1),
            other => panic!("a quiet room is not silence: {other:?}"),
        }
    }

    #[test]
    fn an_untested_choice_names_the_device_that_would_be_tried_first() {
        let c = AudioChoice::not_tested(
            IN,
            vec![
                dev("Phone", false, DeviceKind::Hardware),
                dev("Built-in", true, DeviceKind::Hardware),
            ],
            None,
            false,
        );
        assert_eq!(c.device_id(), Some("Built-in"));
        assert_eq!(c.basis, ChoiceBasis::NotTested);
        assert_eq!(c.problem(), None);
        assert!(c
            .devices
            .iter()
            .all(|d| d.outcome == ProbeOutcome::Untested));
    }

    #[test]
    fn mic_off_reports_no_problem_and_no_device() {
        let c = AudioChoice::mic_off(Some("USB mic"));
        assert_eq!(c.basis, ChoiceBasis::MicOff);
        assert_eq!(c.device, None);
        assert_eq!(c.problem(), None);
        assert_eq!(c.to_string(), "microphone off in Settings");
    }

    #[test]
    fn the_status_round_trips_through_json_for_the_other_process() {
        let status = AudioDeviceStatus {
            input: choose(IN, vec![probed("Built-in", true, hearing())], None, false),
            output: choose(
                OUT,
                vec![probed("Speakers", true, ProbeOutcome::Accepting)],
                None,
                false,
            ),
            updated_at_ms: 1,
            source: "voice".into(),
            pid: 42,
            raised: Vec::new(),
            sound_server: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"result\":\"hearing\""), "{json}");
        assert!(json.contains("\"basis\":\"system_default\""), "{json}");
        assert!(
            !json.contains("raised") && !json.contains("sound_server"),
            "a status from before these fields reads the same: {json}"
        );
        let back: AudioDeviceStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, status);
    }

    /// The way back is the adapter's, and only in the process that raised the level.
    #[test]
    fn a_raised_level_is_shown_without_how_to_undo_it() {
        let change = LevelChange {
            device: "USB PnP Audio Device Analog Stereo".into(),
            what: "volume".into(),
            from: "51%".into(),
            to: "100%".into(),
            undo: vec!["set-sink-volume".into(), "usb".into(), "33595".into()],
        };
        assert_eq!(
            change.to_string(),
            "USB PnP Audio Device Analog Stereo volume from 51% to 100%"
        );
        let json = serde_json::to_string(&change).unwrap();
        assert!(!json.contains("33595"), "{json}");
    }

    fn usb(id: &str) -> AudioDevice {
        AudioDevice::new(id, false, DeviceKind::Hardware).plugged_in()
    }

    /// The Jetson: PulseAudio defaults to the board's analog path, which nothing is plugged
    /// into and which takes audio all the same. The USB card is the one the household connected.
    #[test]
    fn on_a_board_a_plugged_in_card_beats_a_built_in_default_that_also_works() {
        let devices = vec![
            ProbedDevice::new(
                dev("default", true, DeviceKind::SoundServer),
                ProbeOutcome::Accepting,
            ),
            ProbedDevice::new(usb("sysdefault:CARD=Device"), ProbeOutcome::Accepting),
        ];
        let c = choose(OUT, devices.clone(), None, true);
        assert_eq!(c.device_id(), Some("sysdefault:CARD=Device"));
        assert_eq!(c.basis, ChoiceBasis::PluggedIn);
        assert_eq!(c.problem(), None);

        // Where a person picked the default (CoreAudio), their default stands.
        let c = choose(OUT, devices, None, false);
        assert_eq!(c.device_id(), Some("default"));
        assert_eq!(c.basis, ChoiceBasis::SystemDefault);
    }

    #[test]
    fn a_plugged_in_card_that_fails_gives_way_to_the_default() {
        let c = choose(
            IN,
            vec![
                ProbedDevice::new(dev("default", true, DeviceKind::SoundServer), hearing()),
                ProbedDevice::new(usb("sysdefault:CARD=Device"), failed()),
            ],
            None,
            true,
        );
        assert_eq!(c.device_id(), Some("default"));
        assert_eq!(c.basis, ChoiceBasis::SystemDefault);
    }

    #[test]
    fn a_pin_still_beats_a_plugged_in_card() {
        let c = choose(
            OUT,
            vec![
                ProbedDevice::new(usb("sysdefault:CARD=Device"), ProbeOutcome::Accepting),
                ProbedDevice::new(
                    dev("hdmi:CARD=HDA,DEV=0", false, DeviceKind::Hardware),
                    ProbeOutcome::Accepting,
                ),
            ],
            Some("hdmi:CARD=HDA,DEV=0"),
            true,
        );
        assert_eq!(c.device_id(), Some("hdmi:CARD=HDA,DEV=0"));
        assert_eq!(c.basis, ChoiceBasis::Pinned);
    }

    /// The card's shared route mixes; the raw node locks the card, so the shared one goes first.
    #[test]
    fn a_plugged_in_cards_shared_route_is_tried_before_its_raw_node() {
        let devices = vec![
            dev("default", true, DeviceKind::SoundServer),
            usb("plughw:CARD=Device,DEV=0").with_kind(DeviceKind::Raw),
            usb("sysdefault:CARD=Device"),
        ];
        let ids: Vec<&str> = probe_order(&devices, None, true)
            .into_iter()
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "sysdefault:CARD=Device",
                "plughw:CARD=Device,DEV=0",
                "default"
            ]
        );
    }

    #[test]
    fn a_status_written_before_plugged_in_existed_still_reads() {
        let old = r#"{"id":"Built-in","label":"Built-in","is_default":true,"kind":"hardware"}"#;
        let d: AudioDevice = serde_json::from_str(old).unwrap();
        assert!(!d.plugged_in);
    }

    #[test]
    fn live_streams_go_through_the_sound_server_once_its_default_is_the_chosen_card() {
        let choice = choose(
            OUT,
            vec![
                ProbedDevice::new(usb("sysdefault:CARD=Device"), ProbeOutcome::Accepting),
                ProbedDevice::new(
                    dev("default", true, DeviceKind::SoundServer),
                    ProbeOutcome::Untested,
                ),
            ],
            None,
            true,
        );
        assert_eq!(choice.device_id(), Some("sysdefault:CARD=Device"));
        let server = SoundServerDefaults {
            server: "pulseaudio".into(),
            speaker: Some("USB PnP Audio Device Analog Stereo".into()),
            mic: None,
            problems: Vec::new(),
        };
        assert_eq!(stream_route(&choice, Some(&server), &[]), Some("default"));
        assert_eq!(
            stream_route(&choice, None, &[]),
            Some("sysdefault:CARD=Device")
        );
        assert_eq!(
            stream_route(&choice, Some(&server), &["default".into()]),
            Some("sysdefault:CARD=Device"),
            "a sound server route that stalled mid-session falls back to the card itself"
        );
    }

    #[test]
    fn an_array_or_webcam_mic_beats_a_generic_dongle_when_unpinned() {
        let mut dongle = usb("sysdefault:CARD=Device");
        dongle.label = "USB PnP Audio Device".into();
        let mut webcam = usb("sysdefault:CARD=U0x46d0x892");
        webcam.label = "HD Pro Webcam C920".into();

        let devices = vec![
            ProbedDevice::new(dongle, hearing()),
            ProbedDevice::new(webcam, hearing()),
        ];
        let c = choose(IN, devices, None, true);
        assert_eq!(c.device_id(), Some("sysdefault:CARD=U0x46d0x892"));
        assert_eq!(c.basis, ChoiceBasis::PluggedIn);
    }

    #[test]
    fn a_pinned_generic_dongle_still_beats_an_unpinned_array() {
        let mut dongle = usb("sysdefault:CARD=Device");
        dongle.label = "USB PnP Audio Device".into();
        let mut webcam = usb("sysdefault:CARD=U0x46d0x892");
        webcam.label = "HD Pro Webcam C920".into();

        let devices = vec![
            ProbedDevice::new(dongle, hearing()),
            ProbedDevice::new(webcam, hearing()),
        ];
        let c = choose(IN, devices, Some("sysdefault:CARD=Device"), true);
        assert_eq!(c.device_id(), Some("sysdefault:CARD=Device"));
        assert_eq!(c.basis, ChoiceBasis::Pinned);
    }

    #[test]
    fn auto_pin_string_means_automatic() {
        let mut dongle = usb("sysdefault:CARD=Device");
        dongle.label = "USB PnP Audio Device".into();
        let mut webcam = usb("sysdefault:CARD=U0x46d0x892");
        webcam.label = "HD Pro Webcam C920".into();

        let devices = vec![
            ProbedDevice::new(dongle, hearing()),
            ProbedDevice::new(webcam, hearing()),
        ];
        let c = choose(IN, devices, Some("auto"), true);
        assert_eq!(c.device_id(), Some("sysdefault:CARD=U0x46d0x892"));
        assert_eq!(c.basis, ChoiceBasis::PluggedIn);
        assert_eq!(c.pinned, None);
    }
}
