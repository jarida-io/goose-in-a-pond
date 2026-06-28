//! In-memory [`DeviceController`] test double (#84).
//!
//! Records control commands against an in-memory state map so Core tests (and,
//! later, the rules engine) can exercise device control without a real
//! protocol adapter. Capabilities are declared up front via [`with_device`];
//! control calls auto-create an entry so simple tests need no setup.
//!
//! [`with_device`]: MockDeviceController::with_device

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::device::{
    DeviceCapability, DeviceId, DeviceState, DeviceStateValue, POWER_KEY,
};
use crate::user_data::ports::device_controller::DeviceController;

#[derive(Default)]
struct MockDevice {
    capabilities: Vec<DeviceCapability>,
    state: DeviceState,
}

/// Test double for [`DeviceController`]. Thread-safe; cheap to clone via `Arc`.
pub struct MockDeviceController {
    devices: Mutex<HashMap<DeviceId, MockDevice>>,
}

impl MockDeviceController {
    pub fn new() -> Self {
        Self {
            devices: Mutex::new(HashMap::new()),
        }
    }

    /// Pre-register a device with a declared capability set (builder style).
    pub fn with_device(self, id: impl Into<DeviceId>, capabilities: Vec<DeviceCapability>) -> Self {
        self.devices.lock().unwrap().insert(
            id.into(),
            MockDevice {
                capabilities,
                state: DeviceState::new(),
            },
        );
        self
    }
}

impl Default for MockDeviceController {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DeviceController for MockDeviceController {
    async fn set_power(&self, device: &DeviceId, on: bool) -> Result<()> {
        self.set_state(device, POWER_KEY, DeviceStateValue::Bool(on))
            .await
    }

    async fn set_state(&self, device: &DeviceId, key: &str, value: DeviceStateValue) -> Result<()> {
        let mut devices = self.devices.lock().unwrap();
        devices
            .entry(device.clone())
            .or_default()
            .state
            .set(key, value);
        Ok(())
    }

    async fn query_state(&self, device: &DeviceId) -> Result<DeviceState> {
        let devices = self.devices.lock().unwrap();
        Ok(devices
            .get(device)
            .map(|d| d.state.clone())
            .unwrap_or_default())
    }

    async fn capabilities(&self, device: &DeviceId) -> Result<Vec<DeviceCapability>> {
        let devices = self.devices.lock().unwrap();
        Ok(devices
            .get(device)
            .map(|d| d.capabilities.clone())
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn set_power_roundtrips_through_query() {
        let ctrl = MockDeviceController::new();
        let id = DeviceId::new("lamp-1");

        ctrl.set_power(&id, true).await.unwrap();
        assert_eq!(ctrl.query_state(&id).await.unwrap().power(), Some(true));

        ctrl.set_power(&id, false).await.unwrap();
        assert_eq!(ctrl.query_state(&id).await.unwrap().power(), Some(false));
    }

    #[tokio::test]
    async fn set_state_preserves_value_type() {
        let ctrl = MockDeviceController::new();
        let id = DeviceId::new("lamp-1");

        ctrl.set_state(&id, "brightness", DeviceStateValue::Int(80))
            .await
            .unwrap();
        let state = ctrl.query_state(&id).await.unwrap();
        assert_eq!(
            state.get("brightness").and_then(DeviceStateValue::as_int),
            Some(80)
        );
    }

    #[tokio::test]
    async fn capabilities_reports_registered_set() {
        let ctrl = MockDeviceController::new().with_device(
            "lamp-1",
            vec![DeviceCapability::Power, DeviceCapability::Dimmable],
        );
        let caps = ctrl.capabilities(&DeviceId::new("lamp-1")).await.unwrap();
        assert!(caps.contains(&DeviceCapability::Dimmable));
        // Unknown device reports no capabilities rather than erroring.
        assert!(ctrl
            .capabilities(&DeviceId::new("unknown"))
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn usable_as_trait_object() {
        let ctrl: Arc<dyn DeviceController> = Arc::new(MockDeviceController::new());
        ctrl.set_power(&DeviceId::new("lamp-1"), true)
            .await
            .unwrap();
    }
}
