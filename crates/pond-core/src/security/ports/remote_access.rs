//! Durable removal of remote networking permission for an authenticated device.
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Queue revocation before discarding Pond credentials; persist it locally even when offline.
#[async_trait]
pub trait RemoteRevocation: Send + Sync {
    /// Record an authenticated device revocation; repeated calls are idempotent.
    async fn queue(&self, device_id: &str) -> Result<()>;

    /// Every device currently enrolled for remote access, for a factory reset to revoke in bulk.
    async fn enrolled_devices(&self) -> Result<Vec<String>>;
}

/// Days a device keeps remote access off the household LAN: covers travel, not a lost phone.
pub const LAN_PRESENCE_WINDOW_DAYS: i64 = 30;

/// Proof a device is still in the household, renewed on its LAN. Independent of local approval:
/// approval gates who may change the remote identity, presence how long it stays valid.
#[async_trait]
pub trait DevicePresence: Send + Sync {
    /// Records a LAN sighting now. Must not fail the request; a lost write only costs a renewal.
    async fn seen_on_lan(&self, device_id: &str);

    /// Devices last seen on the LAN before `cutoff`; a device never seen is never returned.
    async fn absent_since(&self, cutoff: DateTime<Utc>) -> Result<Vec<String>>;

    /// When this device's remote access lapses unless it returns to the LAN.
    async fn lapses_at(&self, device_id: &str) -> Result<Option<DateTime<Utc>>>;
}

/// The tailnet machine a request arrived from, named by the Pond's own network helper. Only the
/// private tailnet listener sets it; it is never read from a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailnetMachine(pub String);

/// The machine each remotely enrolled device uses, so its token is refused from any other.
#[async_trait]
pub trait RemoteMachines: Send + Sync {
    /// The machine key enrolled for `device_id`; `None` when it holds no active enrollment.
    async fn machine_for(&self, device_id: &str) -> Result<Option<String>>;
}
