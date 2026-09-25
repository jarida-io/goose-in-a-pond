//! Driven port: capability-typed device actuation, reached only via `giap-device-control`.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// State after a control action, echoed so callers can reconcile optimistic UI.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceStatePatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<bool>,
    /// Brightness as a 0–100 percentage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u8>,
    /// Speaker level, 0–100. Not brightness, though Matter puts both on one cluster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<u8>,
    /// Target temperature in degrees Celsius.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_temp: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    /// Colour hue in degrees (0–360).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hue: Option<u16>,
    /// Colour saturation as a 0–100 percentage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saturation: Option<u8>,
    /// Colour temperature in kelvin, not the cluster's mireds (the controller converts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_temp: Option<u32>,
    /// Fan speed as a 0–100 percentage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fan_speed: Option<u8>,
    /// Fan mode by name: off, low, medium, high, on, auto, smart ("auto" has no percentage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fan_mode: Option<String>,
    /// Covering position as a 0–100 percentage **open** (100 = fully open).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<u8>,
    /// Slat angle as a 0–100 percentage **open**, a covering's second axis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt: Option<u8>,
    /// A valve, open (`true`) or shut (`false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valve: Option<bool>,
    /// The named setting that changed, and what it became.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeChange>,
    /// The operation that was run: start, stop, pause or resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
}

/// A named setting and its new value, both in the device's own words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeChange {
    pub setting: String,
    pub value: String,
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

/// One current reading (`spin speed` is `High`), named as the verb or setting that changes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateValue {
    pub name: String,
    pub value: String,
}

/// Everything a device currently reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceState {
    pub device_id: String,
    pub values: Vec<StateValue>,
}

/// What a device can be told to do and what it measures, in its own terms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceDescription {
    pub device_id: String,
    pub device_type: String,
    /// Verbs the device accepts, named as this port names them.
    pub capabilities: Vec<Capability>,
    /// What it measures, whether or not it has reported yet.
    pub sensors: Vec<SensorSpec>,
    /// Manufacturer-specific controls: present but not drivable (so not [`Capability`]s).
    #[serde(default)]
    pub vendor_clusters: Vec<VendorCluster>,
    /// What the device reports and nothing can set. Read-only by construction: every
    /// writable DoorLock attribute is a security control.
    #[serde(default)]
    pub states: Vec<StateSpec>,
}

/// Something a device reports under a name, which nothing can write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateSpec {
    /// The name [`DeviceControlPort::state`] reports it under.
    pub name: String,
    /// The words it takes; an enum is the closed list `state` may report.
    pub value: ValueSpec,
}

/// A control the device has and nothing here can name; Matter publishes no names for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VendorCluster {
    /// The 32-bit Matter cluster id, e.g. `0xfff1fc01`. Upper 16 bits are the vendor.
    pub cluster_id: u32,
    /// The endpoint carrying it, which is how a user tells two apart on one device.
    pub endpoint: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capability {
    pub verb: String,
    /// Which setting a repeated verb is (a washer's four `mode`s); the name `set_mode` takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting: Option<String>,
    pub value: ValueSpec,
}

