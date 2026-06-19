//! Device Control MCP Server — actuation.
//!
//! Provides one tool: `set_device_state`, which actuates a device through the
//! [`DeviceControlPort`] (power / brightness / target temperature / lock). The
//! agent and the desktop Hub (via `POST /api/v1/tools/invoke`) both reach
//! devices through this single tool surface. Backends are pluggable behind the
//! port (logging stub today; MQTT/HTTP/IR or a Home-Assistant MCP-client later).

use pond_core::user_data::ports::device_control::DeviceControlPort;
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
// All params optional with serde(default) + a flatten extra absorber so a small
// model sending `{}` (or unexpected fields) never breaks deserialization.

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SetDeviceStateParams {
    /// The device to control — its id or name (e.g. "living-room-lamp").
    #[serde(default)]
    pub device_id: String,
    /// Turn the device on (true) or off (false).
    #[serde(default)]
    pub power: Option<bool>,
    /// Brightness as a 0–100 percentage.
    #[serde(default)]
    pub brightness: Option<u8>,
    /// Thermostat target temperature in degrees Celsius.
    #[serde(default)]
    pub target_temp: Option<f32>,
    /// Lock (true) or unlock (false).
    #[serde(default)]
    pub locked: Option<bool>,
    /// Absorbs any unexpected fields a small model might emit.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct DeviceControlMcpServer {
    control: Arc<dyn DeviceControlPort + Send + Sync>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl DeviceControlMcpServer {
    pub fn new(control: Arc<dyn DeviceControlPort + Send + Sync>) -> Self {
        Self {
            control,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Control a smart device: turn power on/off, set brightness (0-100), set thermostat target temperature, or lock/unlock. Use when the user asks to change a device's state."
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
        {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No change requested for '{device_id}'. Specify one of: power (on/off), \
                 brightness (0-100), target_temp (°C), locked (true/false)."
            ))]));
        }

        let mut applied: Vec<String> = Vec::new();

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

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Set {device_id}: {}.",
            applied.join(", ")
        ))]))
    }
}

/// Error-as-guidance: the LLM reads this and adapts (per the MCP server standard).
fn guidance(msg: String) -> CallToolResult {
    CallToolResult::success(vec![Content::text(msg)])
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
                 lock/unlock). Provide device_id plus the field(s) to change.",
            )
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────────

use rmcp::ServiceExt;
use std::sync::OnceLock;
use tokio::io::DuplexStream;

static DEVICE_CONTROL_DEPS: OnceLock<Arc<dyn DeviceControlPort + Send + Sync>> = OnceLock::new();

/// Initialize device-control server dependencies. Call once at startup.
pub fn init_device_control_deps(control: Arc<dyn DeviceControlPort + Send + Sync>) {
    let _ = DEVICE_CONTROL_DEPS.set(control);
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_device_control_server(reader: DuplexStream, writer: DuplexStream) {
    let control = DEVICE_CONTROL_DEPS
        .get()
        .expect("init_device_control_deps() not called")
        .clone();
    let server = DeviceControlMcpServer::new(control);
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-device-control MCP server failed: {e}"),
        }
    });
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pond_core::user_data::ports::device_control::{DeviceControlOutcome, DeviceStatePatch};

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

    fn test_server() -> DeviceControlMcpServer {
        DeviceControlMcpServer::new(Arc::new(StubControl))
    }

    #[test]
    fn server_constructs() {
        let _server = test_server();
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
