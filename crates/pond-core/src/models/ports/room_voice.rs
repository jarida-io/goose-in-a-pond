use async_trait::async_trait;

use crate::models::domain::room_voice::RoomStatus;
use crate::shared::domain::agent::WorkflowEvent;

/// The room's voice loop, run by serve: its status, the switch, and its events for clients.
#[async_trait]
pub trait RoomVoiceControl: Send + Sync {
    fn status(&self) -> RoomStatus;

    /// Start or stop the room now; persisting `voice_room_enabled` is the caller's.
    async fn set_enabled(&self, enabled: bool) -> RoomStatus;

    /// End the turn in flight, as the shell's interrupt button does; nothing while idle.
    fn interrupt(&self);

    /// Every workflow event the room's loop emits from now on, `audio_level` included.
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<WorkflowEvent>;
}
