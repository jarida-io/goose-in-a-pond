//! Driven port: protocol-agnostic device control (MQTT, HTTP, IR, …).

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::device::{DeviceCapability, DeviceId, DeviceState, DeviceStateValue};

/// Driven port: control a device and read back its state, whatever the transport.
#[async_trait]
pub trait DeviceController: Send + Sync {
    /// Turn a device on or off (the canonical `power` capability).
    async fn set_power(&self, device: &DeviceId, on: bool) -> Result<()>;

    /// Set a single typed state value (e.g. `brightness` → `Int(80)`).
    async fn set_state(&self, device: &DeviceId, key: &str, value: DeviceStateValue) -> Result<()>;

    /// Read a device's current full state.
    async fn query_state(&self, device: &DeviceId) -> Result<DeviceState>;

    /// The capabilities a device advertises (used to validate/negotiate control).
    async fn capabilities(&self, device: &DeviceId) -> Result<Vec<DeviceCapability>>;
}
