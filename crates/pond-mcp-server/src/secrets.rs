//! Process-global secret store for MCP tools that need third-party API keys. Exactly one
//! instance per process: `FileSecretRepository` caches `secrets.json` and rewrites it whole.

use pond_core::security::ports::secret::SecretRepository;
use std::sync::{Arc, OnceLock};

static SECRET_REPO: OnceLock<Arc<dyn SecretRepository + Send + Sync>> = OnceLock::new();

/// Install the secret store. Call once at startup, before any chat session.
pub fn init_secret_deps(repo: Arc<dyn SecretRepository + Send + Sync>) {
    let _ = SECRET_REPO.set(repo);
}

/// Read one secret; `None` if unset, unreadable or never installed (the CLI paths), so
/// every failure degrades a tool to its keyless fallback.
pub async fn secret(key: &str) -> Option<String> {
    let repo = SECRET_REPO.get()?;
    match repo.get(key).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(key, error = %e, "secret store read failed; treating the key as unset");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    struct OneKey;

    #[async_trait::async_trait]
    impl SecretRepository for OneKey {
        async fn get(&self, key: &str) -> Result<Option<String>> {
            Ok((key == "GUARDIAN_API_KEY").then(|| "guardian-live-key".to_string()))
        }
        async fn set(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn list_keys(&self) -> Result<Vec<String>> {
            Ok(vec!["GUARDIAN_API_KEY".to_string()])
        }
        async fn has(&self, key: &str) -> Result<bool> {
            Ok(key == "GUARDIAN_API_KEY")
        }
    }

    /// A single test because `SECRET_REPO` is a process-global `OnceLock`; no other test in
    /// the crate may call `init_secret_deps`.
    #[tokio::test]
    async fn an_uninstalled_store_reads_as_unset_and_an_installed_one_reads_through() {
        assert!(
            secret("GUARDIAN_API_KEY").await.is_none(),
            "with no store installed a tool must see no key, so it takes its keyless fallback"
        );

        init_secret_deps(Arc::new(OneKey));

        assert_eq!(
            secret("GUARDIAN_API_KEY").await.as_deref(),
            Some("guardian-live-key")
        );
        assert!(secret("GNEWS_API_KEY").await.is_none());
    }
}
