//! No-op mock implementation of `GuestRoomAccess` for use in tests and convenience factories.

use crate::user_data::ports::guest_room_access::GuestRoomAccess;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;

/// Grants no room to guests; for tests that don't exercise guest room scoping.
pub struct MockGuestRoomAccess;

#[async_trait]
impl GuestRoomAccess for MockGuestRoomAccess {
    async fn set_guest_accessible(&self, _room_id: &str, _accessible: bool) -> Result<()> {
        Ok(())
    }

    async fn accessible_room_ids(&self) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }

    async fn is_guest_accessible(&self, _room_id: &str) -> Result<bool> {
        Ok(false)
    }
}
