//! The board's sound server (PulseAudio, or PipeWire's PulseAudio service), told through
//! `pactl`: which speaker and microphone are its defaults, and their levels. Once a card becomes
//! the server's default, the pond plays and records through the server (`default`) rather than
//! holding the ALSA card itself, so other programs on the board can use it at the same time.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use pond_core::models::domain::audio_device::{
    AudioDevice, AudioDirection, DeviceKind, LevelChange, SoundServerDefaults,
};

use crate::names::{self, Card};

/// The server's 100%, in its raw volume units.
const FULL: u32 = 65_536;
/// A sound server that does not answer in this long is treated as absent, not waited on.
const PACTL_LIMIT: Duration = Duration::from_secs(3);
/// How long a headset's nodes may take to appear after its profile changes.
const PROFILE_SETTLES: Duration = Duration::from_secs(2);

pub trait Pactl: Send + Sync {
    fn run(&self, args: &[&str]) -> Result<String, String>;
}

/// The real `pactl`, in the C locale so its output parses on a board set to any language.
pub struct SystemPactl;

impl Pactl for SystemPactl {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut child = Command::new("pactl")
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("pactl: {e}"))?;
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            if started.elapsed() >= PACTL_LIMIT {
                let _ = child.kill();
                let _ = child.wait();
                return Err("the sound server did not answer".to_string());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut out = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            let _ = stdout.read_to_string(&mut out);
        }
        if status.success() {
            Ok(out)
        } else {
            let mut err = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut err);
            }
            Err(format!("pactl {}: {}", args.join(" "), err.trim()))
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub server: String,
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
}

pub fn parse_info(text: &str) -> Info {
    let mut info = Info::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "Server Name" => info.server = value,
            "Default Sink" => info.default_sink = Some(value),
            "Default Source" => info.default_source = Some(value),
            _ => {}
        }
    }
    info
}

/// One sink or source, from `pactl list sinks` or `pactl list sources`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub description: String,
    pub card: Option<u32>,
    pub monitor: bool,
    pub muted: bool,
    /// `RUNNING` or `IDLE`: the sound server has the ALSA card open right now.
    pub active: bool,
    /// The first channel's raw volume.
    pub volume: Option<u32>,
    /// The device's natural level: 100% for a speaker, the card's 0 dB for a microphone.
    pub base_volume: Option<u32>,
    /// Its volume is the card's own mixer, so it reaches a program that opens the card
    /// directly, as the pond does during a sound test.
    pub hardware_volume: bool,
}

fn first_raw_volume(value: &str) -> Option<u32> {
    let after_channel = value.split_once(':').map_or(value, |(_, v)| v);
    after_channel.split_whitespace().next()?.parse().ok()
}

fn quoted(value: &str) -> &str {
    value.trim().trim_matches('"')
}

pub fn parse_nodes(text: &str) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut current: Option<Node> = None;
    for line in text.lines() {
        if line.starts_with("Sink #") || line.starts_with("Source #") {
            nodes.extend(current.take());
            current = Some(Node::default());
            continue;
        }
        let Some(node) = current.as_mut() else {
            continue;
        };
        let trimmed = line.trim();
        if let Some((key, value)) = trimmed.split_once(" = ") {
            match key {
                "alsa.card" => node.card = quoted(value).parse().ok(),
                "device.class" => node.monitor = quoted(value) == "monitor",
                _ => {}
            }
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key {
            "State" => node.active = matches!(value, "RUNNING" | "IDLE"),
            "Name" => node.name = value.to_string(),
            "Description" => node.description = value.to_string(),
            "Mute" => node.muted = value == "yes",
            "Volume" => node.volume = first_raw_volume(value),
            "Base Volume" => {
                node.base_volume = value.split_whitespace().next().and_then(|v| v.parse().ok())
            }
            "Flags" => {
                node.hardware_volume = value.split_whitespace().any(|f| f == "HW_VOLUME_CTRL")
            }
            _ => {}
        }
    }
    nodes.extend(current);
    nodes
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Speaker,
    Mic,
}

impl Side {
    fn from_direction(direction: AudioDirection) -> Self {
        match direction {
            AudioDirection::Input => Self::Mic,
            AudioDirection::Output => Self::Speaker,
        }
    }
    fn list(self) -> &'static str {
        match self {
            Self::Speaker => "sinks",
            Self::Mic => "sources",
        }
    }
    fn set_default(self) -> &'static str {
        match self {
            Self::Speaker => "set-default-sink",
            Self::Mic => "set-default-source",
        }
    }
    fn set_mute(self) -> &'static str {
        match self {
            Self::Speaker => "set-sink-mute",
            Self::Mic => "set-source-mute",
        }
    }
    fn set_volume(self) -> &'static str {
        match self {
            Self::Speaker => "set-sink-volume",
            Self::Mic => "set-source-volume",
        }
    }
    fn suspend(self) -> &'static str {
        match self {
            Self::Speaker => "suspend-sink",
            Self::Mic => "suspend-source",
        }
    }
    fn noun(self) -> &'static str {
        match self {
            Self::Speaker => "speaker",
            Self::Mic => "microphone",
        }
    }
    fn default_of(self, info: &Info) -> Option<&str> {
        match self {
            Self::Speaker => info.default_sink.as_deref(),
            Self::Mic => info.default_source.as_deref(),
        }
    }
}

