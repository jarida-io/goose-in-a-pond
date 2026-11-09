//! Speech outside a turn: queued for the speaker behind any turn, never over one.

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Announce: Send + Sync {
    /// Returns once the text has been said, or failed to be.
    async fn announce(&self, text: &str) -> Result<()>;
}
