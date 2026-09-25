//! Settings port — driven port for persisting and loading GIAP configuration.

use crate::user_data::domain::settings::Settings;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;

/// Settings as flat key-value rows, one per field; missing keys take `Settings::default()`.
#[async_trait]
pub trait SettingsRepository: Send + Sync {
    /// Load all settings, applying defaults for any keys absent from the store.
    async fn get(&self) -> Result<Settings>;

    /// Persist an entire `Settings` struct (upsert every field).
    async fn update(&self, settings: &Settings) -> Result<()>;

    /// Persist only the named fields (`None` = all). Pass just the keys you changed: a whole
    /// snapshot would revert fields other writers changed after you read it.
    async fn update_fields(
        &self,
        settings: &Settings,
        _only: Option<&HashSet<String>>,
    ) -> Result<()> {
        self.update(settings).await
    }

    async fn get_key(&self, key: &str) -> Result<Option<String>>;

    /// Upsert one key's value; records no user intent (see [`SettingsRepository::mark_user_set`]).
    async fn set_key(&self, key: &str, value: String) -> Result<()>;

    /// Delete one setting row (an absent key is fine). The default errs, never faking success:
    /// the API-key move into `SecretRepository` relies on the row really being gone.
    async fn delete_key(&self, _key: &str) -> Result<()> {
        anyhow::bail!("delete_key is not implemented for this SettingsRepository")
    }

    /// Record that the user deliberately chose these keys, exempting them from `DEFAULT_ADOPTIONS`.
    /// Only marks keys that already have a row, so call it after the write.
    async fn mark_user_set(&self, _keys: &HashSet<String>) -> Result<()> {
        Ok(())
    }

    /// Whether `key` was marked by [`SettingsRepository::mark_user_set`]; unknown keys are not.
    async fn is_user_set(&self, _key: &str) -> Result<bool> {
        Ok(false)
    }
}