/// The pond's route through the sound server: it already follows the server's default.
fn through_the_server(device: &AudioDevice) -> bool {
    through_the_server_id(&device.id)
}

fn through_the_server_id(id: &str) -> bool {
    matches!(id, "default" | "pulse" | "pipewire")
}

/// Releases the sound server's hold on ALSA cards for the life of a direct ALSA probe or
/// listing, then lets the server resume them on drop.
pub struct SuspendedNodes<'a> {
    pactl: &'a dyn Pactl,
    resumed: Vec<(&'static str, String)>,
}

impl Drop for SuspendedNodes<'_> {
    fn drop(&mut self) {
        for (cmd, name) in self.resumed.drain(..).rev() {
            let _ = self.pactl.run(&[cmd, &name, "0"]);
        }
    }
}

/// Suspend every active node in `direction` so `cpal`'s ALSA listing can open each card.
pub fn suspend_active(pactl: &dyn Pactl, direction: AudioDirection) -> SuspendedNodes<'_> {
    let side = Side::from_direction(direction);
    let mut resumed = Vec::new();
    if let Ok(text) = pactl.run(&["list", side.list()]) {
        for node in parse_nodes(&text)
            .into_iter()
            .filter(|n| !n.monitor && n.active)
        {
            if pactl.run(&[side.suspend(), &node.name, "1"]).is_ok() {
                resumed.push((side.suspend(), node.name));
            }
        }
    }
    SuspendedNodes { pactl, resumed }
}

/// Suspend active nodes on the ALSA cards named by `input` and `output` while a direct ALSA
/// probe or test sound opens them; leaves the sound server alone for `default` / `pulse`.
pub fn suspend_for_devices<'a>(
    pactl: &'a dyn Pactl,
    cards: &[Card],
    input: Option<&str>,
    output: Option<&str>,
) -> SuspendedNodes<'a> {
    let mut resumed = Vec::new();
    for (id, side) in [(output, Side::Speaker), (input, Side::Mic)] {
        let Some(id) = id.filter(|id| !through_the_server_id(id)) else {
            continue;
        };
        let Some(card) = names::card_index(id, cards) else {
            continue;
        };
        let Ok(text) = pactl.run(&["list", side.list()]) else {
            continue;
        };
        for node in parse_nodes(&text)
            .into_iter()
            .filter(|n| !n.monitor && n.card == Some(card) && n.active)
        {
            if pactl.run(&[side.suspend(), &node.name, "1"]).is_ok() {
                resumed.push((side.suspend(), node.name));
            }
        }
    }
    SuspendedNodes { pactl, resumed }
}

/// The server's node for a device the pond chose: its current default for the server route,
/// the node on the same ALSA card otherwise. `Ok(None)` for a device that is not a card.
fn node_for(
    pactl: &dyn Pactl,
    cards: &[Card],
    info: &Info,
    device: &AudioDevice,
    side: Side,
) -> Result<Option<Node>, String> {
    let nodes: Vec<Node> = parse_nodes(&pactl.run(&["list", side.list()])?)
        .into_iter()
        .filter(|n| !n.monitor)
        .collect();
    if through_the_server(device) {
        let default = side.default_of(info);
        return Ok(nodes.into_iter().find(|n| Some(n.name.as_str()) == default));
    }
    if device.kind == DeviceKind::Bluetooth {
        return nodes
            .into_iter()
            .find(|n| on_card(n, &device.id))
            .map(Some)
            .ok_or_else(|| format!("'{}' offers no {} now", device.label, side.noun()));
    }
    let Some(card) = names::card_index(&device.id, cards) else {
        return Ok(None);
    };
    nodes
        .into_iter()
        .find(|n| n.card == Some(card))
        .map(Some)
        .ok_or_else(|| {
            format!(
                "{} lists no {} on '{}'",
                info.server,
                side.noun(),
                device.label
            )
        })
}

fn make_default(
    pactl: &dyn Pactl,
    cards: &[Card],
    info: &Info,
    device: &AudioDevice,
    side: Side,
) -> Result<Option<String>, String> {
    let Some(node) = node_for(pactl, cards, info, device, side)? else {
        return Ok(None);
    };
    if side.default_of(info) != Some(node.name.as_str()) {
        pactl.run(&[side.set_default(), &node.name])?;
        tracing::info!(node = %node.name, "made the pond's {} the sound server's default", side.noun());
    }
    if node.muted {
        pactl.run(&[side.set_mute(), &node.name, "0"])?;
        tracing::info!(node = %node.name, "unmuted the {}", side.noun());
    }
    Ok(Some(node.description))
}

