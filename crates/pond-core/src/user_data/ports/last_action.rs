//! What "undo" needs: each member's most recent device command, what the device was before it,
//! and what it became — so misheard voice command can be reversed and named back to the speaker.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::device_control::DeviceStatePatch;

/// One reversible command. `before`/`after` share the same typed fields as
/// [`DeviceStatePatch`][crate::user_data::ports::device_control::DeviceStatePatch] so undo can
/// feed `before` straight back to the matching `set_*` call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastDeviceAction {
    pub device_id: String,
    /// Only the fields this command changed, as they were immediately beforehand.
    pub before: DeviceStatePatch,
    /// The same fields, as the command set them.
    pub after: DeviceStatePatch,
    pub recorded_at: DateTime<Utc>,
}

/// Driven Port: at most one remembered action per member, the one "undo" would reverse.
#[async_trait::async_trait]
pub trait LastActionRepository: Send + Sync {
    /// The command to undo, if the member has one recorded.
    async fn last_for(&self, profile_id: &str) -> anyhow::Result<Option<LastDeviceAction>>;

    /// Replaces whatever was remembered for this member with `action`.
    async fn record(&self, profile_id: &str, action: LastDeviceAction) -> anyhow::Result<()>;

    /// Forgets the member's remembered action — called once undo has reversed it, so saying
    /// "undo" twice in a row the second time finds nothing to undo rather than redoing it.
    async fn clear(&self, profile_id: &str) -> anyhow::Result<()>;
}
