//! Place -> coordinates. Split as a privacy boundary: [`NetworkPlaceLookup`] tells a third
//! party the household's IP, so it must be passed in on purpose, never folded into lookups.

use anyhow::Result;
use async_trait::async_trait;

/// A place, fixed to a point on the earth.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaceFix {
    /// Readable name, e.g. `Kisumu, Kenya`.
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    /// The zone the source believes this point is in, when it says.
    pub timezone: Option<String>,
}

/// Name → coordinates. Reveals the query, not the asker.
#[async_trait]
pub trait PlaceLookup: Send + Sync {
    async fn by_name(&self, query: &str) -> Result<PlaceFix>;
}

/// This connection → coordinates. Reveals the asker, so never wired by default.
#[async_trait]
pub trait NetworkPlaceLookup: Send + Sync {
    async fn by_network(&self) -> Result<PlaceFix>;
}