/// Make the chosen devices the sound server's defaults and unmute them. `None` when no sound
/// server answers: then there are no defaults but the cards themselves.
pub fn set_defaults(
    pactl: &dyn Pactl,
    cards: &[Card],
    input: Option<&AudioDevice>,
    output: Option<&AudioDevice>,
) -> Option<SoundServerDefaults> {
    let info = parse_info(&pactl.run(&["info"]).ok()?);
    let mut result = SoundServerDefaults {
        server: info.server.clone(),
        ..Default::default()
    };
    for card in [output, input].into_iter().flatten() {
        if card.kind != DeviceKind::Bluetooth {
            continue;
        }
        let speaker = output.is_some_and(|d| d.id == card.id);
        let mic = input.is_some_and(|d| d.id == card.id);
        if let Err(problem) = ready_card(pactl, &card.id, speaker, mic, false) {
            tracing::warn!(%problem, "could not ready the Bluetooth device");
            result.problems.push(problem);
        }
    }
    for (device, side) in [(output, Side::Speaker), (input, Side::Mic)] {
        let Some(device) = device else { continue };
        let made = if device.kind == DeviceKind::Bluetooth {
            settled(|| make_default(pactl, cards, &info, device, side))
        } else {
            make_default(pactl, cards, &info, device, side)
        };
        match made {
            Ok(described) => match side {
                Side::Speaker => result.speaker = described,
                Side::Mic => result.mic = described,
            },
            Err(problem) => {
                tracing::warn!(%problem, "could not make the pond's {} the board's default", side.noun());
                result.problems.push(problem);
            }
        }
    }
    Some(result)
}

fn percent(raw: u32) -> String {
    format!("{}%", (raw as u64 * 100 + FULL as u64 / 2) / FULL as u64)
}

/// Unmute the pair and raise a volume below its natural level up to it, never beyond: louder
/// than unity distorts. Only levels the pond's own sound passes through are touched.
pub fn raise_levels(
    pactl: &dyn Pactl,
    cards: &[Card],
    speaker: &AudioDevice,
    mic: &AudioDevice,
) -> Vec<LevelChange> {
    let Ok(info) = pactl.run(&["info"]).map(|t| parse_info(&t)) else {
        return Vec::new();
    };
    let mut changes = Vec::new();
    for (device, side) in [(speaker, Side::Speaker), (mic, Side::Mic)] {
        let Ok(Some(node)) = node_for(pactl, cards, &info, device, side) else {
            continue;
        };
        if !(node.hardware_volume || through_the_server(device)) {
            continue;
        }
        if node.muted && pactl.run(&[side.set_mute(), &node.name, "0"]).is_ok() {
            changes.push(LevelChange {
                device: node.description.clone(),
                what: "mute".into(),
                from: "on".into(),
                to: "off".into(),
                undo: vec![side.set_mute().into(), node.name.clone(), "1".into()],
            });
        }
        let target = node.base_volume.unwrap_or(FULL).min(FULL);
        if let Some(volume) = node.volume.filter(|v| *v < target) {
            if pactl
                .run(&[side.set_volume(), &node.name, &target.to_string()])
                .is_ok()
            {
                changes.push(LevelChange {
                    device: node.description.clone(),
                    what: "volume".into(),
                    from: percent(volume),
                    to: percent(target),
                    undo: vec![
                        side.set_volume().into(),
                        node.name.clone(),
                        volume.to_string(),
                    ],
                });
            }
        }
    }
    if !changes.is_empty() {
        let raised: Vec<String> = changes.iter().map(ToString::to_string).collect();
        tracing::info!(?raised, "raised levels for another sound test");
    }
    changes
}

pub fn restore_levels(pactl: &dyn Pactl, changes: &[LevelChange]) {
    for change in changes.iter().rev() {
        let args: Vec<&str> = change.undo.iter().map(String::as_str).collect();
        if let Err(e) = pactl.run(&args) {
            tracing::warn!(error = %e, "could not put a raised level back");
        }
    }
}

/// A profile of a Bluetooth card: `a2dp_sink`, `handsfree_head_unit` and the like.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub sinks: u32,
    pub sources: u32,
    pub priority: u32,
    pub available: bool,
}

/// A connected Bluetooth headset or speaker, from `pactl list cards`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BluetoothCard {
    pub name: String,
    pub description: String,
    pub profiles: Vec<Profile>,
    pub active: String,
}

impl BluetoothCard {
    /// The best available profile that offers what is asked. `keep_active` holds on to the
    /// current one while it does, so a brief test does not switch a headset that is in use.
    fn profile_for(&self, speaker: bool, mic: bool, keep_active: bool) -> Option<&Profile> {
        let fits =
            |p: &&Profile| p.available && (!speaker || p.sinks > 0) && (!mic || p.sources > 0);
        let active = self.profiles.iter().find(|p| p.name == self.active);
        if keep_active {
            if let Some(active) = active.filter(fits) {
                return Some(active);
            }
        }
        self.profiles.iter().filter(fits).max_by_key(|p| p.priority)
    }
}

/// `a2dp_sink: High Fidelity Playback (A2DP Sink) (sinks: 1, sources: 0, priority: 40, available: yes)`
fn parse_profile(line: &str) -> Option<Profile> {
    let (name, rest) = line.split_once(": ")?;
    let (_, counts) = rest.rsplit_once('(')?;
    let mut profile = Profile {
        name: name.to_string(),
        available: true,
        ..Default::default()
    };
    for field in counts.trim_end_matches(')').split(", ") {
        let (key, value) = field.split_once(": ")?;
        match key {
            "sinks" => profile.sinks = value.parse().ok()?,
            "sources" => profile.sources = value.parse().ok()?,
            "priority" => profile.priority = value.parse().ok()?,
            "available" => profile.available = value != "no",
            _ => {}
        }
    }
    Some(profile)
}

