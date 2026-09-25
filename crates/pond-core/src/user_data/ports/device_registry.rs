//! Driven port: registry of connected devices (phones, smart-home devices, other ponds).
//! TODO: device status (online/offline/last_seen).
//! TODO: device capabilities discovery.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A registered device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// "gotg", "smart_speaker", "sensor", "pond", etc.
    pub device_type: String,
    pub hostname: Option<String>,
    pub ip_address: Option<String>,
    pub capabilities: Vec<String>,
    pub registered_at: String,
    pub last_seen: Option<String>,
    pub is_online: bool,
    /// Hub UI room ("Kitchen", …); `None` shows under the default "Home" room.
    #[serde(default)]
    pub room: Option<String>,
}

/// Editable fields from the Devices "Configure" UI (name, hostname, room).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateDeviceRequest {
    pub name: String,
    pub hostname: Option<String>,
    pub room: Option<String>,
}

/// Request to register a new device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterDeviceRequest {
    /// Stable caller-supplied id (e.g. `"matter-3"`) so re-syncs hit one row; `None` = new UUID.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub device_type: String,
    pub hostname: Option<String>,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub room: Option<String>,
}

/// Driven Port: device lifecycle management.
#[async_trait]
pub trait DeviceRegistry: Send + Sync {
    /// Register a new device with the pond.
    async fn register(&self, request: RegisterDeviceRequest) -> Result<Device>;

    /// List all registered devices.
    async fn list_devices(&self) -> Result<Vec<Device>>;

    /// Get a specific device by ID.
    async fn get_device(&self, device_id: &str) -> Result<Option<Device>>;

    /// Remove a device registration.
    async fn unregister(&self, device_id: &str) -> Result<()>;

    /// Update device last-seen / online status.
    async fn heartbeat(&self, device_id: &str) -> Result<()>;

    /// Set a device's display name, which chat resolution matches on.
    async fn rename(&self, device_id: &str, _name: &str) -> Result<()> {
        anyhow::bail!("this registry does not support renaming device '{device_id}'")
    }

    /// Force a device to read as offline until its next heartbeat/turn-on.
    async fn set_offline(&self, device_id: &str) -> Result<()> {
        anyhow::bail!("this registry does not support setting device '{device_id}' offline")
    }

    /// Update editable fields (name, hostname, room) and return the updated device.
    async fn update(&self, device_id: &str, _request: UpdateDeviceRequest) -> Result<Device> {
        anyhow::bail!("this registry does not support updating device '{device_id}'")
    }

    /// Replace the bridge-derived device type and capabilities; never user fields (`update`).
    /// Defaults to a no-op, not an error: every sync calls it.
    async fn set_discovered_profile(
        &self,
        _device_id: &str,
        _device_type: &str,
        _capabilities: &[String],
    ) -> Result<()> {
        Ok(())
    }
}
