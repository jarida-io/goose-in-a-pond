//! Mock `DeviceControlPort`, shared with downstream `pond-api` tests.

use crate::user_data::ports::device_control::{
    DeviceControlOutcome, DeviceControlPort, DeviceStatePatch,
};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Mutex;

/// Records the last call as a string and echoes the requested state; always succeeds.
#[derive(Default)]
pub struct RecordingDeviceControl {
    last: Mutex<Option<String>>,
}

impl RecordingDeviceControl {
    /// The last control call, formatted (e.g. `"set_power(lamp-1, on=true)"`).
    pub fn last_call(&self) -> Option<String> {
        self.last.lock().unwrap().clone()
    }

    fn record(&self, call: String) {
        *self.last.lock().unwrap() = Some(call);
    }
}

#[async_trait]
impl DeviceControlPort for RecordingDeviceControl {
    async fn set_power(&self, device_id: &str, on: bool) -> Result<DeviceControlOutcome> {
        self.record(format!("set_power({device_id}, on={on})"));
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch {
                on: Some(on),
                ..Default::default()
            },
        ))
    }

    async fn set_brightness(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome> {
        self.record(format!("set_brightness({device_id}, {percent})"));
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch {
                brightness: Some(percent),
                on: Some(percent > 0),
                ..Default::default()
            },
        ))
    }

    async fn set_target_temp(&self, device_id: &str, celsius: f32) -> Result<DeviceControlOutcome> {
        self.record(format!("set_target_temp({device_id}, {celsius})"));
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch {
                target_temp: Some(celsius),
                ..Default::default()
            },
        ))
    }

    async fn set_locked(&self, device_id: &str, locked: bool) -> Result<DeviceControlOutcome> {
        self.record(format!("set_locked({device_id}, {locked})"));
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch {
                locked: Some(locked),
                ..Default::default()
            },
        ))
    }

    async fn set_valve(&self, device_id: &str, open: bool) -> Result<DeviceControlOutcome> {
        self.record(format!("set_valve({device_id}, open={open})"));
        Ok(DeviceControlOutcome::new(
            device_id,
            DeviceStatePatch {
                valve: Some(open),
                ..Default::default()
            },
        ))
    }
}