pub fn parse_bluetooth_cards(text: &str) -> Vec<BluetoothCard> {
    let mut cards = Vec::new();
    let mut current: Option<(BluetoothCard, bool)> = None;
    let mut in_profiles = false;
    for line in text.lines() {
        if line.starts_with("Card #") {
            cards.extend(current.take().filter(|(_, bt)| *bt).map(|(c, _)| c));
            current = Some((BluetoothCard::default(), false));
            in_profiles = false;
            continue;
        }
        let Some((card, bluetooth)) = current.as_mut() else {
            continue;
        };
        let trimmed = line.trim();
        if let Some((key, value)) = trimmed.split_once(" = ") {
            match key {
                "device.description" => card.description = quoted(value).to_string(),
                "device.bus" => *bluetooth = quoted(value) == "bluetooth",
                _ => {}
            }
            continue;
        }
        // The section's own lines sit one tab deeper than its heading.
        if in_profiles && line.starts_with("\t\t") {
            card.profiles.extend(parse_profile(trimmed));
            continue;
        }
        in_profiles = trimmed == "Profiles:";
        if let Some(name) = trimmed.strip_prefix("Name: ") {
            card.name = name.to_string();
        } else if let Some(active) = trimmed.strip_prefix("Active Profile: ") {
            card.active = active.to_string();
        }
    }
    cards.extend(current.filter(|(_, bt)| *bt).map(|(c, _)| c));
    cards
}

/// Whether a sink or source belongs to a Bluetooth card: PulseAudio and PipeWire both name
/// them after the headset's address, `bluez_sink.B0_F0_0C_5F_3E_F6.a2dp_sink` on
/// `bluez_card.B0_F0_0C_5F_3E_F6`.
fn on_card(node: &Node, card: &str) -> bool {
    let address = card.strip_prefix("bluez_card.");
    node.name.starts_with("bluez_") && node.name.split('.').nth(1) == address
}

fn bluetooth_cards(pactl: &dyn Pactl) -> Vec<BluetoothCard> {
    pactl
        .run(&["list", "cards"])
        .map(|t| parse_bluetooth_cards(&t))
        .unwrap_or_default()
}

/// The connected Bluetooth devices that can play (`Output`) or listen (`Input`) in some profile:
/// a headset's microphone exists only in its hands-free profile, which choosing it switches to.
pub fn bluetooth_devices(pactl: &dyn Pactl, direction: AudioDirection) -> Vec<AudioDevice> {
    let speaker = direction == AudioDirection::Output;
    bluetooth_cards(pactl)
        .into_iter()
        .filter(|c| c.profile_for(speaker, !speaker, false).is_some())
        .map(|c| AudioDevice {
            label: if c.description.is_empty() {
                c.name.clone()
            } else {
                c.description
            },
            id: c.name,
            is_default: false,
            kind: DeviceKind::Bluetooth,
            plugged_in: true,
        })
        .collect()
}

/// The connected Bluetooth cards, which change when a headset connects or leaves; a profile
/// switch leaves them as they are.
pub fn bluetooth_fingerprint(pactl: &dyn Pactl) -> String {
    let listed = pactl.run(&["list", "short", "cards"]).unwrap_or_default();
    listed
        .lines()
        .filter(|l| l.contains("bluez_card."))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Put a Bluetooth card in a profile that offers what is asked; the profile it left, if it
/// had to change one.
fn ready_card(
    pactl: &dyn Pactl,
    card: &str,
    speaker: bool,
    mic: bool,
    keep_active: bool,
) -> Result<Option<String>, String> {
    let cards = bluetooth_cards(pactl);
    let found = cards
        .iter()
        .find(|c| c.name == card)
        .ok_or_else(|| format!("the Bluetooth device '{card}' is not connected"))?;
    let profile = found
        .profile_for(speaker, mic, keep_active)
        .ok_or_else(|| format!("'{}' has no profile for that", found.description))?;
    if profile.name == found.active {
        return Ok(None);
    }
    pactl.run(&["set-card-profile", card, &profile.name])?;
    tracing::info!(card, profile = %profile.name, "switched the Bluetooth device's profile");
    Ok(Some(found.active.clone()))
}

/// `find` until it succeeds or a headset's new profile has had time to bring up its nodes.
fn settled<T>(mut find: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let started = Instant::now();
    loop {
        match find() {
            Err(_) if started.elapsed() < PROFILE_SETTLES => {
                std::thread::sleep(Duration::from_millis(100))
            }
            done => return done,
        }
    }
}

/// A Bluetooth device made the sound server's default for a brief test, so the pond can open
/// it as `default`. Drop puts back the default and the profile it found.
pub struct BluetoothRoute<'a> {
    pactl: &'a dyn Pactl,
    side: Side,
    card: String,
    previous_default: Option<String>,
    previous_profile: Option<String>,
}

impl Drop for BluetoothRoute<'_> {
    fn drop(&mut self) {
        if let Some(name) = &self.previous_default {
            let _ = self.pactl.run(&[self.side.set_default(), name]);
        }
        if let Some(profile) = &self.previous_profile {
            let _ = self.pactl.run(&["set-card-profile", &self.card, profile]);
        }
    }
}

