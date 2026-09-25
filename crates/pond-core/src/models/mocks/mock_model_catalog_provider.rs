use anyhow::Result;
use async_trait::async_trait;

use crate::models::domain::model_record::{BinaryRecord, ModelRecord};
use crate::models::ports::model_catalog_provider::ModelCatalogProvider;

/// Returns a fixed set of models and binaries, or always fails.
pub struct MockModelCatalogProvider {
    models: Vec<ModelRecord>,
    binaries: Vec<BinaryRecord>,
    fail: bool,
}

impl MockModelCatalogProvider {
    /// Provider that returns the given model list (no binaries).
    pub fn with_models(models: Vec<ModelRecord>) -> Self {
        Self {
            models,
            binaries: vec![],
            fail: false,
        }
    }

    pub fn failing() -> Self {
        Self {
            models: vec![],
            binaries: vec![],
            fail: true,
        }
    }
}

impl Default for MockModelCatalogProvider {
    fn default() -> Self {
        Self::with_models(vec![])
    }
}

#[async_trait]
impl ModelCatalogProvider for MockModelCatalogProvider {
    async fn fetch(&self) -> Result<(Vec<ModelRecord>, Vec<BinaryRecord>)> {
        if self.fail {
            anyhow::bail!("MockModelCatalogProvider: forced failure");
        }
        Ok((self.models.clone(), self.binaries.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_configured_models() {
        let provider = MockModelCatalogProvider::with_models(vec![]);
        let (models, binaries) = provider.fetch().await.unwrap();
        assert!(models.is_empty());
        assert!(binaries.is_empty());
    }

    #[tokio::test]
    async fn failing_provider_returns_error() {
        let provider = MockModelCatalogProvider::failing();
        let result = provider.fetch().await;
        assert!(result.is_err());
    }
}
