//! No-op mock implementation of `GuestDeviceAccess` for use in tests and convenience factories.

use crate::user_data::ports::guest_device_access::GuestDeviceAccess;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;

/// Grants no device to any guest; for tests that don't exercise per-guest device scoping.
pub struct MockGuestDeviceAccess;

#[async_trait]
impl GuestDeviceAccess for MockGuestDeviceAccess {
    async fn allow(&self, _profile_id: &str, _device_id: &str) -> Result<()> {
        Ok(())
    }

    async fn revoke(&self, _profile_id: &str, _device_id: &str) -> Result<()> {
        Ok(())
    }

    async fn allowed_devices(&self, _profile_id: &str) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }
}