pub fn route_bluetooth<'a>(
    pactl: &'a dyn Pactl,
    device: &AudioDevice,
    direction: AudioDirection,
) -> Result<BluetoothRoute<'a>, String> {
    let side = Side::from_direction(direction);
    let info = parse_info(&pactl.run(&["info"])?);
    let mut route = BluetoothRoute {
        pactl,
        side,
        card: device.id.clone(),
        previous_default: None,
        previous_profile: ready_card(
            pactl,
            &device.id,
            side == Side::Speaker,
            side == Side::Mic,
            true,
        )?,
    };
    let node = settled(|| node_for(pactl, &[], &info, device, side))?
        .ok_or_else(|| format!("'{}' offers no {}", device.label, side.noun()))?;
    let previous = side.default_of(&info).map(str::to_string);
    if previous.as_deref() != Some(node.name.as_str()) {
        pactl.run(&[side.set_default(), &node.name])?;
        route.previous_default = previous;
    }
    Ok(route)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::audio_device::DeviceKind;
    use std::sync::Mutex;

    const INFO: &str = "Server String: /run/user/1000/pulse/native
Server Name: pulseaudio
Server Version: 15.99.1
Default Sink: alsa_output.platform-sound.analog-stereo
Default Source: alsa_input.platform-sound.analog-stereo
";

    /// The household nano's own `pactl list sinks`, trimmed to the lines that matter.
    const SINKS: &str = "Sink #0
\tState: SUSPENDED
\tName: alsa_output.usb-Solid_State_System_Co._Ltd._USB_PnP_Audio_Device_000000000000-00.analog-stereo
\tDescription: USB PnP Audio Device Analog Stereo
\tMute: no
\tVolume: front-left: 33595 /  51% / -17.41 dB,   front-right: 33595 /  51% / -17.41 dB
\t        balance 0.00
\tBase Volume: 65536 / 100% / 0.00 dB
\tFlags: HARDWARE HW_MUTE_CTRL HW_VOLUME_CTRL DECIBEL_VOLUME LATENCY
\tProperties:
\t\tdevice.class = \"sound\"
\t\talsa.card = \"0\"
\t\talsa.card_name = \"USB PnP Audio Device\"
Sink #1
\tState: SUSPENDED
\tName: alsa_output.platform-sound.analog-stereo
\tDescription: Built-in Audio Analog Stereo
\tMute: no
\tVolume: front-left: 62914 /  96% / -1.06 dB,   front-right: 62914 /  96% / -1.06 dB
\tBase Volume: 65536 / 100% / 0.00 dB
\tFlags: HARDWARE DECIBEL_VOLUME LATENCY
\tProperties:
\t\tdevice.class = \"sound\"
\t\talsa.card = \"2\"
";

    const SOURCES: &str = "Source #0
