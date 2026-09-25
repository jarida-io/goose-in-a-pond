use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

#[async_trait]
pub trait ModelDownloader: Send + Sync {
    /// No-op if `dest` exists; its parent must exist. `size_hint_mb` only drives progress (0 ok).
    async fn download(&self, url: &str, dest: &Path, size_hint_mb: u64) -> Result<()>;
}
