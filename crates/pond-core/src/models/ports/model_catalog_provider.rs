use anyhow::Result;
use async_trait::async_trait;

use crate::models::domain::model_record::{BinaryRecord, ModelRecord};

/// Fetch a model + binary catalog from upstream sources.
#[async_trait]
pub trait ModelCatalogProvider: Send + Sync {
    /// Must be idempotent; `models` are ready for `ModelRepository::upsert()`.
    async fn fetch(&self) -> Result<(Vec<ModelRecord>, Vec<BinaryRecord>)>;
}
