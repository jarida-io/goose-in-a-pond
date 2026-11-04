use async_trait::async_trait;

use crate::models::domain::room_voice::{RoomEvent, RoomStatus};

/// The room's voice loop, run by serve: its status, the switch, and its events for clients.
#[async_trait]
pub trait RoomVoiceControl: Send + Sync {
    fn status(&self) -> RoomStatus;

    /// Start or stop the room now; persisting `voice_room_enabled` is the caller's.
    async fn set_enabled(&self, enabled: bool) -> RoomStatus;

    /// End the turn in flight, as the shell's interrupt button does; nothing while idle.
    fn interrupt(&self);

    /// Every event the room emits from now on: the loop's, `audio_level` included, and its pauses.
    fn subscribe(&self) -> tokio::sync::broadcast::Receiver<RoomEvent>;

    /// Voice prints or wake calibration changed: the ear learns the household again.
    fn household_changed(&self) {}

    /// Hear no wake word and start no turn for `span`, resuming on its own; never persisted. A
    /// longer pause already running is kept.
    fn pause(&self, _span: std::time::Duration) {}

    /// End a pause now.
    fn resume(&self) {}
}
