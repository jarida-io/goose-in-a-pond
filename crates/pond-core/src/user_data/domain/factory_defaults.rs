//! Reads `config/factory_defaults.toml`, embedded at compile time, for `Settings::default_*()`.
//! Every lookup here takes a fallback: a missing key, an unparsable file, or a value of the
//! wrong type all just return the fallback — the literal that used to be the only source of
//! truth, before this file existed. A typo in the TOML degrades silently rather than breaking
//! startup; `settings.rs`'s `every_factory_default_is_in_the_file` test is what catches drift
//! between the two instead.

use std::sync::OnceLock;

const RAW: &str = include_str!("../../../config/factory_defaults.toml");

fn table() -> &'static toml::Table {
    static TABLE: OnceLock<toml::Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        RAW.parse::<toml::Table>().unwrap_or_else(|e| {
            tracing::error!(error = %e, "factory_defaults.toml failed to parse; every setting falls back to its built-in default");
            toml::Table::new()
        })
    })
}

pub(super) fn str(key: &str, fallback: &str) -> String {
    match table().get(key) {
        Some(toml::Value::String(s)) => s.clone(),
        _ => fallback.to_string(),
    }
}

pub(super) fn boolean(key: &str, fallback: bool) -> bool {
    match table().get(key) {
        Some(toml::Value::Boolean(b)) => *b,
        _ => fallback,
    }
}

pub(super) fn u8(key: &str, fallback: u8) -> u8 {
    match table().get(key) {
        Some(toml::Value::Integer(i)) => u8::try_from(*i).unwrap_or(fallback),
        _ => fallback,
    }
}

pub(super) fn u32(key: &str, fallback: u32) -> u32 {
    match table().get(key) {
        Some(toml::Value::Integer(i)) => u32::try_from(*i).unwrap_or(fallback),
        _ => fallback,
    }
}

pub(super) fn u64(key: &str, fallback: u64) -> u64 {
    match table().get(key) {
        Some(toml::Value::Integer(i)) => u64::try_from(*i).unwrap_or(fallback),
        _ => fallback,
    }
}

pub(super) fn f32(key: &str, fallback: f32) -> f32 {
    match table().get(key) {
        Some(toml::Value::Float(f)) => *f as f32,
        Some(toml::Value::Integer(i)) => *i as f32,
        _ => fallback,
    }
}

pub(super) fn f64(key: &str, fallback: f64) -> f64 {
    match table().get(key) {
        Some(toml::Value::Float(f)) => *f,
        Some(toml::Value::Integer(i)) => *i as f64,
        _ => fallback,
    }
}

