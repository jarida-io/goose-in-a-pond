//! Saying something aloud in the room on the pond's own initiative.

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait RoomSpeaker: Send + Sync {
    /// Say `text` in the room. `Ok(false)` when the household does not want speech now, so the
    /// caller may try again later.
    async fn say(&self, text: &str) -> Result<bool>;
}
