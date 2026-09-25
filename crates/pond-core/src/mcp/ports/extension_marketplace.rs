use anyhow::Result;
use async_trait::async_trait;

use crate::mcp::domain::marketplace::MarketplaceExtension;

/// Read-only catalogue of curated MCP extensions available for installation.
#[async_trait]
pub trait ExtensionMarketplace: Send + Sync {
    async fn list_available(&self) -> Result<Vec<MarketplaceExtension>>;

    async fn get_by_id(&self, id: &str) -> Result<Option<MarketplaceExtension>>;
}