/// Every key `Settings::default_*()` actually looks up — kept here, not derived, so a function
/// that forgets to add its key is caught the same way one with a typo'd key is.
#[cfg(test)]
pub(super) const EXPECTED_KEYS: &[&str] = &[
    "prompt_style",
    "prompt_addendum",
    "assistant_name",
    "assistant_personality",
    "user_name",
    "timezone",
    "home_name",
    "max_tokens",
    "temperature",
    "llm_provider",
    "active_llm_model",
    "wake_word",
    "voice_room_enabled",
    "browser_rendering_enabled",
    "kws_energy_threshold",
    "kws_post_trigger_silence_ms",
    "kws_cooldown_ms",
    "tts_voice",
    "tts_speed",
    "tts_quality",
    "wake_detector",
    "vad_backend",
    "voice_thinking_tone_enabled",
    "voice_tts_gpu_enabled",
    "audio_sound_test",
    "audio_manage_system",
    "recording_duration",
    "whisper_url",
    "active_whisper_model",
    "active_tts_model",
    "embedding_provider",
    "weather_enabled",
    "weather_latitude",
    "weather_longitude",
    "weather_location_name",
    "vision_enabled",
    "vision_camera_url",
    "vision_classifier_model",
    "vision_face_recognition_enabled",
    "vision_face_on_motion",
    "picture_support_download_enabled",
    "matter_ws_url",
    "matter_ble_enabled",
    "mesh_enabled",
    "lightning_enabled",
    "mesh_settlement_millisats_per_token",
    "mesh_lend_token_ceiling",
    "mic_enabled",
    "cameras_enabled",
    "cloud_fallback_enabled",
    "event_log_days",
    "sensor_days",
    "session_messages_keep",
    "events_days",
    "sensitive_days",
    "thinking_mode",
    "reasoning_effort",
    "review_mode",
    "review_max_rounds",
    "review_pass_threshold",
    "summary_idle_secs",
    "hybrid_compaction_enabled",
    "agent_backend",
    "agent_goose_mode",
    "agent_max_turns",
    "voice_max_turns",
    "agent_timeout_secs",
    "tool_selection_mode",
    "security_policy_mode",
    "network_mode",
    "prefix_cache_prompt",
    "agent_memory_inject",
    "agent_memory_limit",
    "schedule_result_notify",
    "system_notices_enabled",
    "lone_voice_sets_reminders",
    "unseen_phone_remove_days",
    "unseen_phone_skip_days",
    "speak_unfetched_reminders",
    "notices_wait_in_quiet_hours",
    "tool_output_compaction",
    "suggestion_generation_enabled",
    "memory_extraction_enabled",
    "memory_cleanup_enabled",
    "memory_consolidation_enabled",
    "session_titling_enabled",
    "memory_consolidation_mode",
    "memory_decay_base_half_life_days",
    "memory_decay_beta",
    "memory_prune_threshold",
    "memory_archive_threshold",
    "memory_cleanup_interval_hours",
    "memory_consolidation_interval_hours",
    "memory_consolidation_batch_size",
    "memory_extraction_max_facts",
    "memory_extraction_interval_secs",
    "memory_extraction_sessions_per_pass",
    "memory_extraction_window_messages",
    "memory_extraction_idle_secs",
    "memory_reinforce_threshold",
    "memory_relate_threshold",
    "memory_date_proposals_enabled",
    "memory_extraction_mode",
    "memory_extraction_first_pass_at",
    "schedule_max_concurrent",
    "schedule_max_runs_per_task",
    "goal_check_enabled",
    "context_monitor_enabled",
    "cloud_input_price_per_million",
    "cloud_output_price_per_million",
    "telemetry_enabled",
    "tool_call_validation",
    "tool_request_detection",
    "ext_enabled",
    "ext_orchestrator_enabled",
    "ext_travel_enabled",
    "unprompted_speech_enabled",
    "quiet_hours_start",
    "quiet_hours_end",
    "unprompted_speech_categories",
    "proactive_review_enabled",
    "long_tasks_enabled",
    "agent_activations_enabled",
    "context_ingest_enabled",
    "spoken_pairing_codes",
    "ble_setup_enabled",
    "ext_context_enabled",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_file_parses() {
        let parsed = RAW.parse::<toml::Table>();
        assert!(parsed.is_ok(), "{:?}", parsed.err());
    }

    #[test]
    fn every_expected_key_is_present_in_the_file() {
        let t = table();
        let missing: Vec<&str> = EXPECTED_KEYS
            .iter()
            .filter(|k| !t.contains_key(**k))
            .copied()
            .collect();
        assert!(
            missing.is_empty(),
            "factory_defaults.toml is missing keys that Settings::default_*() looks up: {missing:?}"
        );
    }

    #[test]
    fn a_missing_key_falls_back_rather_than_panicking() {
        assert_eq!(str("not_a_real_key", "fallback"), "fallback");
        assert_eq!(boolean("not_a_real_key", true), true);
        assert_eq!(u32("not_a_real_key", 7), 7);
    }

    #[test]
    fn a_wrong_typed_value_falls_back_rather_than_panicking() {
        // `assistant_name` is a string in the file; asking for it as a bool must not panic.
        assert_eq!(boolean("assistant_name", true), true);
    }

    #[test]
    fn scalar_kinds_read_back_the_file_s_own_values() {
        assert_eq!(str("assistant_name", "wrong"), "Goose");
        assert_eq!(boolean("voice_room_enabled", false), true);
        assert_eq!(u32("max_tokens", 0), 4096);
        assert_eq!(u64("kws_cooldown_ms", 0), 2000);
        assert_eq!(u8("review_pass_threshold", 0), 3);
        assert_eq!(f32("temperature", 0.0), 0.7);
        assert_eq!(f64("weather_latitude", 1.0), 0.0);
    }
}
