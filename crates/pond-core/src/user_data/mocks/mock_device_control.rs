//! Mock `DeviceControlPort`, shared with downstream `pond-api` tests.

use crate::user_data::ports::device_control::{
    DeviceControlOutcome, DeviceControlPort, DeviceStatePatch,
};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Mutex;

/// Records the last call as a string and echoes the requested state; always succeeds.
/// Also accumulates every applied field, so [`Self::state_patch`] answers like a real backend
/// that remembers what it was last told.
#[derive(Default)]
pub struct RecordingDeviceControl {
    last: Mutex<Option<String>>,
    state: Mutex<DeviceStatePatch>,
}

impl RecordingDeviceControl {
    /// The last control call, formatted (e.g. `"set_power(lamp-1, on=true)"`).
    pub fn last_call(&self) -> Option<String> {
        self.last.lock().unwrap().clone()
    }

    fn record(&self, call: String, applied: &DeviceStatePatch) {
        *self.last.lock().unwrap() = Some(call);
        self.state.lock().unwrap().merge_from(applied);
    }
}

#[async_trait]
impl DeviceControlPort for RecordingDeviceControl {
    async fn set_power(&self, device_id: &str, on: bool) -> Result<DeviceControlOutcome> {
        let applied = DeviceStatePatch {
            on: Some(on),
            ..Default::default()
        };
        self.record(format!("set_power({device_id}, on={on})"), &applied);
        Ok(DeviceControlOutcome::new(device_id, applied))
    }

    async fn set_brightness(&self, device_id: &str, percent: u8) -> Result<DeviceControlOutcome> {
        let applied = DeviceStatePatch {
            brightness: Some(percent),
            on: Some(percent > 0),
            ..Default::default()
        };
        self.record(format!("set_brightness({device_id}, {percent})"), &applied);
        Ok(DeviceControlOutcome::new(device_id, applied))
    }

    async fn set_target_temp(&self, device_id: &str, celsius: f32) -> Result<DeviceControlOutcome> {
        let applied = DeviceStatePatch {
            target_temp: Some(celsius),
            ..Default::default()
        };
        self.record(format!("set_target_temp({device_id}, {celsius})"), &applied);
        Ok(DeviceControlOutcome::new(device_id, applied))
    }

    async fn set_locked(&self, device_id: &str, locked: bool) -> Result<DeviceControlOutcome> {
        let applied = DeviceStatePatch {
            locked: Some(locked),
            ..Default::default()
        };
        self.record(format!("set_locked({device_id}, {locked})"), &applied);
        Ok(DeviceControlOutcome::new(device_id, applied))
    }

    async fn set_valve(&self, device_id: &str, open: bool) -> Result<DeviceControlOutcome> {
        let applied = DeviceStatePatch {
            valve: Some(open),
            ..Default::default()
        };
        self.record(format!("set_valve({device_id}, open={open})"), &applied);
        Ok(DeviceControlOutcome::new(device_id, applied))
    }

    async fn state_patch(&self, _device_id: &str) -> Result<DeviceStatePatch> {
        Ok(self.state.lock().unwrap().clone())
    }
}
