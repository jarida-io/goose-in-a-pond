//! Device Control MCP server: describe, read and set devices via [`DeviceControlPort`]. The
//! desktop Hub drives devices through these same tools (`POST /api/v1/tools/invoke`).

use pond_core::user_data::ports::device_control::{
    DeviceControlPort, DeviceDescription, DeviceState, ValueSpec,
};
use pond_core::user_data::ports::device_registry::{Device, DeviceRegistry};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorData, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

// ── Parameter struct ─────────────────────────────────────────────────────────
// All optional with a flatten absorber, so `{}` or stray fields from a small model still parse.

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DescribeDeviceParams {
    /// Device id, name, or a natural reference like "the fan".
    pub device_id: String,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SetDeviceStateParams {
    /// Device id or name.
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub power: Option<bool>,
    /// 0-100 percent.
    #[serde(default)]
    pub brightness: Option<u8>,
    /// Degrees Celsius.
    #[serde(default)]
    pub target_temp: Option<f32>,
    #[serde(default)]
    pub locked: Option<bool>,
    /// Hue degrees 0-360.
    #[serde(default)]
    pub hue: Option<u16>,
    /// 0-100 percent.
    #[serde(default)]
    pub saturation: Option<u8>,
    /// Speaker volume 0-100. A television's level is its VOLUME, not its brightness.
    #[serde(default)]
    pub volume: Option<u8>,
    /// Colour temperature in kelvin — roughly 2000 (warm/amber) to 6500 (cool/daylight).
    /// A device's own achievable range comes from describe_device; this is not the same
    /// control as hue + saturation, and a white cannot be asked for as a hue.
    #[serde(default)]
    pub color_temp: Option<u32>,
    /// 0-100 percent.
    #[serde(default)]
    pub fan_speed: Option<u8>,
    /// Fan mode: off, low, medium, high, on, auto, smart. Prefer over fan_speed
    /// when a mode is named — auto and smart have no percentage.
    #[serde(default)]
    pub fan_mode: Option<String>,
    /// A named appliance setting (wash cycle, spin speed). Name and value come
    /// from describe_device verbatim — never translate or abbreviate them.
    #[serde(default)]
    pub setting: Option<String>,
    /// The value for `setting`. Ignored unless `setting` is given.
    #[serde(default)]
    pub setting_value: Option<String>,
    /// start, stop, pause or resume, for a device that runs cycles.
    #[serde(default)]
    pub operation: Option<String>,
    /// 0-100 percent open (100=fully open).
    #[serde(default)]
    pub position: Option<u8>,
    /// Slat angle, 0-100 percent open. Distinct from position: a blind can be
    /// fully down with slats open. Slatted blinds only.
    #[serde(default)]
    pub tilt: Option<u8>,
    /// Open (true) or shut (false) a valve. Not `power`: a valve has no on/off
    /// switch. Use `position` for how far open, where the valve has a level.
    #[serde(default)]
    pub valve: Option<bool>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DeviceControlMcpServer {
    control: Arc<dyn DeviceControlPort + Send + Sync>,
    /// Resolves natural references ("the light", a friendly name) to backend device ids.
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

/// Device-reference resolution, shared by every tool that takes a `device_id`.
impl DeviceControlMcpServer {
    /// The registered id for a caller's reference, or the guidance to send instead.
    async fn resolve_or_explain(
        &self,
        reference: &str,
        tool: &str,
    ) -> Result<String, CallToolResult> {
        match self.registry.list_devices().await {
            Ok(devices) => match resolve_device(reference, &devices) {
                DeviceResolution::Resolved(id) => Ok(id),
                DeviceResolution::Ambiguous(names) => Err(guidance(format!(
                    "'{reference}' matches several devices: {}. Which one?",
                    names.join(", ")
                ))),
                DeviceResolution::NotFound => {
                    let known: Vec<String> = devices.iter().map(|d| d.name.clone()).collect();
                    Err(guidance(if known.is_empty() {
                        "No devices are registered yet.".to_string()
                    } else {
                        format!(
                            "No device matches '{reference}'. Registered devices: {}.",
                            known.join(", ")
                        )
                    }))
                }
            },
            // Registry down: pass the reference through; a backend may still understand it.
            Err(e) => {
                tracing::warn!(error = %e, tool, "device list unavailable");
                Ok(reference.to_string())
            }
        }
    }
}

/// One `render_state` line; pond-desktop's `powerStateOf` (`Devices.tsx`) parses it.
fn state_line(name: &str, value: &str) -> String {
    format!("\n    {name}: {value}")
}

/// A device's current state, as the model reads it.
fn render_state(state: &DeviceState) -> String {
    if state.values.is_empty() {
        // Distinct from "off": the device reported nothing at all.
        return format!("{} reports nothing about its state.", state.device_id);
    }

    let mut out = format!("{} is:", state.device_id);
    for value in &state.values {
        out.push_str(&state_line(&value.name, &value.value));
    }
    out
}

/// The state an operation aims for: verbs and states are different words ("start" → "running").
fn state_intended_by(operation: &str) -> Option<&'static str> {
    match operation.to_ascii_lowercase().as_str() {
        "start" | "resume" => Some("running"),
        "stop" => Some("stopped"),
        "pause" => Some("paused"),
        _ => None,
    }
}

/// Whether Operational State defines this state; only such a state can contradict a verb.
fn is_standard_state(state: &str) -> bool {
    matches!(
        state.to_ascii_lowercase().as_str(),
        "stopped" | "running" | "paused" | "error"
    )
}

/// What an operation did, kept apart from what it was asked to do.
enum OperationNote {
    /// The device reached the state the verb asks for.
    Reached(String),
    Missed(String),
}

/// Report the state an operation reached, not the request: a device may accept Start and
/// stay Stopped (Google's Matter Virtual Device washer does). A miss is its own sentence.
fn render_operation(requested: &str, became: Option<&str>) -> OperationNote {
    let Some(became) = became else {
        // Nothing said about where it ended up: the request is all that is known.
        return OperationNote::Reached(format!("operation={requested}"));
    };

    let contradicted = match state_intended_by(requested) {
        Some(wanted) => is_standard_state(became) && !became.eq_ignore_ascii_case(wanted),
        None => false,
    };

    if contradicted {
        OperationNote::Missed(format!(
            "It was asked to {requested}, but reports being {became}."
        ))
    } else {
        OperationNote::Reached(format!("operation={became}"))
    }
}

/// A device's description as prose, not JSON, so the next tool call is obvious from it.
fn render_description(d: &DeviceDescription) -> String {
    let mut out = format!("{} ({})", d.device_id, d.device_type);

    if d.capabilities.is_empty() {
        out.push_str("\n  Cannot be controlled — nothing to set on this device.");
    } else {
        out.push_str("\n  Accepts:");
        for capability in &d.capabilities {
            // The setting name is what tells an appliance's several `mode` capabilities apart.
            match &capability.setting {
                Some(setting) => out.push_str(&format!(
                    "\n    {} (setting: \"{}\") — {}",
                    capability.verb,
                    setting,
                    render_value(&capability.value)
                )),
                None => out.push_str(&format!(
                    "\n    {} — {}",
                    capability.verb,
                    render_value(&capability.value)
                )),
            }
        }
    }

    if d.sensors.is_empty() {
        out.push_str("\n  Measures: nothing.");
    } else {
        out.push_str("\n  Measures:");
        for sensor in &d.sensors {
            out.push_str(&format!("\n    {} ({})", sensor.sensor_type, sensor.unit));
        }
    }

    // Its own heading, neither Accepts nor Measures, so the agent never tries to set these.
    if !d.states.is_empty() {
        out.push_str("\n  Reports, and cannot be told to change:");
        for state in &d.states {
            out.push_str(&format!(
                "\n    {} — {}",
                state.name,
                render_value(&state.value)
            ));
        }
    }

    // Only when present (rarely), or every description pays for an empty line. It must say
    // both that the control exists and that nothing here can drive it.
    if !d.vendor_clusters.is_empty() {
        out.push_str(
            "\n  Has manufacturer-specific controls that cannot be named or driven from \
             here — the maker's own app is the only thing that can set them:",
        );
        for vendor in &d.vendor_clusters {
            out.push_str(&format!(
                "\n    cluster 0x{:08x} on endpoint {}",
                vendor.cluster_id, vendor.endpoint
            ));
        }
    }

    out
}

fn render_value(value: &ValueSpec) -> String {
    match value {
        ValueSpec::Boolean => "true or false".to_string(),
        ValueSpec::Percent => "0-100 percent".to_string(),
        ValueSpec::Color => "hue 0-360 with saturation 0-100".to_string(),
        ValueSpec::Enum { values } => format!("one of: {}", values.join(", ")),
        ValueSpec::Number {
            min,
            max,
            step,
            unit,
            when,
        } => {
            let unit = unit.as_deref().unwrap_or("");
            let range = match (min, max) {
                (Some(lo), Some(hi)) => format!("a number from {lo} to {hi} {unit}"),
                (Some(lo), None) => format!("a number from {lo} {unit}"),
                (None, Some(hi)) => format!("a number up to {hi} {unit}"),
                // No stated limits: say so rather than implying a range.
                (None, None) => format!("a number in {unit}"),
            };
            let range = range.trim_end().to_string();
            // The step is part of what's accepted: 50.5 into a whole-degree dishwasher is refused.
            let range = match step {
                Some(step) => format!("{range} in steps of {step}"),
                None => range,
            };
            // The condition travels with the range, so a quote of it stays accurate.
            match when {
                Some(when) => format!("{range} ({when})"),
                None => range,
            }
        }
    }
}

#[tool_router]
impl DeviceControlMcpServer {
    /// All tools, without constructing the server; the generated `tool_router()` is private.
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new(
        control: Arc<dyn DeviceControlPort + Send + Sync>,
        registry: Arc<dyn DeviceRegistry + Send + Sync>,
    ) -> Self {
        Self {
            control,
            registry,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "What a device can do and measures: accepted verbs, the values each \
                       takes, its sensors. Consult before driving an unfamiliar device -- \
                       never probe by trying verbs. device_id: id, name, or a ref like \
                       \"the fan\"."
    )]
    async fn describe_device(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<DescribeDeviceParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("describe_device");
        let Parameters(p) = params;
        let device_id = p.device_id.trim();

        let device_id = match self.resolve_or_explain(device_id, "describe_device").await {
            Ok(id) => id,
            Err(explanation) => return Ok(explanation),
        };

        match self.control.describe(&device_id).await {
            Ok(description) => Ok(CallToolResult::success(vec![Content::text(
                render_description(&description),
            )])),
            Err(e) => Ok(guidance(format!("Couldn't describe '{device_id}': {e}"))),
        }
    }

    #[tool(
        description = "What a device currently is: on/off, each setting's value, running or \
                       not. Answer state questions with this, never by driving the device. \
                       Names match describe_device. device_id: id, name, or a ref like \
                       \"the washer\"."
    )]
    async fn get_device_state(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<DescribeDeviceParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("get_device_state");
        let Parameters(p) = params;

        let device_id = match self
            .resolve_or_explain(p.device_id.trim(), "get_device_state")
            .await
        {
            Ok(id) => id,
            Err(explanation) => return Ok(explanation),
        };

        match self.control.state(&device_id).await {
            Ok(state) => Ok(CallToolResult::success(vec![Content::text(render_state(
                &state,
            ))])),
            Err(e) => Ok(guidance(format!("Couldn't read '{device_id}': {e}"))),
        }
    }

    #[tool(
        description = "Set smart-device state: power, brightness, target_temp, lock, colour, fan, position, tilt, valve. device_id: id, name, or natural ref like \"the light\". Unlocking a door or disarming an alarm needs the user's explicit go-ahead in the same message. A device you cannot find is not set up yet — say so rather than guessing."
    )]
    async fn set_device_state(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SetDeviceStateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let device_id = p.device_id.trim();

        if device_id.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "Which device? Provide `device_id` (the device name or id) plus what to change: \
                 power (on/off), brightness (0-100), target_temp (°C), or locked (true/false).",
            )]));
        }
        if p.power.is_none()
            && p.brightness.is_none()
            && p.target_temp.is_none()
            && p.locked.is_none()
            && p.hue.is_none()
            && p.saturation.is_none()
            && p.color_temp.is_none()
            && p.volume.is_none()
            && p.fan_speed.is_none()
            && p.fan_mode.is_none()
            && p.setting.is_none()
            && p.operation.is_none()
            && p.position.is_none()
            && p.tilt.is_none()
            && p.valve.is_none()
        {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No change requested for '{device_id}'. Specify one of: power (on/off), \
                 brightness (0-100), target_temp (°C), locked (true/false), hue (0-360) + \
                 saturation (0-100), color_temp (kelvin, e.g. 2700 for warm white), \
                 volume (0-100, a speaker's level), \
                 fan_speed (0-100), fan_mode (off/low/medium/high/on/auto/\
                 smart), setting + setting_value (appliance settings such as a wash \
                 cycle or spin speed — see describe_device), operation (start/stop/\
                 pause/resume), position (0-100 percent open), tilt (0-100 percent \
                 open, for slats), or valve (true/false, to open or shut one)."
            ))]));
        }

        let device_id = match self.registry.list_devices().await {
            Ok(devices) => match resolve_device(device_id, &devices) {
                DeviceResolution::Resolved(id) => id,
                DeviceResolution::Ambiguous(names) => {
                    return Ok(guidance(format!(
                        "'{device_id}' matches several devices: {}. Which one?",
                        names.join(", ")
                    )));
                }
                DeviceResolution::NotFound => {
                    let known: Vec<String> = devices.iter().map(|d| d.name.clone()).collect();
                    return Ok(guidance(if known.is_empty() {
                        "No devices are registered yet.".to_string()
                    } else {
                        format!(
                            "No device matches '{device_id}'. Registered devices: {}.",
                            known.join(", ")
                        )
                    }));
                }
            },
            Err(e) => {
                // Registry down: pass the reference through; a backend may still understand it.
                tracing::warn!(error = %e, "device resolution unavailable; using raw id");
                device_id.to_string()
            }
        };
        let device_id = device_id.as_str();

        let mut applied: Vec<String> = Vec::new();
        // Anything the device declined to do, said in its own sentence.
        let mut notes: Vec<String> = Vec::new();

        if let Some(on) = p.power {
            match self.control.set_power(device_id, on).await {
                Ok(_) => applied.push(format!("power={}", if on { "on" } else { "off" })),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set power on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(b) = p.brightness {
            let pct = b.min(100);
            match self.control.set_brightness(device_id, pct).await {
                Ok(_) => applied.push(format!("brightness={pct}%")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set brightness on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(t) = p.target_temp {
            match self.control.set_target_temp(device_id, t).await {
                Ok(_) => applied.push(format!("target_temp={t}°C")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set temperature on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(locked) = p.locked {
            match self.control.set_locked(device_id, locked).await {
                Ok(_) => applied.push(format!("locked={locked}")),
                Err(e) => return Ok(guidance(format!("Couldn't (un)lock '{device_id}': {e}"))),
            }
        }
        if let Some(open) = p.valve {
            match self.control.set_valve(device_id, open).await {
                Ok(_) => applied.push(format!("valve={}", if open { "open" } else { "closed" })),
                Err(e) => {
                    let asked = if open { "open" } else { "close" };
                    return Ok(guidance(format!("Couldn't {asked} '{device_id}': {e}")));
                }
            }
        }
        // Hue and saturation are one action, so a missing half takes a default.
        if p.hue.is_some() || p.saturation.is_some() {
            let hue = p.hue.unwrap_or(0) % 360;
            let sat = p.saturation.unwrap_or(100).min(100);
            match self.control.set_color(device_id, hue, sat).await {
                Ok(_) => applied.push(format!("colour=hue {hue}°/sat {sat}%")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set colour on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(level) = p.volume {
            let pct = level.min(100);
            match self.control.set_volume(device_id, pct).await {
                Ok(_) => applied.push(format!("volume={pct}%")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set volume on '{device_id}': {e}"
                    )))
                }
            }
        }
        // Its own action, never substituted for hue: hue on a tunable-white bulb is rejected.
        if let Some(kelvin) = p.color_temp {
            match self.control.set_color_temp(device_id, kelvin).await {
                Ok(_) => applied.push(format!("color_temp={kelvin}K")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set colour temperature on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(speed) = p.fan_speed {
            let pct = speed.min(100);
            match self.control.set_fan_speed(device_id, pct).await {
                Ok(_) => applied.push(format!("fan_speed={pct}%")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set fan speed on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(mode) = p.fan_mode.as_deref() {
            match self.control.set_fan_mode(device_id, mode).await {
                Ok(_) => applied.push(format!("fan_mode={mode}")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set fan mode on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(setting) = p.setting.as_deref() {
            // A setting without a value is a question: ask rather than guess the cycle.
            let Some(value) = p.setting_value.as_deref() else {
                return Ok(guidance(format!(
                    "Which value for '{setting}' on '{device_id}'? Pass setting_value. \
                     describe_device lists what it accepts."
                )));
            };
            match self.control.set_mode(device_id, setting, value).await {
                // Reported in the device's own label, which may differ from what was typed.
                Ok(outcome) => match outcome.applied.mode {
                    Some(mode) => applied.push(format!("{}={}", mode.setting, mode.value)),
                    None => applied.push(format!("{setting}={value}")),
                },
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set {setting} on '{device_id}': {e}"
                    )))
                }
            }
        }
        if let Some(operation) = p.operation.as_deref() {
            match self.control.set_operation(device_id, operation).await {
                Ok(outcome) => {
                    match render_operation(operation, outcome.applied.operation.as_deref()) {
                        OperationNote::Reached(text) => applied.push(text),
                        OperationNote::Missed(text) => notes.push(text),
                    }
                }
                Err(e) => return Ok(guidance(format!("Couldn't {operation} '{device_id}': {e}"))),
            }
        }
        if let Some(open) = p.tilt {
            let pct = open.min(100);
            match self.control.set_tilt(device_id, pct).await {
                Ok(_) => applied.push(format!("tilt={pct}% open")),
                Err(e) => return Ok(guidance(format!("Couldn't tilt '{device_id}': {e}"))),
            }
        }

        if let Some(open) = p.position {
            let pct = open.min(100);
            match self.control.set_position(device_id, pct).await {
                Ok(_) => applied.push(format!("position={pct}% open")),
                Err(e) => {
                    return Ok(guidance(format!(
                        "Couldn't set position on '{device_id}': {e}"
                    )))
                }
            }
        }

        if applied.is_empty() && notes.is_empty() {
            // Nothing was asked for; don't report it as a change.
            return Ok(guidance(format!(
                "Nothing to set on '{device_id}' — no state was given. \
                 describe_device lists what it accepts."
            )));
        }

        // Separate sentences: what took effect, then anything the device did not do.
        let mut message = if applied.is_empty() {
            String::new()
        } else {
            format!("Set {device_id}: {}.", applied.join(", "))
        };
        for note in notes {
            if !message.is_empty() {
                message.push(' ');
            }
            message.push_str(&note);
        }

        Ok(CallToolResult::success(vec![Content::text(message)]))
    }
}

/// Error-as-guidance: the LLM reads this and adapts (per the MCP server standard).
fn guidance(msg: String) -> CallToolResult {
    CallToolResult::success(vec![Content::text(msg)])
}

// ── Natural device resolution (pure, unit-tested) ────────────────────────────

/// Outcome of resolving a user/LLM device reference against the registry.
#[derive(Debug, PartialEq)]
pub(crate) enum DeviceResolution {
    /// The registered id to hand to the control backend.
    Resolved(String),
    /// Multiple devices plausibly match — names for a disambiguation prompt.
    Ambiguous(Vec<String>),
    NotFound,
}

/// Lowercase, drop a leading "the "/"my " and a plural 's' ("the lights" → "light").
fn normalize_reference(input: &str) -> String {
    let lower = input.trim().to_lowercase();
    let stripped = lower
        .strip_prefix("the ")
        .or_else(|| lower.strip_prefix("my "))
        .unwrap_or(&lower);
    stripped.strip_suffix('s').unwrap_or(stripped).to_string()
}

/// Resolve `input` to a device id by the first tier with hits: exact id, exact name
/// (case-insensitive), name containing it, then device type. Several hits are ambiguous.
pub(crate) fn resolve_device(input: &str, devices: &[Device]) -> DeviceResolution {
    let raw = input.trim();
    if let Some(device) = devices.iter().find(|d| d.id == raw) {
        return DeviceResolution::Resolved(device.id.clone());
    }

    let lower = raw.to_lowercase();
    let tiers: [Vec<&Device>; 3] = [
        devices
            .iter()
            .filter(|d| d.name.to_lowercase() == lower)
            .collect(),
        devices
            .iter()
            .filter(|d| d.name.to_lowercase().contains(&lower))
            .collect(),
        devices
            .iter()
            .filter(|d| d.device_type.to_lowercase() == normalize_reference(raw))
            .collect(),
    ];
    for tier in tiers {
        match tier.as_slice() {
            [] => continue,
            [device] => return DeviceResolution::Resolved(device.id.clone()),
            many => {
                return DeviceResolution::Ambiguous(many.iter().map(|d| d.name.clone()).collect())
            }
        }
    }
    DeviceResolution::NotFound
}

#[tool_handler]
impl ServerHandler for DeviceControlMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-device-control",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Device Control MCP server — actuate smart devices.\n\n\
                 Tool: set_device_state (power on/off, brightness 0-100, target_temp °C, \
                 lock/unlock). device_id accepts the registered id, the device name, or a \
                 natural reference like \"the light\"; plus the field(s) to change.",
            )
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct DeviceControlDeps {
    control: Arc<dyn DeviceControlPort + Send + Sync>,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
}

static DEVICE_CONTROL_DEPS: OnceLock<DeviceControlDeps> = OnceLock::new();

/// Initialize device-control server dependencies. Call once at startup.
pub fn init_device_control_deps(
    control: Arc<dyn DeviceControlPort + Send + Sync>,
    registry: Arc<dyn DeviceRegistry + Send + Sync>,
) {
    let _ = DEVICE_CONTROL_DEPS.set(DeviceControlDeps { control, registry });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_device_control_server(reader: DuplexStream, writer: DuplexStream) {
    // No deps = this binary didn't install this family: skip, as a panic kills every builtin.
    let Some(deps) = DEVICE_CONTROL_DEPS.get() else {
        tracing::error!(
            "spawn_device_control_server called before init_device_control_deps — extension will not start"
        );
        return;
    };
    let server = DeviceControlMcpServer::new(deps.control.clone(), deps.registry.clone());
    crate::serve_builtin("giap-device-control", server, reader, writer);
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pond_core::user_data::ports::device_control::{
        DeviceControlOutcome, DeviceStatePatch, StateValue,
    };
    use OperationNote::{Missed, Reached};

    use pond_core::user_data::ports::device_control::{
        Capability, SensorSpec, StateSpec, VendorCluster,
    };

    #[test]
    fn a_state_line_is_the_shape_the_desktop_parses() {
        // Parsed by `powerStateOf` in pond-desktop's `Devices.tsx`: change both together.
        assert_eq!(state_line("power", "on"), "\n    power: on");

        let rendered = render_state(&DeviceState {
            device_id: "matter-2".to_string(),
            values: vec![
                StateValue {
                    name: "power".to_string(),
                    value: "on".to_string(),
                },
                StateValue {
                    name: "brightness".to_string(),
                    value: "50%".to_string(),
                },
            ],
        });
        assert_eq!(rendered, "matter-2 is:\n    power: on\n    brightness: 50%");
    }

    fn spec(verb: &str, value: ValueSpec) -> Capability {
        Capability {
            verb: verb.to_string(),
            setting: None,
            value,
        }
    }

    /// One of several capabilities sharing a verb, told apart by its setting name.
    fn setting_spec(verb: &str, setting: &str, value: ValueSpec) -> Capability {
        Capability {
            verb: verb.to_string(),
            setting: Some(setting.to_string()),
            value,
        }
    }

    #[test]
    fn capabilities_sharing_a_verb_are_told_apart_by_their_setting() {
        let rendered = render_description(&DeviceDescription {
            device_id: "matter-1".into(),
            device_type: "appliance".into(),
            capabilities: vec![
                spec("power", ValueSpec::Boolean),
                setting_spec(
                    "mode",
                    "laundry washer mode",
                    ValueSpec::Enum {
                        values: vec!["Normal".into(), "Heavy".into()],
                    },
                ),
                setting_spec(
                    "mode",
                    "spin speed",
                    ValueSpec::Enum {
                        values: vec!["Off".into(), "High".into()],
                    },
                ),
            ],
            sensors: vec![],
            vendor_clusters: vec![],
            states: vec![],
        });

        // The model passes this name to `set_device_state`, so it must be in the text.
        assert!(
            rendered.contains(r#"mode (setting: "laundry washer mode") — one of: Normal, Heavy"#),
            "{rendered}"
        );
        assert!(
            rendered.contains(r#"mode (setting: "spin speed") — one of: Off, High"#),
            "{rendered}"
        );
        // A verb a device can only have one of stays unadorned.
        assert!(rendered.contains("power — true or false"), "{rendered}");
    }

    #[test]
    fn a_description_names_the_values_a_device_accepts() {
        let rendered = render_description(&DeviceDescription {
            device_id: "matter-18".into(),
            device_type: "fan".into(),
            capabilities: vec![
                spec("power", ValueSpec::Boolean),
                spec("fan_speed", ValueSpec::Percent),
                spec(
                    "fan_mode",
                    ValueSpec::Enum {
                        values: vec!["off".into(), "low".into(), "high".into()],
                    },
                ),
            ],
            sensors: vec![],
            vendor_clusters: vec![],
            states: vec![],
        });

        assert!(rendered.contains("matter-18 (fan)"), "{rendered}");
        // Without the modes, "fan_mode" leaves the model guessing which words are accepted.
        assert!(rendered.contains("one of: off, low, high"), "{rendered}");
        assert!(rendered.contains("0-100 percent"), "{rendered}");
        assert!(rendered.contains("Measures: nothing."), "{rendered}");
    }

    #[test]
    fn a_stated_range_is_shown_and_an_unstated_one_is_not_invented() {
        let stated = render_description(&DeviceDescription {
            device_id: "matter-30".into(),
            device_type: "thermostat".into(),
            capabilities: vec![spec(
                "target_temp",
                ValueSpec::Number {
                    min: Some(7.0),
                    max: Some(30.0),
                    step: None,
                    unit: Some("C".into()),
                    when: None,
                },
            )],
            sensors: vec![],
            vendor_clusters: vec![],
            states: vec![],
        });
        assert!(stated.contains("from 7 to 30 C"), "{stated}");

        let silent = render_description(&DeviceDescription {
            device_id: "matter-31".into(),
            device_type: "thermostat".into(),
            capabilities: vec![spec(
                "target_temp",
                ValueSpec::Number {
                    min: None,
                    max: None,
                    step: None,
                    unit: Some("C".into()),
                    when: None,
                },
            )],
            sensors: vec![],
            vendor_clusters: vec![],
            states: vec![],
        });
        assert!(silent.contains("a number in C"), "{silent}");
        assert!(!silent.contains("from"), "no range is implied: {silent}");
    }

    #[test]
    fn a_sensor_lists_what_it_measures_with_units() {
        let rendered = render_description(&DeviceDescription {
            device_id: "matter-40".into(),
            device_type: "sensor".into(),
            capabilities: vec![],
            sensors: vec![
                SensorSpec {
                    sensor_type: "carbon_dioxide".into(),
                    unit: "ppm".into(),
                },
                SensorSpec {
                    sensor_type: "pm2_5".into(),
                    unit: "ug/m3".into(),
                },
            ],
            vendor_clusters: vec![],
            states: vec![],
        });

        assert!(rendered.contains("carbon_dioxide (ppm)"), "{rendered}");
        assert!(rendered.contains("pm2_5 (ug/m3)"), "{rendered}");
        assert!(rendered.contains("Cannot be controlled"), "{rendered}");
    }

    #[test]
    fn a_conditional_range_carries_its_condition() {
        let rendered = render_description(&DeviceDescription {
            device_id: "matter-1".into(),
            device_type: "thermostat".into(),
            capabilities: vec![Capability {
                verb: "target_temp".into(),
                setting: None,
                value: ValueSpec::Number {
                    min: Some(7.0),
                    max: Some(23.5),
                    step: None,
                    unit: Some("C".into()),
                    when: Some(
                        "while heating; this device reaches 7 to 32 C across its modes".into(),
                    ),
                },
            }],
            sensors: vec![],
            vendor_clusters: vec![],
            states: vec![],
        });

        assert!(rendered.contains("a number from 7 to 23.5 C"), "{rendered}");
        // Both halves: what it is true of, and what the device can still reach.
        assert!(rendered.contains("(while heating"), "{rendered}");
        assert!(rendered.contains("reaches 7 to 32 C"), "{rendered}");
    }

    /// Anything whose limits do not move says nothing extra.
    #[test]
    fn an_unconditional_range_reads_as_it_did() {
        let rendered = render_value(&ValueSpec::Number {
            min: Some(0.0),
            max: Some(100.0),
            step: None,
            unit: Some("%".into()),
            when: None,
        });
        assert_eq!(rendered, "a number from 0 to 100 %");
    }

    #[test]
    fn a_state_reads_as_names_that_can_be_set() {
        let rendered = render_state(&DeviceState {
            device_id: "matter-1".into(),
            values: vec![
                StateValue {
                    name: "power".into(),
                    value: "on".into(),
                },
                StateValue {
                    name: "spin speed".into(),
                    value: "High".into(),
                },
                StateValue {
                    name: "operation".into(),
                    value: "running".into(),
                },
            ],
        });

        assert!(rendered.contains("power: on"), "{rendered}");
        // Named as describe_device names it, so no second lookup is needed to change it.
        assert!(rendered.contains("spin speed: High"), "{rendered}");
        assert!(rendered.contains("operation: running"), "{rendered}");
    }

    #[test]
    fn a_silent_device_says_so_rather_than_rendering_an_empty_list() {
        let rendered = render_state(&DeviceState {
            device_id: "matter-9".into(),
            values: vec![],
        });
        assert!(
            rendered.contains("reports nothing about its state"),
            "{rendered}"
        );
    }

    #[test]
    fn an_operation_reports_the_state_reached_not_the_one_requested() {
        // The Matter Virtual Device washer accepts Start and stays Stopped.
        let Missed(text) = render_operation("start", Some("stopped")) else {
            panic!("a washer that stayed stopped is not a reached state");
        };
        assert_eq!(text, "It was asked to start, but reports being stopped.");

        let Missed(text) = render_operation("pause", Some("running")) else {
            panic!("still running is not a completed pause");
        };
        assert_eq!(text, "It was asked to pause, but reports being running.");

        // A verb and its resulting state are different words for one success.
        for (verb, state, expected) in [
            ("start", "running", "operation=running"),
            ("stop", "stopped", "operation=stopped"),
            ("resume", "running", "operation=running"),
            // A device's own word for its state is accepted, not read as a miss.
            ("start", "washing", "operation=washing"),
        ] {
            let Reached(text) = render_operation(verb, Some(state)) else {
                panic!("{verb} -> {state} should read as reached");
            };
            assert_eq!(text, expected);
        }

        // Nothing said about where it ended up: the request is all that is known.
        let Reached(text) = render_operation("stop", None) else {
            panic!("an unstated result is not a miss");
        };
        assert_eq!(text, "operation=stop");
    }

    struct StubControl;
    #[async_trait]
    impl DeviceControlPort for StubControl {
        async fn set_power(&self, id: &str, on: bool) -> anyhow::Result<DeviceControlOutcome> {
            Ok(DeviceControlOutcome::new(
                id,
                DeviceStatePatch {
                    on: Some(on),
                    ..Default::default()
                },
            ))
        }
        async fn set_brightness(
            &self,
            id: &str,
            percent: u8,
        ) -> anyhow::Result<DeviceControlOutcome> {
            Ok(DeviceControlOutcome::new(
                id,
                DeviceStatePatch {
                    brightness: Some(percent),
                    ..Default::default()
                },
            ))
        }
        async fn set_target_temp(
            &self,
            id: &str,
            celsius: f32,
        ) -> anyhow::Result<DeviceControlOutcome> {
            Ok(DeviceControlOutcome::new(
                id,
                DeviceStatePatch {
                    target_temp: Some(celsius),
                    ..Default::default()
                },
            ))
        }
        async fn set_locked(&self, id: &str, locked: bool) -> anyhow::Result<DeviceControlOutcome> {
            Ok(DeviceControlOutcome::new(
                id,
                DeviceStatePatch {
                    locked: Some(locked),
                    ..Default::default()
                },
            ))
        }
    }

    /// Local stub: pond-core's mocks are feature-gated.
    struct EmptyRegistry;
    #[async_trait]
    impl DeviceRegistry for EmptyRegistry {
        async fn register(
            &self,
            _: pond_core::user_data::ports::device_registry::RegisterDeviceRequest,
        ) -> anyhow::Result<Device> {
            anyhow::bail!("not used")
        }
        async fn list_devices(&self) -> anyhow::Result<Vec<Device>> {
            Ok(vec![])
        }
        async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> {
            Ok(None)
        }
        async fn unregister(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn heartbeat(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_server() -> DeviceControlMcpServer {
        DeviceControlMcpServer::new(Arc::new(StubControl), Arc::new(EmptyRegistry))
    }

    #[test]
    fn server_constructs() {
        let _server = test_server();
    }

    // ── resolve_device: natural references → registered ids ─────────────────

    fn device(id: &str, name: &str, device_type: &str) -> Device {
        Device {
            id: id.to_string(),
            name: name.to_string(),
            device_type: device_type.to_string(),
            hostname: None,
            ip_address: None,
            capabilities: vec![],
            registered_at: "2026-01-01T00:00:00Z".to_string(),
            last_seen: None,
            is_online: true,
            room: None,
        }
    }

    #[test]
    fn the_light_resolves_by_type_when_unique() {
        let devices = [
            device("matter-2", "Living Room Light", "light"),
            device("matter-9", "Front Door", "lock"),
        ];
        for spoken in ["the light", "light", "The Lights", "my light"] {
            assert_eq!(
                resolve_device(spoken, &devices),
                DeviceResolution::Resolved("matter-2".into()),
                "failed for {spoken:?}"
            );
        }
    }

    #[test]
    fn ids_names_and_partial_names_resolve_in_tier_order() {
        let devices = [
            device("matter-2", "Living Room Light", "light"),
            device("matter-3", "Bedroom Light", "light"),
        ];
        // Exact id always wins.
        assert_eq!(
            resolve_device("matter-3", &devices),
            DeviceResolution::Resolved("matter-3".into())
        );
        // Case-insensitive exact name.
        assert_eq!(
            resolve_device("living room light", &devices),
            DeviceResolution::Resolved("matter-2".into())
        );
        // Unique partial name.
        assert_eq!(
            resolve_device("bedroom", &devices),
            DeviceResolution::Resolved("matter-3".into())
        );
    }

    #[test]
    fn ambiguous_and_unknown_references_ask_instead_of_guessing() {
        let devices = [
            device("matter-2", "Living Room Light", "light"),
            device("matter-3", "Bedroom Light", "light"),
        ];
        // Two lights: "the light" must NOT pick one silently.
        match resolve_device("the light", &devices) {
            DeviceResolution::Ambiguous(names) => assert_eq!(names.len(), 2),
            other => panic!("expected ambiguity, got {other:?}"),
        }
        assert_eq!(
            resolve_device("the thermostat", &devices),
            DeviceResolution::NotFound
        );
    }

    #[test]
    fn a_control_that_cannot_be_driven_is_still_disclosed() {
        let rendered = render_description(&DeviceDescription {
            device_id: "matter-31".into(),
            device_type: "light".into(),
            capabilities: vec![
                spec("power", ValueSpec::Boolean),
                spec("brightness", ValueSpec::Percent),
            ],
            sensors: vec![],
            vendor_clusters: vec![VendorCluster {
                cluster_id: 0xfff1_fc01,
                endpoint: 1,
            }],
            states: vec![],
        });

        // Both halves: naming the control without the caveat promises what can't be reached.
        assert!(
            rendered.contains("cluster 0xfff1fc01 on endpoint 1"),
            "{rendered}"
        );
        assert!(rendered.contains("cannot be named or driven"), "{rendered}");
        assert!(rendered.contains("the maker's own app"), "{rendered}");
        // Disclosure is not a capability. The verbs are what `set_device_state` accepts.
        assert!(rendered.contains("power — true or false"), "{rendered}");
        assert!(!rendered.contains("0xfff1fc01 — "), "{rendered}");
    }

    #[test]
    fn a_device_with_no_vendor_cluster_says_nothing_about_them() {
        let rendered = render_description(&DeviceDescription {
            device_id: "matter-2".into(),
            device_type: "light".into(),
            capabilities: vec![spec("power", ValueSpec::Boolean)],
            sensors: vec![],
            vendor_clusters: vec![],
            states: vec![],
        });

        assert!(!rendered.contains("manufacturer-specific"), "{rendered}");
        assert!(!rendered.contains("cluster"), "{rendered}");
    }

    #[test]
    fn a_reading_that_cannot_be_set_is_named_as_one() {
        let rendered = render_description(&DeviceDescription {
            device_id: "matter-44".into(),
            device_type: "lock".into(),
            capabilities: vec![spec("locked", ValueSpec::Boolean)],
            sensors: vec![],
            vendor_clusters: vec![],
            states: vec![
                StateSpec {
                    name: "door".into(),
                    value: ValueSpec::Enum {
                        values: vec!["open".into(), "closed".into(), "jammed".into()],
                    },
                },
                StateSpec {
                    name: "pin_required".into(),
                    value: ValueSpec::Enum {
                        values: vec!["required".into(), "not required".into()],
                    },
                },
            ],
        });

        assert!(
            rendered.contains("door — one of: open, closed, jammed"),
            "{rendered}"
        );
        assert!(
            rendered.contains("pin_required — one of: required, not required"),
            "{rendered}"
        );
        assert!(rendered.contains("cannot be told to change"), "{rendered}");
        // And it is not in Accepts, which is what `set_device_state` reads.
        let accepts = rendered.split("Reports").next().unwrap_or_default();
        assert!(!accepts.contains("pin_required"), "{rendered}");
    }

    #[test]
    fn a_device_that_reports_nothing_read_only_says_nothing_about_it() {
        let rendered = render_description(&DeviceDescription {
            device_id: "matter-2".into(),
            device_type: "light".into(),
            capabilities: vec![spec("power", ValueSpec::Boolean)],
            sensors: vec![],
            vendor_clusters: vec![],
            states: vec![],
        });

        assert!(!rendered.contains("Reports"), "{rendered}");
    }

    #[test]
    fn a_colour_temperature_range_reads_in_kelvin() {
        let rendered = render_description(&DeviceDescription {
            device_id: "matter-51".into(),
            device_type: "light".into(),
            capabilities: vec![
                spec("power", ValueSpec::Boolean),
                spec("color", ValueSpec::Color),
                spec(
                    "color_temp",
                    ValueSpec::Number {
                        min: Some(2000.0),
                        max: Some(6536.0),
                        step: None,
                        unit: Some("K".into()),
                        when: None,
                    },
                ),
            ],
            sensors: vec![],
            vendor_clusters: vec![],
            states: vec![],
        });

        assert!(
            rendered.contains("color_temp — a number from 2000 to 6536 K"),
            "{rendered}"
        );
        // Named separately: a white cannot be asked for as a hue.
        assert!(
            rendered.contains("color — hue 0-360 with saturation 0-100"),
            "{rendered}"
        );
    }

    #[test]
    fn colour_temperature_is_a_thing_the_tool_accepts() {
        let p: SetDeviceStateParams =
            serde_json::from_str(r#"{"device_id":"lamp","color_temp":2700}"#).unwrap();
        assert_eq!(p.color_temp, Some(2700));
        assert!(p.hue.is_none() && p.saturation.is_none());
    }

    #[test]
    fn params_default_from_empty_object() {
        let p: SetDeviceStateParams = serde_json::from_str("{}").unwrap();
        assert!(p.device_id.is_empty());
        assert!(p.power.is_none());
    }

    #[test]
    fn params_absorb_unexpected_fields() {
        let p: SetDeviceStateParams =
            serde_json::from_str(r#"{"device_id":"lamp","power":true,"surprise":1}"#).unwrap();
        assert_eq!(p.device_id, "lamp");
        assert_eq!(p.power, Some(true));
        assert!(p.extra.contains_key("surprise"));
    }
}
