//! Driven port: which devices an invited, identified guest (a `Profile` with `Role::Guest`)
//! may control.
//!
//! Distinct from [`super::guest_room_access::GuestRoomAccess`], which is household-wide because
//! an unidentified voice (`ProfileScope::Guest`) carries no identity to hang a per-guest grant
//! off. A `Role::Guest` profile IS identified, so access here is precise: per-guest, per-device.
//! A guest starts with no devices; `household_permission::may_control_device` also checks the
//! guest's `guest_until` expiry, which this port doesn't know about (that's on `Profile`).

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;

#[async_trait]
pub trait GuestDeviceAccess: Send + Sync {
    /// Grant a guest profile access to a device.
    async fn allow(&self, profile_id: &str, device_id: &str) -> Result<()>;

    /// Revoke a guest profile's access to a device.
    async fn revoke(&self, profile_id: &str, device_id: &str) -> Result<()>;

    /// Every device id this guest profile may currently control.
    async fn allowed_devices(&self, profile_id: &str) -> Result<HashSet<String>>;
}
