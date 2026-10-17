use anyhow::Result;
use async_trait::async_trait;

/// Reads a short-lived pairing code aloud. The code is never in any response, so only someone in
/// earshot of the pond can use it: that is the proof of presence.
#[async_trait]
pub trait CodeAnnouncer: Send + Sync {
    async fn announce_code(&self, digits: &str) -> Result<()>;

    /// Asked before a code is issued, so a pond with no speaker refuses without spending one.
    async fn is_available(&self) -> bool {
        true
    }
}
