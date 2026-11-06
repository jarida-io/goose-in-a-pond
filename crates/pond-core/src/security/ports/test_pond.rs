use anyhow::Result;
use async_trait::async_trait;

/// The host's mark that this is a Jarida test Pond. Only the host sets it (`pond-server
/// test-pond`); no route writes it, so nothing a phone sends turns a home Pond into a test Pond.
#[async_trait]
pub trait TestPondMark: Send + Sync {
    async fn is_test_pond(&self) -> Result<bool>;
}
