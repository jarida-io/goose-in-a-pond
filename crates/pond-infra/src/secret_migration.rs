//! One-time move of `api_key_*` values off the `settings` table into the `SecretRepository`.
//!
//! Write the secret, prove it landed in the file store, and only then delete the settings row,
//! so a configured key is never stranded.

use anyhow::Result;
use pond_core::security::ports::secret::SecretRepository;
use pond_core::user_data::ports::settings::SettingsRepository;
use std::sync::Arc;

/// `(old settings key, secret name)`. Names use env-var spelling, since `get` reads env first.
/// `api_key_coingecko` has no reader but is kept so a pasted key is not lost.
pub const MIGRATED_API_KEYS: &[(&str, &str)] = &[
    ("api_key_guardian", "GUARDIAN_API_KEY"),
    ("api_key_gnews", "GNEWS_API_KEY"),
    ("api_key_finnhub", "FINNHUB_API_KEY"),
    ("api_key_coingecko", "COINGECKO_API_KEY"),
];

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SecretMigrationReport {
    /// Rows whose value was copied into the secret store this run.
    pub moved: Vec<String>,
    /// Rows dropped because the secret store already held that key.
    pub already_present: Vec<String>,
    /// Rows left in place because the copy could not be proved.
    pub left_in_place: Vec<String>,
}