/// The shape a verb accepts; constraints appear only when the device stated them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValueSpec {
    Boolean,
    /// 0–100.
    Percent,
    Number {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        /// The increment the device accepts, where it states one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        /// What this range is true of (e.g. "while heating"); absent when limits don't move.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when: Option<String>,
    },
    Enum {
        values: Vec<String>,
    },
    Color,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensorSpec {
    pub sensor_type: String,
    pub unit: String,
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

    // ── Optional capabilities ────────────────────────────────────────────
    // Default to an "unsupported" error; backends opt in by overriding.

    /// Set colour by hue (0–360 degrees) and saturation (0–100 percent).
    async fn set_color(
        &self,
        device_id: &str,
        _hue_degrees: u16,
        _saturation_percent: u8,
    ) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not support colour control")
    }

    /// Set a speaker's volume as a 0–100 percentage (Level Control, but not brightness).
    async fn set_volume(&self, device_id: &str, _percent: u8) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' has no speaker to set a volume on")
    }

    /// Set colour temperature in kelvin; separate from [`Self::set_color`] (white has no hue).
    async fn set_color_temp(&self, device_id: &str, _kelvin: u32) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not support colour temperature")
    }

    /// Set fan speed as a 0–100 percentage.
    async fn set_fan_speed(&self, device_id: &str, _percent: u8) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not support fan control")
    }

    /// Set fan mode by name: off, low, medium, high, on, auto, smart.
    async fn set_fan_mode(&self, device_id: &str, _mode: &str) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not support fan modes")
    }

    /// What this device can be told to do, and what it measures.
    async fn describe(&self, device_id: &str) -> Result<DeviceDescription> {
        anyhow::bail!("device '{device_id}' does not describe itself")
    }

    /// What this device currently is. Unsupported is an error, never an empty state.
    async fn state(&self, device_id: &str) -> Result<DeviceState> {
        anyhow::bail!("device '{device_id}' cannot report its state")
    }

    /// Choose a named setting (e.g. a wash cycle), as named by [`Self::describe`].
    async fn set_mode(
        &self,
        device_id: &str,
        _setting: &str,
        _value: &str,
    ) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' has no settings that can be chosen")
    }

    /// Start, stop, pause or resume a device that runs cycles.
    async fn set_operation(
        &self,
        device_id: &str,
        _operation: &str,
    ) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not run cycles")
    }

    /// Set a covering's slat angle, as a 0–100 percentage **open**.
    async fn set_tilt(&self, device_id: &str, _percent_open: u8) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not support tilt")
    }

    /// Set a covering's position or a valve's level, 0–100 percent **open** (100 = fully open).
    async fn set_position(
        &self,
        device_id: &str,
        _percent_open: u8,
    ) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' does not support position control")
    }

    /// Open or shut a valve; Matter valves take open/close, not on/off, and may have no level.
    async fn set_valve(&self, device_id: &str, _open: bool) -> Result<DeviceControlOutcome> {
        anyhow::bail!("device '{device_id}' has no valve to open or shut")
    }
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

    #[tokio::test]
    async fn set_valve_records_and_echoes() {
        let dc = RecordingDeviceControl::default();

        let opened = dc.set_valve("garden-valve", true).await.unwrap();
        assert_eq!(opened.applied.valve, Some(true));
        assert_eq!(
            dc.last_call().as_deref(),
            Some("set_valve(garden-valve, open=true)")
        );

        let shut = dc.set_valve("garden-valve", false).await.unwrap();
        assert_eq!(shut.applied.valve, Some(false));
        // Not reported as a power change: nothing was switched.
        assert_eq!(shut.applied.on, None);
    }

    #[tokio::test]
    async fn a_backend_without_valves_says_so() {
        struct PowerOnly;

        #[async_trait]
        impl DeviceControlPort for PowerOnly {
            async fn set_power(&self, device_id: &str, _on: bool) -> Result<DeviceControlOutcome> {
                Ok(DeviceControlOutcome::new(
                    device_id,
                    DeviceStatePatch::default(),
                ))
            }
            async fn set_brightness(
                &self,
                device_id: &str,
                _percent: u8,
            ) -> Result<DeviceControlOutcome> {
                Ok(DeviceControlOutcome::new(
                    device_id,
                    DeviceStatePatch::default(),
                ))
            }
            async fn set_target_temp(
                &self,
                device_id: &str,
                _celsius: f32,
            ) -> Result<DeviceControlOutcome> {
                Ok(DeviceControlOutcome::new(
                    device_id,
                    DeviceStatePatch::default(),
                ))
            }
            async fn set_locked(
                &self,
                device_id: &str,
                _locked: bool,
            ) -> Result<DeviceControlOutcome> {
                Ok(DeviceControlOutcome::new(
                    device_id,
                    DeviceStatePatch::default(),
                ))
            }
        }

        let error = PowerOnly.set_valve("lamp-1", true).await.unwrap_err();
        assert!(
            error.to_string().contains("lamp-1") && error.to_string().contains("valve"),
            "{error}"
        );
    }
}
