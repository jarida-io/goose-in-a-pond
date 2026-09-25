//! Escape hatch that keeps tool narrowing safe: `giap-toolkit` lets the model load any group.

use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolSelectionError {
    #[error("unknown tool group '{0}'")]
    UnknownGroup(String),
    #[error("tool group '{0}' is not available on this device")]
    GroupNotRegistered(String),
    #[error("tool selection is not active for this session")]
    NotActive,
    /// Recorded, but the tool cache was cold, so the tools arrive next turn. Not a silent `Ok`: a
    /// model told they are available calls one now and wastes a turn on the guard's refusal.
    #[error("tool group '{0}' is loaded but its tools arrive on the next turn")]
    NotReady(String),
    #[error("tool selection failed: {0}")]
    Internal(String),
}

/// One row of the catalog as the model sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolGroupStatus {
    pub extension: String,
    pub description: String,
    /// Tools are in the prompt right now.
    pub loaded: bool,
    /// Always loaded, cannot be turned off.
    pub core: bool,
    /// How many tools the group contributes, when known.
    pub tool_count: usize,
}

/// Inspect and widen a session's tool groups. `engine_session_id` is goose's `agent-session-id`
/// from the request `_meta`, not GIAP's: `current_session_id()` races concurrent streams.
#[async_trait]
pub trait ToolSelectionControl: Send + Sync {
    /// Every registered group with its loaded/dormant status for this session.
    async fn group_status(&self, engine_session_id: &str) -> Vec<ToolGroupStatus>;

    /// Load `group` for this session (idempotent); returns all its groups. Takes effect from the
    /// next provider call, even mid-turn, and costs one prompt-prefix rebuild.
    async fn enable_group(
        &self,
        engine_session_id: &str,
        group: &str,
    ) -> Result<Vec<String>, ToolSelectionError>;
}
