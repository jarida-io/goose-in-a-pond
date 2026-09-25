//! Sensor MCP server: live and stored IoT sensor readings.

use std::sync::{Arc, OnceLock};

use chrono::Utc;
use pond_core::user_data::ports::device_control::DeviceControlPort;
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::sensor_storage::SensorStorage;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;

// ── Parameter structs ──────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetSensorReadingParams {
    /// Room/device name, e.g. "bedroom".
    pub device_id: Option<String>,
    /// e.g. "temperature", "humidity".
    pub sensor_type: Option<String>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetSensorHistoryParams {
    /// Room/device name, e.g. "bedroom".
    pub device_id: Option<String>,
    /// e.g. "temperature".
    pub sensor_type: Option<String>,
    /// ISO 8601 start, inclusive. Omit for all history.
    pub since: Option<String>,
    /// ISO 8601 end, exclusive. Omit for up to now.
    pub until: Option<String>,
    /// "min", "max", or "avg"; omit for raw readings.
    pub agg: Option<String>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListSensorsParams {
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CreateSensorRuleParams {
    pub name: Option<String>,
    /// "sensor" (default) | "camera" | "device".
    pub source: Option<String>,
    /// Match this device/camera ID only; omit for any.
    pub device_id: Option<String>,
    /// Event type, e.g. "motion", "person", "temperature"; omit for any.
    pub signal: Option<String>,
    /// Compare event value: gt | gte | lt | lte | eq.
    pub op: Option<String>,
    /// Threshold for `op`.
    pub value: Option<f64>,
    /// Fire only after this local time, 24h "HH:MM".
    pub after: Option<String>,
    /// Fire only before this local time, 24h "HH:MM".
    pub before: Option<String>,
    /// Action: send this prompt to the agent.
    pub prompt: Option<String>,
    /// Action: switch this device (with power_on).
    pub power_device_id: Option<String>,
    /// true = on (default), false = off.
    pub power_on: Option<bool>,
    /// Action: notification title (requires notify_body).
    pub notify_title: Option<String>,
    pub notify_body: Option<String>,
    /// Debounce seconds between fires. Default 60.
    pub cooldown_secs: Option<u64>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DeleteSensorRuleParams {
    /// Rule ID (see list_sensor_rules).
    pub rule_id: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Matter's word for a graded (enum-valued) reading, e.g. air quality 2 = "Fair".
/// Mirrors the table in `matter-server/src/mapping/sensors.ts`; keep both in sync.
fn worded_reading(sensor_type: &str, value: f64) -> Option<&'static str> {
    // Only exact whole numbers name a grade; 1.5 is not a level.
    if value.fract() != 0.0 {
        return None;
    }
    let code = value as i64;

    match sensor_type {
        "air_quality" => match code {
            0 => Some("Unknown"),
            1 => Some("Good"),
            2 => Some("Fair"),
            3 => Some("Moderate"),
            4 => Some("Poor"),
            5 => Some("Very poor"),
            6 => Some("Extremely poor"),
            _ => None,
        },
        "hepa_filter_change" | "carbon_filter_change" => match code {
            0 => Some("OK"),
            1 => Some("Warning"),
            2 => Some("Critical"),
            _ => None,
        },
        "smoke_alarm" => match code {
            0 => Some("Normal"),
            1 => Some("Warning"),
            2 => Some("Critical"),
            _ => None,
        },
        _ => None,
    }
}

/// A reading as a person reads it: the number, and the word where there is one.
fn render_reading(sensor_type: &str, value: f64, unit: &str) -> String {
    match worded_reading(sensor_type, value) {
        Some(word) => format!("{word} ({value} {unit})"),
        None => format!("{value} {unit}"),
    }
}

#[cfg(test)]
mod reading_words {
    use super::*;

    #[test]
    fn a_grade_is_read_as_a_grade() {
        assert_eq!(
            render_reading("air_quality", 2.0, "level"),
            "Fair (2 level)"
        );
        assert_eq!(
            render_reading("hepa_filter_change", 2.0, "state"),
            "Critical (2 state)"
        );
        assert_eq!(
            render_reading("smoke_alarm", 0.0, "state"),
            "Normal (0 state)"
        );
    }

    #[test]
    fn a_quantity_is_left_alone() {
        assert_eq!(render_reading("temperature", 21.5, "C"), "21.5 C");
        assert_eq!(render_reading("carbon_monoxide", 433.0, "ppm"), "433 ppm");
    }

    #[test]
    fn an_unknown_grade_keeps_its_number() {
        assert_eq!(render_reading("air_quality", 9.0, "level"), "9 level");
        // Not a whole number, so not a grade at all.
        assert_eq!(render_reading("air_quality", 1.5, "level"), "1.5 level");
    }
}

/// How long ago, in the coarsest true unit; a model relays a bare timestamp as current.
fn describe_age(elapsed: chrono::Duration) -> String {
    let seconds = elapsed.num_seconds().max(0);
    match seconds {
        0..=90 => format!("{seconds}s"),
        91..=5399 => format!("{}min", seconds / 60),
        5400..=172_799 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

// ── MCP server ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SensorsMcpServer {
    sensor_storage: Arc<dyn SensorStorage + Send + Sync>,
    /// Knows devices that have not reported yet, and whether each is reachable.
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    /// Asked what a reachable device reads NOW. Stored rows can be hours stale: the Matter
    /// controller publishes only changes and the bridge dedupes again.
    device_control: Arc<dyn DeviceControlPort>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl SensorsMcpServer {
    /// Registered id for a device reference, resolved like every other tool; an unknown
    /// reference passes through unchanged so the caller still gets "no readings yet".
    async fn resolved_device(&self, reference: &str) -> String {
        match self.device_registry.list_devices().await {
            Ok(devices) => match crate::device_control::resolve_device(reference, &devices) {
                crate::device_control::DeviceResolution::Resolved(id) => id,
                _ => reference.to_string(),
            },
            Err(e) => {
                tracing::warn!(error = %e, "sensors: device list unavailable for resolution");
                reference.to_string()
            }
        }
    }
}

#[tool_router]
impl SensorsMcpServer {
    /// Every tool this server exposes, without constructing it (`tool_router()` is private).
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new(
        sensor_storage: Arc<dyn SensorStorage + Send + Sync>,
        device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
        device_control: Arc<dyn DeviceControlPort>,
    ) -> Self {
        Self {
            sensor_storage,
            device_registry,
            device_control,
            tool_router: Self::tool_router(),
        }
    }

    /// Live value, or `None` to use the store. Gated on `is_online`: an unreachable Matter
    /// device costs a fabric timeout. `StateValue.name` uses the `list_sensors` words.
    async fn live_reading(&self, device_id: &str, sensor_type: &str) -> Option<String> {
        match self.device_registry.get_device(device_id).await {
            Ok(Some(device)) if device.is_online => {}
            _ => return None,
        }

        let state = match self.device_control.state(device_id).await {
            Ok(state) => state,
            Err(e) => {
                // Debug, not warn: backends that cannot read state hit this on every call.
                tracing::debug!(error = %e, device_id, "sensors: live read unavailable");
                return None;
            }
        };

        state
            .values
            .into_iter()
            .find(|v| v.name == sensor_type)
            .map(|v| v.value)
    }

    /// Registered sensing devices that have no stored reading.
    async fn silent_sensors(&self, reported: &[(String, String)]) -> Vec<String> {
        let Ok(devices) = self.device_registry.list_devices().await else {
            return Vec::new();
        };
        devices
            .into_iter()
            .filter(|d| matches!(d.device_type.as_str(), "sensor" | "alarm"))
            .filter(|d| !reported.iter().any(|(device, _)| device == &d.id))
            .map(|d| format!("  {} ({}) — registered, no readings yet", d.id, d.name))
            .collect()
    }

    #[tool(
        description = "What a sensor reads NOW. Reads the device directly when it is \
                       reachable; otherwise returns the last stored reading, labelled \
                       STORED with its age — say so rather than presenting it as \
                       current. Requires device_id and sensor_type. Never guess values."
    )]
    async fn get_sensor_reading(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<GetSensorReadingParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("get_sensor_reading");

        let device_id = resolve_str_param(
            &params.0.device_id,
            &params.0.extra,
            &["device_id", "device", "room", "location"],
        );
        let sensor_type = resolve_str_param(
            &params.0.sensor_type,
            &params.0.extra,
            &["sensor_type", "type", "sensor"],
        );

        let (Some(device_id), Some(sensor_type)) = (device_id, sensor_type) else {
            return Ok(CallToolResult::success(vec![Content::text(
                // No example values: a model copies them, and unknown ids pass through unresolved.
                "Please provide both `device_id` and `sensor_type`, taken from the \
                 device and reading the user asked about.",
            )]));
        };

        let device_id = self.resolved_device(&device_id).await;

        // Live first: stored rows can be arbitrarily old, since only changes are written.
        if let Some(value) = self.live_reading(&device_id, &sensor_type).await {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Current {sensor_type} reading from '{device_id}': {value} (read from the \
                 device just now)"
            ))]));
        }

        match self
            .sensor_storage
            .get_latest(&device_id, &sensor_type)
            .await
        {
            Ok(Some(r)) => Ok(CallToolResult::success(vec![Content::text(format!(
                // Says STORED and how old, or a model relays it as the current reading.
                "Last STORED {} reading from '{}': {} — recorded {}, {} ago. The device \
                 could not be read just now, so this may no longer be true.",
                r.sensor_type,
                r.device_id,
                render_reading(&r.sensor_type, r.value, &r.unit),
                r.recorded_at.format("%Y-%m-%d %H:%M:%S UTC"),
                describe_age(Utc::now() - r.recorded_at),
            ))])),
            Ok(None) => Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_no_results(
                    &format!(
                        "a '{}' reading for device '{}' (the sensor may not have \
                         reported yet, or the id may be wrong)",
                        sensor_type, device_id
                    ),
                    &["giap-sensors__list_sensors"],
                ),
            )])),
            Err(e) => {
                tracing::warn!(error = %e, device_id, sensor_type, "sensors: get_latest failed");
                Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_dead_end(
                        "a sensor reading",
                        "The sensor store could not be read. Every sensor tool uses \
                         the same store, so no other tool will help — tell the user \
                         the sensor data is unavailable right now.",
                    ),
                )]))
            }
        }
    }

    #[tool(
        description = "Get sensor history for a device, optional time range. agg='min'|'max'|'avg' for one value; omit for raw series."
    )]
    async fn get_sensor_history(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<GetSensorHistoryParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("get_sensor_history");

        let device_id = resolve_str_param(
            &params.0.device_id,
            &params.0.extra,
            &["device_id", "device", "room"],
        );
        let sensor_type = resolve_str_param(
            &params.0.sensor_type,
            &params.0.extra,
            &["sensor_type", "type", "sensor"],
        );

        let (Some(device_id), Some(sensor_type)) = (device_id, sensor_type) else {
            return Ok(CallToolResult::success(vec![Content::text(
                "Please provide both `device_id` and `sensor_type`.",
            )]));
        };

        let since = params.0.since.as_deref().and_then(parse_datetime);
        let until = params.0.until.as_deref().and_then(parse_datetime);
        let agg = params.0.agg.as_deref().map(str::to_lowercase);

        let readings = match self
            .sensor_storage
            .get_history(&device_id, &sensor_type, since, until)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, device_id, sensor_type, "sensors: get_history failed");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_dead_end(
                        "sensor history",
                        "The sensor store could not be read. Every sensor tool uses \
                         the same store, so no other tool will help — tell the user \
                         the sensor data is unavailable right now.",
                    ),
                )]));
            }
        };

        if readings.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_no_results(
                    &format!(
                        "'{}' history for device '{}'{}",
                        sensor_type,
                        device_id,
                        if since.is_some() || until.is_some() {
                            " in the requested time range"
                        } else {
                            ""
                        },
                    ),
                    &[
                        "giap-sensors__get_sensor_reading",
                        "giap-sensors__list_sensors",
                    ],
                ),
            )]));
        }

        let unit = &readings[0].unit;
        let values: Vec<f64> = readings.iter().map(|r| r.value).collect();

        let text = match agg.as_deref() {
            Some("min") => {
                let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
                format!("Min {} for '{}': {} {}", sensor_type, device_id, min, unit)
            }
            Some("max") => {
                let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                format!("Max {} for '{}': {} {}", sensor_type, device_id, max, unit)
            }
            Some("avg") => {
                let avg = values.iter().sum::<f64>() / values.len() as f64;
                format!(
                    "Average {} for '{}': {:.2} {} (over {} readings)",
                    sensor_type,
                    device_id,
                    avg,
                    unit,
                    values.len()
                )
            }
            _ => {
                let lines: Vec<String> = readings
                    .iter()
                    .take(50)
                    .map(|r| {
                        format!(
                            "  {} — {}",
                            r.recorded_at.format("%Y-%m-%d %H:%M"),
                            render_reading(&r.sensor_type, r.value, &r.unit)
                        )
                    })
                    .collect();
                format!(
                    "{} history for '{}' ({} readings):\n{}",
                    sensor_type,
                    device_id,
                    readings.len(),
                    lines.join("\n"),
                )
            }
        };

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        description = "List all device + sensor_type pairs with stored readings. Use to discover available sensor data."
    )]
    async fn list_sensors(
        &self,
        _ctx: RequestContext<RoleServer>,
        _params: Parameters<ListSensorsParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        crate::set_current_tool("list_sensors");

        match self.sensor_storage.list_sensors().await {
            Ok(pairs) => {
                let mut lines: Vec<String> = pairs
                    .iter()
                    .map(|(device, stype)| format!("  {device} — {stype}"))
                    .collect();
                let silent = self.silent_sensors(&pairs).await;
                let total = lines.len() + silent.len();
                lines.extend(silent);

                if lines.is_empty() {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "No sensors are registered, and no readings have been recorded.",
                    )]));
                }
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Known sensors ({total} total):\n{}",
                    lines.join("\n"),
                ))]))
            }
            Err(e) => {
                tracing::warn!(error = %e, "sensors: list_sensors failed");
                Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_dead_end(
                        "the sensor list",
                        "The sensor store could not be read. Every sensor tool uses \
                         the same store, so no other tool will help — tell the user \
                         the sensor data is unavailable right now.",
                    ),
                )]))
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for SensorsMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-sensors",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Sensor MCP server — query stored IoT sensor readings.\n\n\
                 Tools:\n\
                 - get_sensor_reading: latest value for a device + sensor type\n\
                 - get_sensor_history: time-range history with optional min/max/avg aggregation\n\
                 - list_sensors: discover all device + sensor type pairs with stored data\n\n\
                 Never guess sensor values — always use these tools to retrieve stored readings.",
            )
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn resolve_str_param(
    direct: &Option<String>,
    extra: &std::collections::HashMap<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    if let Some(s) = direct.as_deref().filter(|s| !s.trim().is_empty()) {
        return Some(s.trim().to_string());
    }
    for key in keys {
        if let Some(val) = extra.get(*key).and_then(|v| v.as_str()) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn parse_datetime(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    s.parse::<chrono::DateTime<chrono::Utc>>().ok().or_else(|| {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
            .ok()
            .map(|ndt| ndt.and_utc())
    })
}

// ── Static deps + spawn function for the Goose builtin registry ───────────────

use tokio::io::DuplexStream;

struct SensorDeps {
    sensor_storage: Arc<dyn SensorStorage + Send + Sync>,
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    device_control: Arc<dyn DeviceControlPort>,
}

static SENSOR_DEPS: OnceLock<SensorDeps> = OnceLock::new();

/// Install the server's deps; call once at startup, before any session loads the extension.
pub fn init_sensor_deps(
    sensor_storage: Arc<dyn SensorStorage + Send + Sync>,
    device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    device_control: Arc<dyn DeviceControlPort>,
) {
    let _ = SENSOR_DEPS.set(SensorDeps {
        sensor_storage,
        device_registry,
        device_control,
    });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_sensor_server(reader: DuplexStream, writer: DuplexStream) {
    let Some(deps) = SENSOR_DEPS.get() else {
        tracing::error!(
            "spawn_sensor_server called before init_sensor_deps — sensor MCP server will not start"
        );
        return;
    };
    let server = SensorsMcpServer::new(
        deps.sensor_storage.clone(),
        deps.device_registry.clone(),
        deps.device_control.clone(),
    );
    crate::serve_builtin("giap-sensors", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use pond_core::user_data::domain::sensor::SensorReading;
    use pond_core::user_data::ports::device_control::{
        DeviceControlOutcome, DeviceState, StateValue,
    };
    use pond_core::user_data::ports::device_registry::DeviceRegistry;
    use pond_core::user_data::ports::sensor_storage::SensorStorage;
    use rmcp::model::RequestId;
    use rmcp::service::serve_directly;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    use pond_core::user_data::ports::device_registry::{Device, RegisterDeviceRequest};

    /// Local stub: the pond-core mocks sit behind a feature gate.
    struct StubRegistry(Vec<Device>);

    #[async_trait]
    impl DeviceRegistry for StubRegistry {
        async fn register(&self, _: RegisterDeviceRequest) -> Result<Device> {
            unimplemented!("not exercised by the sensor tools")
        }
        async fn list_devices(&self) -> Result<Vec<Device>> {
            Ok(self.0.clone())
        }
        async fn get_device(&self, id: &str) -> Result<Option<Device>> {
            Ok(self.0.iter().find(|d| d.id == id).cloned())
        }
        async fn unregister(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn heartbeat(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn rename(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
    }

    fn empty_registry() -> Arc<dyn DeviceRegistry + Send + Sync> {
        Arc::new(StubRegistry(Vec::new()))
    }

    /// Cannot read state, like every non-Matter backend (the default `state` bails).
    struct MuteControl;

    #[async_trait]
    impl DeviceControlPort for MuteControl {
        async fn set_power(&self, _: &str, _: bool) -> Result<DeviceControlOutcome> {
            unimplemented!("not exercised by the sensor tools")
        }
        async fn set_brightness(&self, _: &str, _: u8) -> Result<DeviceControlOutcome> {
            unimplemented!("not exercised by the sensor tools")
        }
        async fn set_target_temp(&self, _: &str, _: f32) -> Result<DeviceControlOutcome> {
            unimplemented!("not exercised by the sensor tools")
        }
        async fn set_locked(&self, _: &str, _: bool) -> Result<DeviceControlOutcome> {
            unimplemented!("not exercised by the sensor tools")
        }
    }

    fn no_live_control() -> Arc<dyn DeviceControlPort> {
        Arc::new(MuteControl)
    }

    /// A control port that answers `state` with the values it was given.
    struct SpeakingControl(Vec<(String, String)>);

    #[async_trait]
    impl DeviceControlPort for SpeakingControl {
        async fn set_power(&self, _: &str, _: bool) -> Result<DeviceControlOutcome> {
            unimplemented!("not exercised by the sensor tools")
        }
        async fn set_brightness(&self, _: &str, _: u8) -> Result<DeviceControlOutcome> {
            unimplemented!("not exercised by the sensor tools")
        }
        async fn set_target_temp(&self, _: &str, _: f32) -> Result<DeviceControlOutcome> {
            unimplemented!("not exercised by the sensor tools")
        }
        async fn set_locked(&self, _: &str, _: bool) -> Result<DeviceControlOutcome> {
            unimplemented!("not exercised by the sensor tools")
        }
        async fn state(&self, device_id: &str) -> Result<DeviceState> {
            Ok(DeviceState {
                device_id: device_id.to_string(),
                values: self
                    .0
                    .iter()
                    .map(|(name, value)| StateValue {
                        name: name.clone(),
                        value: value.clone(),
                    })
                    .collect(),
            })
        }
    }

    fn sensor_device(id: &str, name: &str) -> Device {
        Device {
            id: id.to_string(),
            name: name.to_string(),
            device_type: "sensor".to_string(),
            hostname: None,
            ip_address: None,
            capabilities: Vec::new(),
            registered_at: Utc::now().to_rfc3339(),
            last_seen: None,
            is_online: true,
            room: None,
        }
    }

    // Minimal in-test stub — avoids the `pond-core/test-mocks` feature gate.
    struct StubStorage {
        readings: Arc<RwLock<Vec<SensorReading>>>,
    }

    impl StubStorage {
        fn new() -> Self {
            Self {
                readings: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl SensorStorage for StubStorage {
        async fn record(&self, reading: SensorReading) -> Result<()> {
            self.readings.write().await.push(reading);
            Ok(())
        }

        async fn get_latest(
            &self,
            device_id: &str,
            sensor_type: &str,
        ) -> Result<Option<SensorReading>> {
            let r = self.readings.read().await;
            Ok(r.iter()
                .filter(|r| r.device_id == device_id && r.sensor_type == sensor_type)
                .max_by_key(|r| r.recorded_at)
                .cloned())
        }

        async fn get_recent(&self, device_id: &str, limit: usize) -> Result<Vec<SensorReading>> {
            let r = self.readings.read().await;
            let mut v: Vec<_> = r
                .iter()
                .filter(|r| r.device_id == device_id)
                .cloned()
                .collect();
            v.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
            v.truncate(limit);
            Ok(v)
        }

        async fn get_history(
            &self,
            device_id: &str,
            sensor_type: &str,
            since: Option<DateTime<Utc>>,
            until: Option<DateTime<Utc>>,
        ) -> Result<Vec<SensorReading>> {
            let r = self.readings.read().await;
            let mut v: Vec<_> = r
                .iter()
                .filter(|r| {
                    r.device_id == device_id
                        && r.sensor_type == sensor_type
                        && since.map_or(true, |s| r.recorded_at >= s)
                        && until.map_or(true, |u| r.recorded_at < u)
                })
                .cloned()
                .collect();
            v.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
            Ok(v)
        }

        async fn list_sensors(&self) -> Result<Vec<(String, String)>> {
            let r = self.readings.read().await;
            let mut seen = std::collections::HashSet::new();
            let mut result = Vec::new();
            for reading in r.iter() {
                let key = (reading.device_id.clone(), reading.sensor_type.clone());
                if seen.insert(key.clone()) {
                    result.push(key);
                }
            }
            result.sort();
            Ok(result)
        }
    }

    fn make_ctx() -> RequestContext<RoleServer> {
        let (_client, stream) = tokio::io::duplex(64);
        let running = serve_directly(
            SensorsMcpServer::new(
                Arc::new(StubStorage::new()),
                empty_registry(),
                no_live_control(),
            ),
            stream,
            None,
        );
        RequestContext::new(RequestId::Number(0), running.peer().clone())
    }

    fn reading(device_id: &str, sensor_type: &str, value: f64) -> SensorReading {
        SensorReading {
            device_id: device_id.to_string(),
            sensor_type: sensor_type.to_string(),
            value,
            unit: "C".to_string(),
            recorded_at: chrono::Utc::now(),
        }
    }

    fn text_of(result: CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text())
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join("")
    }

    #[tokio::test]
    async fn a_reachable_device_is_read_now_rather_than_recalled() {
        let storage = Arc::new(StubStorage::new());
        storage
            .record(reading("matter-5", "flow", 197.8))
            .await
            .unwrap();

        let registry: Arc<dyn DeviceRegistry + Send + Sync> =
            Arc::new(StubRegistry(vec![sensor_device("matter-5", "Flow Sensor")]));
        let live: Arc<dyn DeviceControlPort> = Arc::new(SpeakingControl(vec![(
            "flow".to_string(),
            "12.4 m3/h".to_string(),
        )]));

        let server = SensorsMcpServer::new(storage, registry, live);
        let params = Parameters(GetSensorReadingParams {
            device_id: Some("matter-5".to_string()),
            sensor_type: Some("flow".to_string()),
            extra: Default::default(),
        });
        let text = text_of(server.get_sensor_reading(make_ctx(), params).await.unwrap());

        assert!(text.contains("12.4"), "the device was not asked: {text}");
        assert!(
            !text.contains("197.8"),
            "served the stored row anyway: {text}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_device_falls_back_to_the_store_and_says_so() {
        let storage = Arc::new(StubStorage::new());
        storage
            .record(reading("matter-5", "flow", 197.8))
            .await
            .unwrap();

        let mut absent = sensor_device("matter-5", "Flow Sensor");
        absent.is_online = false;
        let registry: Arc<dyn DeviceRegistry + Send + Sync> = Arc::new(StubRegistry(vec![absent]));
        // Would answer: proves reachability, not a failed call, gates the read.
        let live: Arc<dyn DeviceControlPort> = Arc::new(SpeakingControl(vec![(
            "flow".to_string(),
            "12.4 m3/h".to_string(),
        )]));

        let server = SensorsMcpServer::new(storage, registry, live);
        let params = Parameters(GetSensorReadingParams {
            device_id: Some("matter-5".to_string()),
            sensor_type: Some("flow".to_string()),
            extra: Default::default(),
        });
        let text = text_of(server.get_sensor_reading(make_ctx(), params).await.unwrap());

        assert!(text.contains("197.8"), "lost the stored reading: {text}");
        assert!(text.contains("STORED"), "did not say it was stored: {text}");
        assert!(
            text.contains("ago"),
            "did not say how old it was, which is the part a model can act on: {text}"
        );
    }

    #[tokio::test]
    async fn a_device_that_does_not_report_this_sensor_falls_back_too() {
        let storage = Arc::new(StubStorage::new());
        storage
            .record(reading("matter-5", "temperature", 20.0))
            .await
            .unwrap();

        let registry: Arc<dyn DeviceRegistry + Send + Sync> =
            Arc::new(StubRegistry(vec![sensor_device("matter-5", "Flow Sensor")]));
        let live: Arc<dyn DeviceControlPort> = Arc::new(SpeakingControl(vec![(
            "flow".to_string(),
            "12.4 m3/h".to_string(),
        )]));

        let server = SensorsMcpServer::new(storage, registry, live);
        let params = Parameters(GetSensorReadingParams {
            device_id: Some("matter-5".to_string()),
            sensor_type: Some("temperature".to_string()),
            extra: Default::default(),
        });
        let text = text_of(server.get_sensor_reading(make_ctx(), params).await.unwrap());

        assert!(text.contains("20"), "lost the stored reading: {text}");
        assert!(text.contains("STORED"), "did not say it was stored: {text}");
        assert!(!text.contains("12.4"), "reported the wrong sensor: {text}");
    }

    #[test]
    fn an_age_is_stated_in_the_coarsest_unit_that_is_still_true() {
        assert_eq!(describe_age(chrono::Duration::seconds(12)), "12s");
        assert_eq!(describe_age(chrono::Duration::minutes(14)), "14min");
        assert_eq!(describe_age(chrono::Duration::hours(5)), "5h");
        assert_eq!(describe_age(chrono::Duration::days(3)), "3d");
        // A clock that went backwards must not read as a reading from the future.
        assert_eq!(describe_age(chrono::Duration::seconds(-30)), "0s");
    }

    #[tokio::test]
    async fn get_sensor_reading_returns_latest() {
        let storage = Arc::new(StubStorage::new());
        storage
            .record(reading("bedroom", "temperature", 21.0))
            .await
            .unwrap();
        storage
            .record(reading("bedroom", "temperature", 23.5))
            .await
            .unwrap();

        let server = SensorsMcpServer::new(storage, empty_registry(), no_live_control());
        let params = Parameters(GetSensorReadingParams {
            device_id: Some("bedroom".to_string()),
            sensor_type: Some("temperature".to_string()),
            extra: Default::default(),
        });
        let text = text_of(server.get_sensor_reading(make_ctx(), params).await.unwrap());
        assert!(
            text.contains("23.5"),
            "expected latest value 23.5 in: {text}"
        );
        assert!(text.contains("bedroom"), "expected device_id in: {text}");
    }

    #[tokio::test]
    async fn get_sensor_reading_missing_params_returns_guidance() {
        let server = SensorsMcpServer::new(
            Arc::new(StubStorage::new()),
            empty_registry(),
            no_live_control(),
        );
        let params = Parameters(GetSensorReadingParams::default());
        let text = text_of(server.get_sensor_reading(make_ctx(), params).await.unwrap());
        assert!(
            text.contains("device_id"),
            "should prompt for device_id: {text}"
        );
    }

    #[tokio::test]
    async fn list_sensors_shows_known_devices() {
        let storage = Arc::new(StubStorage::new());
        storage
            .record(reading("bedroom", "temperature", 21.0))
            .await
            .unwrap();
        storage
            .record(reading("kitchen", "humidity", 55.0))
            .await
            .unwrap();

        let server = SensorsMcpServer::new(storage, empty_registry(), no_live_control());
        let text = text_of(
            server
                .list_sensors(make_ctx(), Parameters(ListSensorsParams::default()))
                .await
                .unwrap(),
        );
        assert!(text.contains("bedroom"), "should list bedroom: {text}");
        assert!(text.contains("kitchen"), "should list kitchen: {text}");
    }

    #[tokio::test]
    async fn a_registered_sensor_is_listed_even_before_it_reports() {
        let registry = Arc::new(StubRegistry(vec![sensor_device(
            "matter-18",
            "Air Quality Sensor",
        )]));
        let server =
            SensorsMcpServer::new(Arc::new(StubStorage::new()), registry, no_live_control());

        let text = text_of(
            server
                .list_sensors(make_ctx(), Parameters(ListSensorsParams::default()))
                .await
                .unwrap(),
        );
        assert!(text.contains("matter-18"), "names the device: {text}");
        assert!(
            text.contains("no readings yet"),
            "and says why it has nothing to show: {text}"
        );
    }

    #[tokio::test]
    async fn a_reporting_sensor_is_listed_once() {
        let storage = Arc::new(StubStorage::new());
        storage
            .record(reading("matter-18", "air_quality", 3.0))
            .await
            .unwrap();
        let registry = Arc::new(StubRegistry(vec![sensor_device(
            "matter-18",
            "Air Quality Sensor",
        )]));

        let text = text_of(
            SensorsMcpServer::new(storage, registry, no_live_control())
                .list_sensors(make_ctx(), Parameters(ListSensorsParams::default()))
                .await
                .unwrap(),
        );
        assert_eq!(text.matches("matter-18").count(), 1, "listed once: {text}");
        assert!(!text.contains("no readings yet"), "{text}");
    }

    #[tokio::test]
    async fn get_sensor_history_avg_aggregation() {
        let storage = Arc::new(StubStorage::new());
        storage
            .record(reading("living-room", "temperature", 20.0))
            .await
            .unwrap();
        storage
            .record(reading("living-room", "temperature", 24.0))
            .await
            .unwrap();
        storage
            .record(reading("living-room", "temperature", 22.0))
            .await
            .unwrap();

        let server = SensorsMcpServer::new(storage, empty_registry(), no_live_control());
        let params = Parameters(GetSensorHistoryParams {
            device_id: Some("living-room".to_string()),
            sensor_type: Some("temperature".to_string()),
            since: None,
            until: None,
            agg: Some("avg".to_string()),
            extra: Default::default(),
        });
        let text = text_of(server.get_sensor_history(make_ctx(), params).await.unwrap());
        assert!(text.contains("22.00"), "expected avg 22.00 in: {text}");
    }

    #[test]
    fn server_constructs() {
        let _server = SensorsMcpServer::new(
            Arc::new(StubStorage::new()),
            empty_registry(),
            no_live_control(),
        );
    }

    #[test]
    fn parse_datetime_rfc3339() {
        assert!(parse_datetime("2024-01-15T10:00:00Z").is_some());
        assert!(parse_datetime("not-a-date").is_none());
    }
}