/// Move every stored `api_key_*` row into `secret_repo`. Idempotent.
pub async fn migrate_api_keys_to_secret_repository(
    settings_repo: &Arc<dyn SettingsRepository + Send + Sync>,
    secret_repo: &Arc<dyn SecretRepository + Send + Sync>,
) -> Result<SecretMigrationReport> {
    let mut report = SecretMigrationReport::default();

    for (settings_key, secret_key) in MIGRATED_API_KEYS {
        let stored = match settings_repo.get_key(settings_key).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    key = settings_key,
                    error = %e,
                    "secret migration: could not read the settings row; leaving it alone"
                );
                report.left_in_place.push((*settings_key).to_string());
                continue;
            }
        };
        let Some(value) = stored else { continue };

        if value.trim().is_empty() {
            // Empty means no key configured (`None` was written as ""); nothing to preserve.
            if settings_repo.delete_key(settings_key).await.is_err() {
                report.left_in_place.push((*settings_key).to_string());
            }
            continue;
        }

        // Not `has()`: a same-named env var would pass for a copy and we'd drop an uncopied row.
        let already = secret_repo
            .list_keys()
            .await
            .unwrap_or_default()
            .iter()
            .any(|k| k == secret_key);

        if !already {
            if let Err(e) = secret_repo.set(secret_key, value.trim()).await {
                tracing::warn!(
                    key = settings_key,
                    error = %e,
                    "secret migration: could not store the key; leaving the settings row in place"
                );
                report.left_in_place.push((*settings_key).to_string());
                continue;
            }
        }

        // Prove it landed before destroying the only other copy.
        let landed = secret_repo
            .list_keys()
            .await
            .unwrap_or_default()
            .iter()
            .any(|k| k == secret_key);
        if !landed {
            tracing::warn!(
                key = settings_key,
                "secret migration: the secret store does not report the key after writing it; \
                 leaving the settings row in place"
            );
            report.left_in_place.push((*settings_key).to_string());
            continue;
        }

        if let Err(e) = settings_repo.delete_key(settings_key).await {
            tracing::warn!(
                key = settings_key,
                error = %e,
                "secret migration: key copied but the settings row could not be deleted"
            );
            report.left_in_place.push((*settings_key).to_string());
            continue;
        }

        if already {
            // The secret store wins: it is where the user edits keys, so the row is stale.
            report.already_present.push((*settings_key).to_string());
        } else {
            report.moved.push((*settings_key).to_string());
        }
    }

    if !report.moved.is_empty() {
        tracing::info!(
            keys = ?report.moved,
            "moved API keys off the settings table into the secret repository (PAI-2 P2)"
        );
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_secret_repository::FileSecretRepository;
    use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;

    fn settings() -> Arc<dyn SettingsRepository + Send + Sync> {
        Arc::new(MockSettingsRepository::new())
    }

    #[tokio::test]
    async fn stored_api_keys_move_into_the_secret_store_and_leave_the_settings_table() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = settings();
        settings
            .set_key("api_key_guardian", "guardian-live-key".into())
            .await
            .unwrap();
        settings
            .set_key("api_key_gnews", "  gnews-live-key  ".into())
            .await
            .unwrap();
        // The old upsert wrote "" for a `None` field; that is not a key.
        settings
            .set_key("api_key_finnhub", String::new())
            .await
            .unwrap();

        let secrets: Arc<dyn SecretRepository + Send + Sync> =
            Arc::new(FileSecretRepository::new(tmp.path()).unwrap());

        let report = migrate_api_keys_to_secret_repository(&settings, &secrets)
            .await
            .unwrap();

        // Positive case first: "row gone" alone would pass if the key were deleted uncopied.
        assert_eq!(
            secrets.get("GUARDIAN_API_KEY").await.unwrap().as_deref(),
            Some("guardian-live-key")
        );
        assert_eq!(
            secrets.get("GNEWS_API_KEY").await.unwrap().as_deref(),
            Some("gnews-live-key"),
            "surrounding whitespace must be trimmed, as the old readers did"
        );

        assert!(settings
            .get_key("api_key_guardian")
            .await
            .unwrap()
            .is_none());
        assert!(settings.get_key("api_key_gnews").await.unwrap().is_none());
        assert!(settings.get_key("api_key_finnhub").await.unwrap().is_none());

        assert_eq!(
            report.moved,
            vec!["api_key_guardian".to_string(), "api_key_gnews".to_string()]
        );
        assert!(report.left_in_place.is_empty());

        // Idempotent: a second start finds nothing and changes nothing.
        let again = migrate_api_keys_to_secret_repository(&settings, &secrets)
            .await
            .unwrap();
        assert_eq!(again, SecretMigrationReport::default());
        assert_eq!(
            secrets.get("GUARDIAN_API_KEY").await.unwrap().as_deref(),
            Some("guardian-live-key")
        );
    }

    struct FailingSecrets;

    #[async_trait::async_trait]
    impl SecretRepository for FailingSecrets {
        async fn get(&self, _: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn set(&self, _: &str, _: &str) -> Result<()> {
            anyhow::bail!("secret store unavailable")
        }
        async fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn list_keys(&self) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn has(&self, _: &str) -> Result<bool> {
            Ok(false)
        }
    }

    #[tokio::test]
    async fn a_failed_copy_leaves_the_users_key_where_it_was() {
        let settings = settings();
        settings
            .set_key("api_key_guardian", "guardian-live-key".into())
            .await
            .unwrap();
        let secrets: Arc<dyn SecretRepository + Send + Sync> = Arc::new(FailingSecrets);

        let report = migrate_api_keys_to_secret_repository(&settings, &secrets)
            .await
            .unwrap();

        assert_eq!(report.left_in_place, vec!["api_key_guardian".to_string()]);
        assert!(report.moved.is_empty());
        assert_eq!(
            settings
                .get_key("api_key_guardian")
                .await
                .unwrap()
                .as_deref(),
            Some("guardian-live-key"),
            "a migration that strands a configured key is worse than the exposure it closes"
        );
    }

    /// Reports `has() == true` for unstored keys, as a same-named env var would.
    #[derive(Default)]
    struct EnvShadowSecrets {
        stored: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[async_trait::async_trait]
    impl SecretRepository for EnvShadowSecrets {
        async fn get(&self, k: &str) -> Result<Option<String>> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone()))
        }
        async fn set(&self, k: &str, v: &str) -> Result<()> {
            self.stored
                .lock()
                .unwrap()
                .push((k.to_string(), v.to_string()));
            Ok(())
        }
        async fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn list_keys(&self) -> Result<Vec<String>> {
            Ok(self
                .stored
                .lock()
                .unwrap()
                .iter()
                .map(|(n, _)| n.clone())
                .collect())
        }
        async fn has(&self, _: &str) -> Result<bool> {
            Ok(true)
        }
    }

    #[tokio::test]
    async fn an_env_var_is_not_evidence_the_key_was_copied() {
        let settings = settings();
        settings
            .set_key("api_key_gnews", "gnews-live-key".into())
            .await
            .unwrap();
        let secrets: Arc<dyn SecretRepository + Send + Sync> =
            Arc::new(EnvShadowSecrets::default());

        let report = migrate_api_keys_to_secret_repository(&settings, &secrets)
            .await
            .unwrap();

        assert_eq!(
            report.moved,
            vec!["api_key_gnews".to_string()],
            "the value must be copied even though has() says the name already resolves"
        );
        assert_eq!(
            secrets.get("GNEWS_API_KEY").await.unwrap().as_deref(),
            Some("gnews-live-key")
        );
        assert!(settings.get_key("api_key_gnews").await.unwrap().is_none());
    }
}
