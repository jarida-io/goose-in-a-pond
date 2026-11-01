//! Driven port: which rooms a guest (`ProfileScope::Guest`) may use devices in.
//!
//! `ProfileScope::Guest` carries no per-guest identity (it is recomputed fresh each turn for an
//! unidentified speaker), so access is a household-wide list rather than per-guest grants:
//! presence in this set means "guests may control devices placed in this room."

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;

#[async_trait]
pub trait GuestRoomAccess: Send + Sync {
    /// Grant or revoke guest access to a room.
    async fn set_guest_accessible(&self, room_id: &str, accessible: bool) -> Result<()>;

    /// Every room id guests may currently use devices in.
    async fn accessible_room_ids(&self) -> Result<HashSet<String>>;

    /// Whether guests may currently use devices in this room.
    async fn is_guest_accessible(&self, room_id: &str) -> Result<bool>;
}
