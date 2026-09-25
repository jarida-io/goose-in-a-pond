//! In-memory mock implementation of `SettingsRepository` for tests.

use crate::user_data::domain::settings::Settings;
use crate::user_data::ports::settings::SettingsRepository;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

/// In-memory settings store. Starts empty (all reads return defaults).
pub struct MockSettingsRepository {
    store: Arc<RwLock<HashMap<String, String>>>,
    /// Keys the user chose, tracked apart from values so a snapshot write implies no intent.
    user_set: Arc<RwLock<HashSet<String>>>,
}

impl MockSettingsRepository {
    pub fn new() -> Self {
        Self {
            store: Arc::new(RwLock::new(HashMap::new())),
            user_set: Arc::new(RwLock::new(HashSet::new())),
        }
    }
}

impl Default for MockSettingsRepository {
    fn default() -> Self {
        Self::new()
    }
}

/// Overlay stored string values onto a default Settings struct.
fn build_settings(store: &HashMap<String, String>) -> Settings {
    let mut s = Settings::default();
    if let Some(v) = store.get("assistant_name") {
        s.assistant_name = v.clone();
    }
    if let Some(v) = store.get("assistant_personality") {
        s.assistant_personality = v.clone();
    }
    if let Some(v) = store.get("user_name") {
        s.user_name = v.clone();
    }
    if let Some(v) = store.get("timezone") {
        s.timezone = v.clone();
    }
    if let Some(v) = store.get("llm_max_tokens") {
        if let Ok(n) = v.parse() {
            s.llm_max_tokens = n;
        }
    }
    if let Some(v) = store.get("mesh_settlement_millisats_per_token") {
        if let Ok(n) = v.parse() {
            s.mesh_settlement_millisats_per_token = n;
        }
    }
    if let Some(v) = store.get("mesh_lend_token_ceiling") {
        if let Ok(n) = v.parse() {
            s.mesh_lend_token_ceiling = n;
        }
    }
    if let Some(v) = store.get("llm_temperature") {
        if let Ok(n) = v.parse() {
            s.llm_temperature = n;
        }
    }
    if let Some(v) = store.get("llm_provider") {
        s.llm_provider = v.clone();
    }
    // Privacy toggle: must round-trip or mic-off tests pass for the wrong reason.
    if let Some(v) = store.get("mic_enabled") {
        if let Ok(b) = v.parse() {
            s.mic_enabled = b;
        }
    }
    // Both default to true: dropping them would let "compaction off" tests pass vacuously.
    if let Some(v) = store.get("hybrid_compaction_enabled") {
        if let Ok(b) = v.parse() {
            s.hybrid_compaction_enabled = b;
        }
    }
    if let Some(v) = store.get("context_monitor_enabled") {
        if let Ok(b) = v.parse() {
            s.context_monitor_enabled = b;
        }
    }
    if let Some(v) = store.get("voice_wake_word") {
        s.voice_wake_word = v.clone();
    }
    if let Some(v) = store.get("voice_tts_voice") {
        s.voice_tts_voice = v.clone();
    }
    if let Some(v) = store.get("voice_recording_duration_secs") {
        if let Ok(n) = v.parse() {
            s.voice_recording_duration_secs = n;
        }
    }
    if let Some(v) = store.get("voice_whisper_url") {
        s.voice_whisper_url = v.clone();
    }
    if let Some(v) = store.get("retention_event_log_days") {
        if let Ok(n) = v.parse() {
            s.retention_event_log_days = n;
        }
    }
    if let Some(v) = store.get("retention_sensor_days") {
        if let Ok(n) = v.parse() {
            s.retention_sensor_days = n;
        }
    }
    if let Some(v) = store.get("retention_session_messages_keep") {
        if let Ok(n) = v.parse() {
            s.retention_session_messages_keep = n;
        }
    }
    if let Some(v) = store.get("prompt_style") {
        s.prompt_style = v.clone();
    }
    if let Some(v) = store.get("custom_system_prompt") {
        s.custom_system_prompt = if v.is_empty() { None } else { Some(v.clone()) };
    }
    if let Some(v) = store.get("prompt_addendum") {
        s.prompt_addendum = v.clone();
    }
    // Egress gate: must round-trip, for the same reason as `mic_enabled`.
    if let Some(v) = store.get("network_mode") {
        s.network_mode = v.clone();
    }
    s
}

#[async_trait]
impl SettingsRepository for MockSettingsRepository {
    async fn get(&self) -> Result<Settings> {
        let store = self.store.read().await;
        Ok(build_settings(&store))
    }

