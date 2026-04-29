//! SQLite-backed implementation of `SettingsRepository`.
//!
//! Uses the existing `settings(key TEXT PRIMARY KEY, value TEXT, updated_at TEXT)` table
//! in `pond_system.db`. Each Setting field maps to one row; missing keys fall back to
//! `Settings::default()`.
//!
//! IMPORTANT: `update()` issues one `INSERT OR REPLACE` per field — never batched into
//! a single query (sqlx only executes the first statement when multiple are batched).

use anyhow::Result;
use async_trait::async_trait;
use pond_core::domain::settings::Settings;
use pond_core::ports::settings::SettingsRepository;
use sqlx::{Pool, Sqlite};
use serde_json;

pub struct SqliteSettingsRepository {
    pool: Pool<Sqlite>,
}

impl SqliteSettingsRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SettingsRepository for SqliteSettingsRepository {
    async fn get(&self) -> Result<Settings> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT key, value FROM settings")
                .fetch_all(&self.pool)
                .await?;

        let mut s = Settings::default();
        for (key, value) in rows {
            apply_key(&mut s, &key, &value);
        }
        Ok(s)
    }

    async fn update(&self, settings: &Settings) -> Result<()> {
        macro_rules! upsert {
            ($key:expr, $val:expr) => {
                sqlx::query(
                    "INSERT OR REPLACE INTO settings (key, value, updated_at) \
                     VALUES (?, ?, datetime('now'))",
                )
                .bind($key)
                .bind($val)
                .execute(&self.pool)
                .await?;
            };
        }

        upsert!("primary_profile_id", settings.primary_profile_id.as_deref().unwrap_or(""));
        upsert!("chat_provider",      &settings.chat_provider);
        upsert!("chat_model",         &settings.chat_model);
        upsert!("tool_model",          settings.tool_model.as_deref().unwrap_or(""));
        upsert!("assistant_name",                  &settings.assistant_name);
        upsert!("assistant_personality",           &settings.assistant_personality);
        upsert!("user_name",                       &settings.user_name);
        upsert!("timezone",                        &settings.timezone);
        upsert!("llm_max_tokens",                  settings.llm_max_tokens.to_string());
        upsert!("llm_temperature",                 settings.llm_temperature.to_string());
        upsert!("llm_provider",                    &settings.llm_provider);
        upsert!("voice_wake_word",                      &settings.voice_wake_word);
        upsert!("voice_kws_whisper_url",
            settings.voice_kws_whisper_url.as_deref().unwrap_or(""));
        upsert!("voice_kws_energy_threshold",           settings.voice_kws_energy_threshold.to_string());
        upsert!("voice_kws_post_trigger_silence_ms",    settings.voice_kws_post_trigger_silence_ms.to_string());
        upsert!("voice_kws_cooldown_ms",                settings.voice_kws_cooldown_ms.to_string());
        upsert!("voice_wake_word_transcriptions",
            serde_json::to_string(&settings.voice_wake_word_transcriptions)
                .unwrap_or_else(|_| "[]".to_string()));
        upsert!("voice_tts_voice",                 &settings.voice_tts_voice);
        upsert!("voice_recording_duration_secs",   settings.voice_recording_duration_secs.to_string());
        upsert!("voice_whisper_url",               &settings.voice_whisper_url);
        upsert!("active_llm_model",                &settings.active_llm_model);
        upsert!("active_whisper_model",            &settings.active_whisper_model);
        upsert!("active_tts_model",                &settings.active_tts_model);
        upsert!("retention_event_log_days",        settings.retention_event_log_days.to_string());
        upsert!("retention_sensor_days",           settings.retention_sensor_days.to_string());
        upsert!("retention_session_messages_keep", settings.retention_session_messages_keep.to_string());
        upsert!("prompt_style",         &settings.prompt_style);
        upsert!("custom_system_prompt", settings.custom_system_prompt.as_deref().unwrap_or(""));
        upsert!("prompt_addendum",      &settings.prompt_addendum);
        upsert!("agent_goose_mode",     &settings.agent_goose_mode);
        upsert!("agent_max_turns",      settings.agent_max_turns.to_string());
        upsert!("agent_memory_inject",  if settings.agent_memory_inject { "true" } else { "false" });
        upsert!("agent_memory_limit",   settings.agent_memory_limit.to_string());
        upsert!("weather_enabled",       if settings.weather_enabled { "true" } else { "false" });
        upsert!("weather_latitude",      settings.weather_latitude.to_string());
        upsert!("weather_longitude",     settings.weather_longitude.to_string());
        upsert!("weather_location_name", &settings.weather_location_name);
        // Thinking / reasoning
        upsert!("thinking_mode",         &settings.thinking_mode);
        upsert!("show_thinking",         if settings.show_thinking { "true" } else { "false" });
        upsert!("context_window_override", settings.context_window_override.to_string());
        // Answer review
        upsert!("review_mode",            &settings.review_mode);
        upsert!("review_max_rounds",      settings.review_max_rounds.to_string());
        upsert!("review_pass_threshold",  settings.review_pass_threshold.to_string());

        Ok(())
    }

    async fn get_key(&self, key: &str) -> Result<Option<String>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(v,)| v))
    }

    async fn set_key(&self, key: &str, value: String) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO settings (key, value, updated_at) \
             VALUES (?, ?, datetime('now'))",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// Apply a single key-value pair from the DB onto a `Settings` struct.
