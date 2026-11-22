//! Settings-repository decorator that records every change: a `giap::trace` line (persisted to
//! `pond_logs.db`) and an activity event, per key whose value actually moved. Wraps the one
//! `settings_repo` in `run_server`, so whichever handler, tool or background task writes a
//! setting, the change is logged without that writer doing anything.
//!
//! Each write reads the previous values first. The read and the write are not one transaction,
//! so two racing writers may each report the other's value as `from`; the log is a debugging aid,
//! not a ledger. Values are classified by [`Disclosure`]: credentials and household-personal
//! values are recorded as changed, never with their values.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::security::ports::event_log::EventLog;
use crate::user_data::domain::config_change::{diff_settings, ConfigChange, ConfigChangeKind};
use crate::user_data::domain::settings::Settings;
use crate::user_data::ports::settings::SettingsRepository;

/// Emit the `giap::trace` line for `change` and append its activity event. A failed append is a
/// warning: the setting itself was written, and the trace line already recorded the change.
pub async fn record_config_change(event_log: Option<&dyn EventLog>, change: &ConfigChange) {
    match change.loggable_values() {
        Some((from, to)) => tracing::info!(
            target: "giap::trace",
            kind = change.kind.trace_kind(),
            key = %change.key,
            from = from.as_deref().unwrap_or("<unset>"),
            to = to.as_deref().unwrap_or("<unset>"),
            "[config] {} changed",
            change.key
        ),
        None => tracing::info!(
            target: "giap::trace",
            kind = change.kind.trace_kind(),
            key = %change.key,
            redacted = true,
            "[config] {} changed (value withheld)",
            change.key
        ),
    }
    if let Some(log) = event_log {
        if let Err(e) = log.append(change.to_event()).await {
            tracing::warn!(
                error = %e,
                key = %change.key,
                kind = change.kind.trace_kind(),
                "failed to record a configuration change in the activity log"
            );
        }
    }
}

/// Records every settings change; see the module docs. Forwards every method explicitly.
pub struct AuditedSettingsRepository {
    inner: Arc<dyn SettingsRepository + Send + Sync>,
    event_log: Arc<dyn EventLog>,
}

impl AuditedSettingsRepository {
    /// Wrap `inner`, recording its changes to `event_log` and the `giap::trace` target.
    pub fn new(
        inner: Arc<dyn SettingsRepository + Send + Sync>,
        event_log: Arc<dyn EventLog>,
    ) -> Self {
        Self { inner, event_log }
    }

    async fn record(&self, change: &ConfigChange) {
        record_config_change(Some(self.event_log.as_ref()), change).await;
    }

    /// The current settings as JSON, or `None` (with a warning) when they cannot be read.
    async fn snapshot(&self) -> Option<serde_json::Value> {
        let settings = match self.inner.get().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "settings audit: could not read previous values");
                return None;
            }
        };
        match serde_json::to_value(&settings) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(error = %e, "settings audit: could not serialize previous values");
                None
            }
        }
    }

    async fn record_snapshot_write(
        &self,
        before: Option<serde_json::Value>,
        written: &Settings,
        only: Option<&HashSet<String>>,
    ) {
        let after = match serde_json::to_value(written) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "settings audit: could not serialize written values");
                return;
            }
        };
        let Some(before) = before else {
            tracing::warn!(
                keys = only.map_or(0, HashSet::len),
                "settings written, but the changes are not itemised: previous values unreadable"
            );
            return;
        };
        for change in diff_settings(&before, &after, only) {
            self.record(&change).await;
        }
    }

    /// The stored value of `key`, or `None` (with a warning) when it cannot be read.
    async fn previous(&self, key: &str) -> Option<String> {
        match self.inner.get_key(key).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, key, "settings audit: could not read previous value");
                None
            }
        }
    }
}

#[async_trait]
impl SettingsRepository for AuditedSettingsRepository {
    async fn get(&self) -> Result<Settings> {
        self.inner.get().await
    }

    async fn update(&self, settings: &Settings) -> Result<()> {
        let before = self.snapshot().await;
        self.inner.update(settings).await?;
        self.record_snapshot_write(before, settings, None).await;
        Ok(())
    }

    async fn update_fields(
        &self,
        settings: &Settings,
        only: Option<&HashSet<String>>,
    ) -> Result<()> {
        let before = self.snapshot().await;
        self.inner.update_fields(settings, only).await?;
        self.record_snapshot_write(before, settings, only).await;
        Ok(())
    }

    async fn get_key(&self, key: &str) -> Result<Option<String>> {
        self.inner.get_key(key).await
    }

