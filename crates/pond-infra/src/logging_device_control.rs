//! Logging `DeviceControlPort` stub; the production default until a real device backend exists.

use async_trait::async_trait;
use pond_core::user_data::ports::device_control::{
    DeviceControlOutcome, DeviceControlPort, DeviceStatePatch,
};

/// Logs control actions and echoes the requested state. Never fails.
#[derive(Debug, Default, Clone)]
pub struct LoggingDeviceControl;

impl LoggingDeviceControl {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DeviceControlPort for LoggingDeviceControl {
    async fn set_power(&self, device_id: &str, on: bool) -> anyhow::Result<DeviceControlOutcome> {
        tracing::info!(target: "device_control", device_id, on, "set_power (logging stub)");
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch {
                on: Some(on),
                ..Default::default()
            },
        ))
    }

    async fn set_brightness(
        &self,
        device_id: &str,
        percent: u8,
    ) -> anyhow::Result<DeviceControlOutcome> {
        tracing::info!(target: "device_control", device_id, percent, "set_brightness (logging stub)");
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch {
                brightness: Some(percent),
                on: Some(percent > 0),
                ..Default::default()
            },
        ))
    }

    async fn set_target_temp(
        &self,
        device_id: &str,
        celsius: f32,
    ) -> anyhow::Result<DeviceControlOutcome> {
        tracing::info!(target: "device_control", device_id, celsius, "set_target_temp (logging stub)");
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch {
                target_temp: Some(celsius),
                ..Default::default()
            },
        ))
    }

    async fn set_locked(
        &self,
        device_id: &str,
        locked: bool,
    ) -> anyhow::Result<DeviceControlOutcome> {
        tracing::info!(target: "device_control", device_id, locked, "set_locked (logging stub)");
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch {
                locked: Some(locked),
                ..Default::default()
            },
        ))
    }
}
