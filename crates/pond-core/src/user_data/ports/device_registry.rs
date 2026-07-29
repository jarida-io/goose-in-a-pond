//! Driven Port: Device Registry
//!
//! Manages connected devices — mobile phones (GOTG), smart home devices,
//! other pond instances, etc.
//!
//! # TODO
//! - [ ] Add device status (online/offline/last_seen)
//! - [ ] Add device capabilities discovery

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
    /// Optional room grouping for hub UI ("Living Room", "Kitchen", …).
    /// `None` means the device shows under the default "Home" room.
    #[serde(default)]
    pub room: Option<String>,
}

/// Editable fields from the Devices "Configure" UI (name, hostname, room).
/// Unlike `rename` (name-only, used by Matter commissioning), this covers
/// everything the manual-registration form itself collects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateDeviceRequest {
    pub name: String,
    pub hostname: Option<String>,
    pub room: Option<String>,
}

/// Request to register a new device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterDeviceRequest {
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
<<<<<<< Updated upstream
=======

    /// Set a device's display name. Used when a Matter device is commissioned
    /// with a user-chosen name (the bridge may have registered it first under a
    /// fallback name), so the registry name — which chat resolution matches on —
    /// reflects what the user typed. Default errors so the many test doubles need
    /// no body; real registries override it.
    async fn rename(&self, device_id: &str, _name: &str) -> Result<()> {
        anyhow::bail!("this registry does not support renaming device '{device_id}'")
    }

    /// Force a device to read as offline until its next heartbeat/turn-on.
    /// Devices without a real liveness signal (host, sensor, manually
    /// registered types, …) have no other way to go offline — `is_online` is
    /// otherwise derived purely from `last_seen` recency. Default errors so
    /// the many test doubles need no body; real registries override it.
    async fn set_offline(&self, device_id: &str) -> Result<()> {
        anyhow::bail!("this registry does not support setting device '{device_id}' offline")
    }

    /// Update editable fields (name, hostname, room) and return the updated
    /// device. Default errors so the many test doubles need no body; real
    /// registries override it.
    async fn update(&self, device_id: &str, _request: UpdateDeviceRequest) -> Result<Device> {
        anyhow::bail!("this registry does not support updating device '{device_id}'")
    }
>>>>>>> Stashed changes
}