\tName: alsa_output.usb-Solid_State_System_Co._Ltd._USB_PnP_Audio_Device_000000000000-00.analog-stereo.monitor
\tDescription: Monitor of USB PnP Audio Device Analog Stereo
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
\tBase Volume: 65536 / 100% / 0.00 dB
\tFlags: DECIBEL_VOLUME LATENCY
\tProperties:
\t\tdevice.class = \"monitor\"
\t\talsa.card = \"0\"
Source #1
\tName: alsa_input.usb-Solid_State_System_Co._Ltd._USB_PnP_Audio_Device_000000000000-00.analog-stereo
\tDescription: USB PnP Audio Device Analog Stereo
\tMute: yes
\tVolume: front-left: 6553 /  10% / -60.00 dB,   front-right: 6553 /  10% / -60.00 dB
\tBase Volume: 19944 /  30% / -31.00 dB
\tFlags: HARDWARE HW_MUTE_CTRL HW_VOLUME_CTRL DECIBEL_VOLUME LATENCY
\tProperties:
\t\tdevice.class = \"sound\"
\t\talsa.card = \"0\"
Source #3
\tName: alsa_input.platform-sound.analog-stereo
\tDescription: Built-in Audio Analog Stereo
\tMute: no
\tVolume: front-left: 65536 / 100% / 0.00 dB,   front-right: 65536 / 100% / 0.00 dB
\tBase Volume: 65536 / 100% / 0.00 dB
\tFlags: HARDWARE DECIBEL_VOLUME LATENCY
\tProperties:
\t\tdevice.class = \"sound\"
\t\talsa.card = \"2\"
";

    const USB_SINK: &str =
        "alsa_output.usb-Solid_State_System_Co._Ltd._USB_PnP_Audio_Device_000000000000-00.analog-stereo";
    const USB_SOURCE: &str =
        "alsa_input.usb-Solid_State_System_Co._Ltd._USB_PnP_Audio_Device_000000000000-00.analog-stereo";

    #[derive(Default)]
    struct FakePactl {
        absent: bool,
        calls: Mutex<Vec<String>>,
    }

    impl FakePactl {
        fn changes(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.starts_with("set-"))
                .cloned()
                .collect()
        }
    }

    impl Pactl for FakePactl {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            if self.absent {
                return Err("Connection failure: Connection refused".into());
            }
            self.calls.lock().unwrap().push(args.join(" "));
            Ok(match args {
                ["info"] => INFO,
                ["list", "sinks"] => SINKS,
                ["list", "sources"] => SOURCES,
                _ => "",
            }
            .to_string())
        }
    }

    fn cards() -> Vec<Card> {
        names::parse_cards(
            " 0 [Device         ]: USB-Audio - USB PnP Audio Device\n 2 [APE            ]: tegra-ape - NVIDIA Jetson Orin Nano APE\n",
        )
    }

    fn usb() -> AudioDevice {
        AudioDevice::new("sysdefault:CARD=Device", false, DeviceKind::Hardware).plugged_in()
    }

    #[test]
    fn the_nanos_sinks_and_sources_parse() {
        let sinks = parse_nodes(SINKS);
        assert_eq!(sinks.len(), 2);
        assert_eq!(sinks[0].name, USB_SINK);
        assert_eq!(sinks[0].card, Some(0));
        assert_eq!(sinks[0].volume, Some(33_595));
        assert_eq!(sinks[0].base_volume, Some(FULL));
        assert!(sinks[0].hardware_volume && !sinks[0].muted && !sinks[0].monitor);
        assert!(!sinks[1].hardware_volume);
        let sources = parse_nodes(SOURCES);
        assert!(sources[0].monitor, "a sink's monitor is not a microphone");
        assert_eq!(sources[1].base_volume, Some(19_944));
        assert!(sources[1].muted);
        let info = parse_info(INFO);
        assert_eq!(info.server, "pulseaudio");
        assert_eq!(
            info.default_sink.as_deref(),
            Some("alsa_output.platform-sound.analog-stereo")
        );
    }

    /// The nano's PulseAudio defaulted to the onboard path, which nothing is plugged into.
    #[test]
    fn the_usb_pair_becomes_the_boards_default_and_is_unmuted() {
        let pactl = FakePactl::default();
        let usb = usb();
        let done = set_defaults(&pactl, &cards(), Some(&usb), Some(&usb)).expect("server");
        assert_eq!(done.server, "pulseaudio");
        assert_eq!(
            done.speaker.as_deref(),
            Some("USB PnP Audio Device Analog Stereo")
        );
        assert_eq!(
            done.mic.as_deref(),
            Some("USB PnP Audio Device Analog Stereo")
        );
        assert!(done.problems.is_empty());
        assert_eq!(
            pactl.changes(),
            [
                format!("set-default-sink {USB_SINK}"),
                format!("set-default-source {USB_SOURCE}"),
                format!("set-source-mute {USB_SOURCE} 0"),
            ]
        );
    }

    #[test]
    fn a_device_that_already_follows_the_server_changes_nothing() {
        let pactl = FakePactl::default();
        let default = AudioDevice::new("default", true, DeviceKind::SoundServer);
        let done = set_defaults(&pactl, &cards(), None, Some(&default)).expect("server");
        assert_eq!(
            done.speaker.as_deref(),
            Some("Built-in Audio Analog Stereo")
        );
        assert!(pactl.changes().is_empty());
    }

    #[test]
    fn a_card_the_server_does_not_list_is_reported_not_guessed() {
        let pactl = FakePactl::default();
        let hdmi = AudioDevice::new("hdmi:CARD=HDA,DEV=0", false, DeviceKind::Hardware);
        let cards =
            names::parse_cards(" 1 [HDA            ]: tegra-hda - NVIDIA Jetson Orin Nano HDA\n");
        let done = set_defaults(&pactl, &cards, None, Some(&hdmi)).expect("server");
        assert_eq!(done.speaker, None);
        assert_eq!(done.problems.len(), 1, "{:?}", done.problems);
        assert!(pactl.changes().is_empty());
    }

    #[test]
    fn without_a_sound_server_there_is_nothing_to_tell() {
        let pactl = FakePactl {
            absent: true,
            ..Default::default()
        };
        assert_eq!(
            set_defaults(&pactl, &cards(), Some(&usb()), Some(&usb())),
            None
        );
        assert!(raise_levels(&pactl, &cards(), &usb(), &usb()).is_empty());
    }

    /// The speaker goes to 100%; the microphone only to its card's 0 dB, not to the +31 dB its
    /// 100% means, which would clip speech.
    #[test]
    fn levels_rise_to_their_natural_level_and_go_back_exactly() {
        let pactl = FakePactl::default();
        let raised = raise_levels(&pactl, &cards(), &usb(), &usb());
        let shown: Vec<String> = raised.iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "USB PnP Audio Device Analog Stereo volume from 51% to 100%",
                "USB PnP Audio Device Analog Stereo mute from on to off",
                "USB PnP Audio Device Analog Stereo volume from 10% to 30%",
            ]
        );
        assert_eq!(
            pactl.changes(),
            [
                format!("set-sink-volume {USB_SINK} 65536"),
                format!("set-source-mute {USB_SOURCE} 0"),
                format!("set-source-volume {USB_SOURCE} 19944"),
            ]
        );
        pactl.calls.lock().unwrap().clear();
        restore_levels(&pactl, &raised);
        assert_eq!(
            pactl.changes(),
            [
                format!("set-source-volume {USB_SOURCE} 6553"),
                format!("set-source-mute {USB_SOURCE} 1"),
                format!("set-sink-volume {USB_SINK} 33595"),
            ]
        );
    }

    /// The onboard sink's volume lives only in the server, so the pond's direct playback would
    /// not get louder for raising it.
    #[test]
    fn a_level_the_ponds_own_sound_bypasses_is_left_alone() {
        let pactl = FakePactl::default();
        let ape = AudioDevice::new("sysdefault:CARD=APE", false, DeviceKind::Hardware);
        let raised = raise_levels(&pactl, &cards(), &ape, &ape);
        assert!(raised.is_empty(), "{raised:?}");
    }

    #[test]
    fn active_nodes_are_suspended_for_a_direct_probe_and_resumed_after() {
        struct RunningUsb {
            calls: Mutex<Vec<String>>,
        }
        impl Pactl for RunningUsb {
            fn run(&self, args: &[&str]) -> Result<String, String> {
                self.calls.lock().unwrap().push(args.join(" "));
                Ok(match args {
                    ["list", "sinks"] => SINKS.replacen("State: SUSPENDED", "State: RUNNING", 1),
                    ["list", "sources"] => {
                        SOURCES.replace("Source #1\n", "Source #1\n\tState: IDLE\n")
                    }
                    _ => String::new(),
                })
            }
        }
        let pactl = RunningUsb {
            calls: Mutex::new(Vec::new()),
        };
        {
            let _held = suspend_for_devices(
                &pactl,
                &cards(),
                Some("sysdefault:CARD=Device"),
                Some("sysdefault:CARD=Device"),
            );
            let calls = pactl.calls.lock().unwrap().clone();
            assert!(calls.contains(&format!("suspend-sink {USB_SINK} 1")));
            assert!(calls.contains(&format!("suspend-source {USB_SOURCE} 1")));
        }
        let calls = pactl.calls.lock().unwrap().clone();
        assert!(calls.contains(&format!("suspend-sink {USB_SINK} 0")));
        assert!(calls.contains(&format!("suspend-source {USB_SOURCE} 0")));
    }

    #[test]
    fn opening_the_sound_server_itself_never_suspends_its_own_nodes() {
        let pactl = FakePactl::default();
        drop(suspend_for_devices(
            &pactl,
            &cards(),
            Some("default"),
            Some("default"),
        ));
        assert!(pactl.calls.lock().unwrap().is_empty());
    }

    /// The household nano's `pactl list cards` with a JLab headset connected, trimmed.
    const CARDS: &str = "Card #1