fn apply_key(s: &mut Settings, key: &str, value: &str) {
    match key {
        "primary_profile_id" => {
            s.primary_profile_id = if value.is_empty() { None } else { Some(value.to_string()) };
        }
        "chat_provider"  => s.chat_provider = value.to_string(),
        "chat_model"     => s.chat_model = value.to_string(),
        "tool_model" => {
            s.tool_model = if value.is_empty() { None } else { Some(value.to_string()) };
        }
        // Legacy keys — silently ignored for backward compat with old DBs
        "think_provider" | "think_model" | "task_provider" | "task_model" => {}
        "assistant_name"                  => s.assistant_name = value.to_string(),
        "assistant_personality"           => s.assistant_personality = value.to_string(),
        "user_name"                       => s.user_name = value.to_string(),
        "timezone"                        => s.timezone = value.to_string(),
        "llm_max_tokens"                  => {
            if let Ok(v) = value.parse() { s.llm_max_tokens = v; }
        }
        "llm_temperature"                 => {
            if let Ok(v) = value.parse() { s.llm_temperature = v; }
        }
        "llm_provider"                    => s.llm_provider = value.to_string(),
        "voice_wake_word"                 => s.voice_wake_word = value.to_string(),
        "voice_kws_whisper_url"           => {
            s.voice_kws_whisper_url = if value.is_empty() { None } else { Some(value.to_string()) };
        }
        "voice_kws_energy_threshold"      => {
            if let Ok(v) = value.parse() { s.voice_kws_energy_threshold = v; }
        }
        "voice_kws_post_trigger_silence_ms" => {
            if let Ok(v) = value.parse() { s.voice_kws_post_trigger_silence_ms = v; }
        }
        "voice_kws_cooldown_ms"           => {
            if let Ok(v) = value.parse() { s.voice_kws_cooldown_ms = v; }
        }
        "voice_wake_word_transcriptions"  => {
            if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                s.voice_wake_word_transcriptions = v;
            }
        }
        "voice_tts_voice"                 => s.voice_tts_voice = value.to_string(),
        "voice_recording_duration_secs"   => {
            if let Ok(v) = value.parse() { s.voice_recording_duration_secs = v; }
        }
        "voice_whisper_url"               => s.voice_whisper_url = value.to_string(),
        "active_llm_model"                => s.active_llm_model = value.to_string(),
        "active_whisper_model"            => s.active_whisper_model = value.to_string(),
        "active_tts_model"                => s.active_tts_model = value.to_string(),
        "retention_event_log_days"        => {
            if let Ok(v) = value.parse() { s.retention_event_log_days = v; }
        }
        "retention_sensor_days"           => {
            if let Ok(v) = value.parse() { s.retention_sensor_days = v; }
        }
        "retention_session_messages_keep" => {
            if let Ok(v) = value.parse() { s.retention_session_messages_keep = v; }
        }
        "prompt_style"         => s.prompt_style = value.to_string(),
        "custom_system_prompt" => {
            s.custom_system_prompt = if value.is_empty() { None } else { Some(value.to_string()) };
        }
        "prompt_addendum"      => s.prompt_addendum = value.to_string(),
        "agent_goose_mode"     => s.agent_goose_mode = value.to_string(),
        "agent_max_turns"      => {
            if let Ok(v) = value.parse() { s.agent_max_turns = v; }
        }
        "agent_memory_inject"  => s.agent_memory_inject = value == "true",
        "agent_memory_limit"   => {
            if let Ok(v) = value.parse() { s.agent_memory_limit = v; }
        }
        "weather_enabled"       => s.weather_enabled = value == "true",
        "weather_latitude"      => { if let Ok(v) = value.parse() { s.weather_latitude = v; } }
        "weather_longitude"     => { if let Ok(v) = value.parse() { s.weather_longitude = v; } }
        "weather_location_name" => s.weather_location_name = value.to_string(),
        // Thinking / reasoning
        "thinking_mode"          => s.thinking_mode = value.to_string(),
        "show_thinking"          => s.show_thinking = value == "true",
        "context_window_override" => {
            if let Ok(v) = value.parse() { s.context_window_override = v; }
        }
        // Answer review
        "review_mode"            => s.review_mode = value.to_string(),
        "review_max_rounds"      => {
            if let Ok(v) = value.parse() { s.review_max_rounds = v; }
        }
        "review_pass_threshold"  => {
            if let Ok(v) = value.parse() { s.review_pass_threshold = v; }
        }
        _ => {} // unknown key — ignore
    }
}
