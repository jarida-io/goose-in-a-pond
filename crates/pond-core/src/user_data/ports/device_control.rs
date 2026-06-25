//! Driven Port: Device Control
//!
//! The actuation seam for smart devices. The agent (and the desktop Hub via
//! `POST /api/v1/tools/invoke`) reaches devices only through the
//! `giap-device-control` MCP tool, which calls this port. Concrete backends
//! (MQTT / HTTP / IR, or a Home-Assistant MCP-client) implement it; a logging
//! stub is the default until a real adapter is wired.
//!
//! Capability-typed (no opaque JSON state) so the control boundary is
//! machine-checkable end-to-end.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The device state produced by a control action — echoed back so callers
/// (e.g. the Hub overlay) can reconcile their optimistic UI with reality.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceStatePatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    /// Brightness as a 0–100 percentage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u8>,
    /// Target temperature in degrees Celsius.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_temp: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
}

/// Outcome of a control action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceControlOutcome {
    pub device_id: String,
    /// The resulting state after the action (best-effort echo).
    pub applied: DeviceStatePatch,
}

impl DeviceControlOutcome {
    pub fn new(device_id: impl Into<String>, applied: DeviceStatePatch) -> Self {
        Self {
            device_id: device_id.into(),
            applied,
        }
    }
}

/// Driven Port: actuate a smart device.
#[async_trait]
pub trait DeviceControlPort: Send + Sync {
    /// Turn a device on or off.
    async fn set_power(&self, device_id: &str, on: bool) -> Result<DeviceControlOutcome>;

    /// Set brightness as a 0–100 percentage.
    async fn set_brightness(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome>;

    /// Set a thermostat target temperature in degrees Celsius.
    async fn set_target_temp(&self, device_id: &str, celsius: f32) -> Result<DeviceControlOutcome>;

    /// Lock or unlock a device.
    async fn set_locked(&self, device_id: &str, locked: bool) -> Result<DeviceControlOutcome>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::mocks::mock_device_control::RecordingDeviceControl;

    #[tokio::test]
    async fn set_power_records_and_echoes() {
        let dc = RecordingDeviceControl::default();
        let out = dc.set_power("lamp-1", true).await.unwrap();
        assert_eq!(out.device_id, "lamp-1");
        assert_eq!(out.applied.on, Some(true));
        assert_eq!(
            dc.last_call().as_deref(),
            Some("set_power(lamp-1, on=true)")
        );
    }

    #[tokio::test]
    async fn set_brightness_implies_on_when_positive() {
        let dc = RecordingDeviceControl::default();
        let out = dc.set_brightness("lamp-1", 40).await.unwrap();
        assert_eq!(out.applied.brightness, Some(40));
        assert_eq!(out.applied.on, Some(true));

        let off = dc.set_brightness("lamp-1", 0).await.unwrap();
        assert_eq!(off.applied.on, Some(false));
    }

    #[tokio::test]
    async fn set_target_temp_and_locked_echo() {
        let dc = RecordingDeviceControl::default();
        let t = dc.set_target_temp("thermo", 21.5).await.unwrap();
        assert_eq!(t.applied.target_temp, Some(21.5));

        let l = dc.set_locked("door", true).await.unwrap();
        assert_eq!(l.applied.locked, Some(true));
    }
}