\tName: alsa_card.platform-sound
\tDriver: module-alsa-card.c
\tProperties:
\t\tdevice.description = \"Built-in Audio\"
\t\tdevice.bus = \"platform\"
\tProfiles:
\t\toutput:analog-stereo+input:analog-stereo: Analog Stereo Duplex (sinks: 1, sources: 1, priority: 6565, available: yes)
\tActive Profile: output:analog-stereo+input:analog-stereo
Card #2
\tName: bluez_card.B0_F0_0C_5F_3E_F6
\tDriver: module-bluez5-device.c
\tProperties:
\t\tdevice.description = \"JLab Epic Lux Lab Edition\"
\t\tdevice.api = \"bluez\"
\t\tdevice.bus = \"bluetooth\"
\tProfiles:
\t\ta2dp_sink: High Fidelity Playback (A2DP Sink) (sinks: 1, sources: 0, priority: 40, available: yes)
\t\thandsfree_head_unit: Handsfree Head Unit (HFP) (sinks: 1, sources: 1, priority: 30, available: yes)
\t\toff: Off (sinks: 0, sources: 0, priority: 0, available: yes)
\tActive Profile: ACTIVE
\tPorts:
\t\theadset-input: Headset (type: Headset, priority: 0, latency offset: 0 usec, availability unknown)
\t\t\tPart of profile(s): handsfree_head_unit
";

    const HEADSET: &str = "bluez_card.B0_F0_0C_5F_3E_F6";
    const ONBOARD_SINK: &str = "alsa_output.platform-sound.analog-stereo";
    const ONBOARD_SOURCE: &str = "alsa_input.platform-sound.analog-stereo";

    /// A sound server with the headset connected, whose nodes follow its profile.
    struct Headset {
        profile: Mutex<String>,
        calls: Mutex<Vec<String>>,
    }

    impl Headset {
        fn in_profile(profile: &str) -> Self {
            Self {
                profile: Mutex::new(profile.to_string()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn changes(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.starts_with("set-"))
                .cloned()
                .collect()
        }
    }

    impl Pactl for Headset {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            self.calls.lock().unwrap().push(args.join(" "));
            let profile = self.profile.lock().unwrap().clone();
            let node = |kind: &str, name: &str| {
                format!("{kind} #0\n\tName: {name}\n\tMute: no\n\tProperties:\n\t\tdevice.class = \"sound\"\n")
            };
            Ok(match args {
                ["info"] => format!(
                    "Server Name: pulseaudio\nDefault Sink: {ONBOARD_SINK}\nDefault Source: {ONBOARD_SOURCE}\n"
                ),
                ["list", "cards"] => CARDS.replace("ACTIVE", &profile),
                ["list", "sinks"] => node("Sink", ONBOARD_SINK)
                    + &node("Sink", &format!("bluez_sink.B0_F0_0C_5F_3E_F6.{profile}")),
                ["list", "sources"] if profile == "handsfree_head_unit" => {
                    node("Source", ONBOARD_SOURCE)
                        + &node("Source", "bluez_source.B0_F0_0C_5F_3E_F6.handsfree_head_unit")
                }
                ["list", "sources"] => node("Source", ONBOARD_SOURCE),
                ["set-card-profile", _, to] => {
                    *self.profile.lock().unwrap() = to.to_string();
                    String::new()
                }
                _ => String::new(),
            })
        }
    }

    fn headset() -> AudioDevice {
        AudioDevice::new(HEADSET, false, DeviceKind::Bluetooth).plugged_in()
    }

    #[test]
    fn only_bluetooth_cards_and_their_profiles_are_read() {
        let cards = parse_bluetooth_cards(&CARDS.replace("ACTIVE", "a2dp_sink"));
        assert_eq!(cards.len(), 1, "{cards:?}");
        let jlab = &cards[0];
        assert_eq!(jlab.name, HEADSET);
        assert_eq!(jlab.description, "JLab Epic Lux Lab Edition");
        assert_eq!(jlab.active, "a2dp_sink");
        let profiles: Vec<(&str, u32, u32, u32)> = jlab
            .profiles
            .iter()
            .map(|p| (p.name.as_str(), p.sinks, p.sources, p.priority))
            .collect();
        assert_eq!(
            profiles,
            [
                ("a2dp_sink", 1, 0, 40),
                ("handsfree_head_unit", 1, 1, 30),
                ("off", 0, 0, 0),
            ]
        );
    }

    #[test]
    fn pipewire_profile_lines_with_a_codec_parse_too() {
        let profile = parse_profile(
            "a2dp-sink-sbc: High Fidelity Playback (A2DP Sink, codec SBC) (sinks: 1, sources: 0, priority: 18, available: yes)",
        );
        assert_eq!(
            profile.map(|p| (p.name, p.sinks, p.priority)),
            Some(("a2dp-sink-sbc".into(), 1, 18))
        );
    }

    #[test]
    fn a_headset_is_offered_as_a_speaker_and_as_a_microphone() {
        let pactl = Headset::in_profile("a2dp_sink");
        for direction in [AudioDirection::Output, AudioDirection::Input] {
            let found = bluetooth_devices(&pactl, direction);
            assert_eq!(found.len(), 1, "{direction:?}");
            assert_eq!(found[0].id, HEADSET);
            assert_eq!(found[0].label, "JLab Epic Lux Lab Edition");
            assert_eq!(found[0].kind, DeviceKind::Bluetooth);
            assert!(found[0].plugged_in);
        }
        assert!(pactl.changes().is_empty(), "listing switches nothing");
    }

    #[test]
    fn a_headset_chosen_to_play_stays_in_high_fidelity_and_becomes_the_default() {
        let pactl = Headset::in_profile("a2dp_sink");
        let done = set_defaults(&pactl, &[], None, Some(&headset())).expect("server");
        assert!(done.problems.is_empty(), "{:?}", done.problems);
        assert!(done.speaker.is_some());
        assert_eq!(
            pactl.changes(),
            ["set-default-sink bluez_sink.B0_F0_0C_5F_3E_F6.a2dp_sink"]
        );
    }

    #[test]
    fn a_headset_chosen_to_listen_switches_to_hands_free_for_its_microphone() {
        let pactl = Headset::in_profile("a2dp_sink");
        let done = set_defaults(&pactl, &[], Some(&headset()), Some(&headset())).expect("server");
        assert!(done.problems.is_empty(), "{:?}", done.problems);
        assert_eq!(
            pactl.changes(),
            [
                format!("set-card-profile {HEADSET} handsfree_head_unit"),
                "set-default-sink bluez_sink.B0_F0_0C_5F_3E_F6.handsfree_head_unit".to_string(),
                "set-default-source bluez_source.B0_F0_0C_5F_3E_F6.handsfree_head_unit".to_string(),
            ]
        );
    }

    #[test]
    fn a_headset_no_longer_listening_goes_back_to_high_fidelity() {
        let pactl = Headset::in_profile("handsfree_head_unit");
        set_defaults(&pactl, &[], None, Some(&headset())).expect("server");
        assert_eq!(
            pactl.changes()[0],
            format!("set-card-profile {HEADSET} a2dp_sink")
        );
    }

    #[test]
    fn a_brief_test_of_a_headsets_microphone_puts_everything_back() {
        let pactl = Headset::in_profile("a2dp_sink");
        {
            let _route =
                route_bluetooth(&pactl, &headset(), AudioDirection::Input).expect("routed");
            assert_eq!(*pactl.profile.lock().unwrap(), "handsfree_head_unit");
        }
        assert_eq!(
            pactl.changes(),
            [
                format!("set-card-profile {HEADSET} handsfree_head_unit"),
                "set-default-source bluez_source.B0_F0_0C_5F_3E_F6.handsfree_head_unit".to_string(),
                format!("set-default-source {ONBOARD_SOURCE}"),
                format!("set-card-profile {HEADSET} a2dp_sink"),
            ]
        );
    }

    #[test]
    fn a_brief_test_of_a_headsets_speaker_leaves_its_hands_free_profile_alone() {
        let pactl = Headset::in_profile("handsfree_head_unit");
        drop(route_bluetooth(&pactl, &headset(), AudioDirection::Output).expect("routed"));
        assert!(pactl
            .changes()
            .iter()
            .all(|c| !c.starts_with("set-card-profile")));
    }
}