    async fn set_key(&self, key: &str, value: String) -> Result<()> {
        let before = self.previous(key).await;
        let changed = before.as_deref() != Some(value.as_str());
        let after = changed.then(|| value.clone());
        self.inner.set_key(key, value).await?;
        if changed {
            self.record(&ConfigChange::new(
                ConfigChangeKind::Setting,
                key,
                before,
                after,
            ))
            .await;
        }
        Ok(())
    }

    async fn delete_key(&self, key: &str) -> Result<()> {
        let before = self.previous(key).await;
        self.inner.delete_key(key).await?;
        if before.is_some() {
            self.record(&ConfigChange::new(
                ConfigChangeKind::Setting,
                key,
                before,
                None,
            ))
            .await;
        }
        Ok(())
    }

    async fn mark_user_set(&self, keys: &HashSet<String>) -> Result<()> {
        self.inner.mark_user_set(keys).await
    }

    async fn is_user_set(&self, key: &str) -> Result<bool> {
        self.inner.is_user_set(key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::domain::event::{AttributeValue, PrivacySensitivity};
    use crate::security::mocks::mock_event_log::MockEventLog;
    use crate::user_data::mocks::mock_settings::MockSettingsRepository;

    fn audited() -> (AuditedSettingsRepository, Arc<MockEventLog>) {
        let log = Arc::new(MockEventLog::new());
        let repo =
            AuditedSettingsRepository::new(Arc::new(MockSettingsRepository::new()), log.clone());
        (repo, log)
    }

    fn text(value: &str) -> Option<AttributeValue> {
        Some(AttributeValue::Text(value.to_string()))
    }

    #[tokio::test]
    async fn a_changed_key_is_logged_with_its_values() {
        let (repo, log) = audited();
        repo.set_key("assistant_name", "Goose".into())
            .await
            .unwrap();
        repo.set_key("assistant_name", "Pond".into()).await.unwrap();

        let events = log.all();
        assert_eq!(events.len(), 2);
        let last = &events[1];
        assert_eq!(last.action, "settings.changed");
        assert_eq!(last.attributes.get("key").cloned(), text("assistant_name"));
        assert_eq!(last.attributes.get("from").cloned(), text("Goose"));
        assert_eq!(last.attributes.get("to").cloned(), text("Pond"));
    }

    #[tokio::test]
    async fn an_unchanged_key_is_not_logged() {
        let (repo, log) = audited();
        repo.set_key("timezone", "UTC".into()).await.unwrap();
        repo.set_key("timezone", "UTC".into()).await.unwrap();
        assert_eq!(log.all().len(), 1);

        // A whole snapshot equal to what is stored changes nothing.
        let current = repo.get().await.unwrap();
        repo.update(&current).await.unwrap();
        assert_eq!(log.all().len(), 1);
    }

    #[tokio::test]
    async fn a_snapshot_write_logs_only_the_moved_keys_in_the_patch() {
        let (repo, log) = audited();
        let mut s = repo.get().await.unwrap();
        s.llm_max_tokens = 1024;
        s.mic_enabled = !s.mic_enabled;
        let only: HashSet<String> = ["llm_max_tokens".to_string()].into_iter().collect();
        repo.update_fields(&s, Some(&only)).await.unwrap();

        let events = log.all();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].attributes.get("key").cloned(),
            text("llm_max_tokens")
        );
        assert_eq!(events[0].attributes.get("to").cloned(), text("1024"));
    }

    #[tokio::test]
    async fn a_secret_is_redacted() {
        let (repo, log) = audited();
        repo.set_key("api_key_gnews", "not-a-real-key".into())
            .await
            .unwrap();
        repo.delete_key("api_key_gnews").await.unwrap();

        let events = log.all();
        assert_eq!(events.len(), 2);
        for event in events {
            assert_eq!(
                event.attributes.get("redacted"),
                Some(&AttributeValue::Bool(true))
            );
            assert!(!event.attributes.contains_key("from"));
            assert!(!event.attributes.contains_key("to"));
        }
    }

    #[tokio::test]
    async fn a_personal_value_is_withheld() {
        let (repo, log) = audited();
        let mut s = repo.get().await.unwrap();
        s.user_name = "Arlene".into();
        repo.update(&s).await.unwrap();

        let events = log.all();
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.attributes.get("key").cloned(), text("user_name"));
        assert_eq!(
            event.attributes.get("redacted"),
            Some(&AttributeValue::Bool(true))
        );
        assert_eq!(event.privacy_sensitivity, PrivacySensitivity::Sensitive);
        assert!(!event.attributes.contains_key("to"));
    }

    #[tokio::test]
    async fn deleting_an_absent_key_logs_nothing() {
        let (repo, log) = audited();
        repo.delete_key("never_written").await.unwrap();
        assert!(log.all().is_empty());
    }
}