    async fn update(&self, settings: &Settings) -> Result<()> {
        let mut store = self.store.write().await;
        store.insert("assistant_name".into(), settings.assistant_name.clone());
        store.insert(
            "assistant_personality".into(),
            settings.assistant_personality.clone(),
        );
        store.insert("user_name".into(), settings.user_name.clone());
        store.insert("timezone".into(), settings.timezone.clone());
        store.insert("llm_max_tokens".into(), settings.llm_max_tokens.to_string());
        store.insert(
            "mesh_settlement_millisats_per_token".into(),
            settings.mesh_settlement_millisats_per_token.to_string(),
        );
        store.insert(
            "mesh_lend_token_ceiling".into(),
            settings.mesh_lend_token_ceiling.to_string(),
        );
        store.insert(
            "llm_temperature".into(),
            settings.llm_temperature.to_string(),
        );
        store.insert("llm_provider".into(), settings.llm_provider.clone());
        store.insert("mic_enabled".into(), settings.mic_enabled.to_string());
        store.insert(
            "hybrid_compaction_enabled".into(),
            settings.hybrid_compaction_enabled.to_string(),
        );
        store.insert(
            "context_monitor_enabled".into(),
            settings.context_monitor_enabled.to_string(),
        );
        store.insert("voice_wake_word".into(), settings.voice_wake_word.clone());
        store.insert("voice_tts_voice".into(), settings.voice_tts_voice.clone());
        store.insert(
            "voice_recording_duration_secs".into(),
            settings.voice_recording_duration_secs.to_string(),
        );
        store.insert(
            "voice_whisper_url".into(),
            settings.voice_whisper_url.clone(),
        );
        store.insert(
            "retention_event_log_days".into(),
            settings.retention_event_log_days.to_string(),
        );
        store.insert(
            "retention_sensor_days".into(),
            settings.retention_sensor_days.to_string(),
        );
        store.insert(
            "retention_session_messages_keep".into(),
            settings.retention_session_messages_keep.to_string(),
        );
        store.insert("prompt_style".into(), settings.prompt_style.clone());
        store.insert(
            "custom_system_prompt".into(),
            settings
                .custom_system_prompt
                .as_deref()
                .unwrap_or("")
                .to_string(),
        );
        store.insert("prompt_addendum".into(), settings.prompt_addendum.clone());
        store.insert("network_mode".into(), settings.network_mode.clone());
        Ok(())
    }

    async fn get_key(&self, key: &str) -> Result<Option<String>> {
        Ok(self.store.read().await.get(key).cloned())
    }

    async fn set_key(&self, key: &str, value: String) -> Result<()> {
        self.store.write().await.insert(key.to_string(), value);
        Ok(())
    }

    async fn delete_key(&self, key: &str) -> Result<()> {
        self.store.write().await.remove(key);
        Ok(())
    }

    async fn mark_user_set(&self, keys: &HashSet<String>) -> Result<()> {
        // Mirrors the adapter: only keys that already have a value are marked.
        let store = self.store.read().await;
        let mut marked = self.user_set.write().await;
        for key in keys {
            if store.contains_key(key) {
                marked.insert(key.clone());
            }
        }
        Ok(())
    }

    async fn is_user_set(&self, key: &str) -> Result<bool> {
        Ok(self.user_set.read().await.contains(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_store_returns_defaults() {
        let repo = MockSettingsRepository::new();
        let s = repo.get().await.unwrap();
        assert_eq!(s.assistant_name, "Goose");
        assert_eq!(s.llm_max_tokens, 4096);
    }

    #[tokio::test]
    async fn update_and_get_roundtrip() {
        let repo = MockSettingsRepository::new();
        let mut s = Settings::default();
        s.assistant_name = "Duck".to_string();
        s.llm_max_tokens = 2048;
        repo.update(&s).await.unwrap();

        let loaded = repo.get().await.unwrap();
        assert_eq!(loaded.assistant_name, "Duck");
        assert_eq!(loaded.llm_max_tokens, 2048);
        assert_eq!(loaded.llm_temperature, 0.7); // unchanged default
    }

    #[tokio::test]
    async fn get_key_and_set_key() {
        let repo = MockSettingsRepository::new();
        assert!(repo.get_key("assistant_name").await.unwrap().is_none());
        repo.set_key("assistant_name", "Pond".to_string())
            .await
            .unwrap();
        assert_eq!(
            repo.get_key("assistant_name").await.unwrap(),
            Some("Pond".to_string())
        );
    }

    /// Only an explicit `mark_user_set` (the PUT patch key set) claims intent.
    #[tokio::test]
    async fn snapshot_write_does_not_imply_user_intent() {
        let repo = MockSettingsRepository::new();
        repo.update(&Settings::default()).await.unwrap();
        assert!(!repo.is_user_set("assistant_name").await.unwrap());

        let patch: HashSet<String> = ["assistant_name".to_string()].into_iter().collect();
        repo.mark_user_set(&patch).await.unwrap();
        assert!(repo.is_user_set("assistant_name").await.unwrap());
        assert!(!repo.is_user_set("user_name").await.unwrap());
    }

    #[tokio::test]
    async fn set_key_reflected_in_get() {
        let repo = MockSettingsRepository::new();
        repo.set_key("user_name", "Jerry".to_string())
            .await
            .unwrap();
        let s = repo.get().await.unwrap();
        assert_eq!(s.user_name, "Jerry");
    }
}
