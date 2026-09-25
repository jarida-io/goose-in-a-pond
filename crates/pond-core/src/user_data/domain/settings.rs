//! GIAP settings; each field is a key in the flat `settings` SQLite table, defaulted when absent.

use serde::{Deserialize, Serialize};

/// Where GIAP's Matter controller listens; the path names its protocol so stale URLs fail loudly.
pub const DEFAULT_MATTER_WS_URL: &str = "ws://127.0.0.1:5580/giap";

/// The pre-`giap-matter` default; the controller no longer serves this path, so it is migrated.
pub const LEGACY_MATTER_WS_URL: &str = "ws://127.0.0.1:5580/ws";

/// Rewrite the superseded default, leaving anything user-chosen alone.
pub fn migrate_matter_ws_url(stored: &str) -> String {
    if stored.trim() == LEGACY_MATTER_WS_URL {
        DEFAULT_MATTER_WS_URL.to_string()
    } else {
        stored.to_string()
    }
}

/// Turn budget the engine gets for `agent_max_turns == 0`; unreachable in practice.
/// Not `u32::MAX`, which overflows arithmetic on the budget and renders absurdly in the prompt.
pub const UNCAPPED_MAX_TURNS: u32 = 100_000;

/// `tool_selection_mode`: send every registered extension's tools every turn.
pub const TOOL_SELECTION_MODE_ALL: &str = "all";
/// `tool_selection_mode`: core groups plus those scored relevant, fixed per session for KV reuse.
pub const TOOL_SELECTION_MODE_RELEVANT: &str = "relevant";
/// `tool_selection_mode`: only the toolkit hatch; other groups arrive via `enable_tool_group`.
pub const TOOL_SELECTION_MODE_MINIMAL: &str = "minimal";

/// `security_policy_mode`: no evaluation, no audit trail. Debugging only.
pub const SECURITY_POLICY_MODE_OFF: &str = "off";
/// `security_policy_mode`: evaluate and record every decision, block none.
pub const SECURITY_POLICY_MODE_AUDIT: &str = "audit";
/// `security_policy_mode`: denials bite.
pub const SECURITY_POLICY_MODE_ENFORCE: &str = "enforce";

pub const SECURITY_POLICY_MODES: &[&str] = &[
    SECURITY_POLICY_MODE_OFF,
    SECURITY_POLICY_MODE_AUDIT,
    SECURITY_POLICY_MODE_ENFORCE,
];

/// `network_mode`: record every outbound call, refuse none.
pub const NETWORK_MODE_OPEN: &str = "open";
/// `network_mode`: refuse hosts that classify as privacy-`Sensitive`.
pub const NETWORK_MODE_ALLOWLIST: &str = "allowlist";
/// `network_mode`: refuse everything that is not loopback.
pub const NETWORK_MODE_OFFLINE: &str = "offline";

pub const NETWORK_MODES: &[&str] = &[
    NETWORK_MODE_OPEN,
    NETWORK_MODE_ALLOWLIST,
    NETWORK_MODE_OFFLINE,
];

/// Accepted `reasoning_effort` values; must stay in step with `context_budget::ReasoningEffort`.
pub const REASONING_EFFORTS: &[&str] = &["brief", "balanced", "thorough"];

// ── The remaining closed vocabularies ───────────────────────────────────────
// Enforced server-side by `settings_validation::FIELD_RULES`; not every writer is the desktop UI.

/// How many tools a turn is offered. See `tool_selection_mode`.
pub const TOOL_SELECTION_MODES: &[&str] = &[
    TOOL_SELECTION_MODE_ALL,
    TOOL_SELECTION_MODE_RELEVANT,
    TOOL_SELECTION_MODE_MINIMAL,
];

/// Allowed agent loops; any value but `goose` would silently select `MockAgent`.
pub const AGENT_BACKENDS: &[&str] = &["goose"];

/// Backends that exist but must never be selected; listed so the refusal can say why.
pub const QUARANTINED_AGENT_BACKENDS: &[&str] = &["pond"];

/// Built-in system-prompt templates.
pub const PROMPT_STYLES: &[&str] = &["balanced", "concise", "technical", "warm"];

/// Whether the model is asked to think before answering.
pub const THINKING_MODES: &[&str] = &["auto", "on", "off"];

/// Whether a turn's answer is reviewed before it is shown.
pub const REVIEW_MODES: &[&str] = &["off", "on", "auto"];

/// How memory consolidation is run.
pub const CONSOLIDATION_MODES: &[&str] = &["single", "adversarial"];

/// Which engine computes embeddings.
pub const EMBEDDING_PROVIDERS: &[&str] = &["fastembed", "gguf", "none"];

/// Kokoro quality tiers, smallest first.
pub const TTS_QUALITIES: &[&str] = &["q4", "q4f16", "q8", "q8f16", "fp16", "fp32"];

/// Speech detectors: `rms` (energy gate, fooled by steady noise) or `silero` (2 MB ONNX model).
pub const VAD_BACKENDS: &[&str] = &["rms", "silero"];

/// A factory default that changed after installs existed, adopted once by `migration`.
/// Saved snapshots pin every key, so only a stored value equal to `old_default` is rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultAdoption {
    pub key: &'static str,
    /// How the OLD default was rendered into the store.
    pub old_default: &'static str,
    /// How the CURRENT default is rendered into the store.
    pub new_default: &'static str,
    /// Numeric prefix of the migration that performs the adoption.
    pub migration: &'static str,
}

/// Shipped default changes, oldest first; a second move needs a new chained entry and migration.
/// pond-infra checks each `new_default` against the literal the adapter writes.
pub const DEFAULT_ADOPTIONS: &[DefaultAdoption] = &[
    DefaultAdoption {
        key: "agent_max_turns",
        old_default: "20",
        new_default: "50",
        migration: "0035",
    },
    DefaultAdoption {
        key: "hybrid_compaction_enabled",
        old_default: "false",
        new_default: "true",
        migration: "0035",
    },
    DefaultAdoption {
        key: "vad_backend",
        old_default: "rms",
        new_default: "silero",
        migration: "0052",
    },
    DefaultAdoption {
        key: "voice_max_turns",
        old_default: "8",
        new_default: "0",
        migration: "0053",
    },
];

/// All configurable settings for GIAP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    // ── Assistant identity ──────────────────────────────────────────────────
    /// UUID of the primary household profile created during onboarding.
    #[serde(default)]
    pub primary_profile_id: Option<String>,

    #[serde(default = "Settings::default_assistant_name")]
    pub assistant_name: String,

    #[serde(default = "Settings::default_assistant_personality")]
    pub assistant_personality: String,

    #[serde(default = "Settings::default_user_name")]
    pub user_name: String,

    /// IANA timezone string, e.g. "Africa/Nairobi"
    #[serde(default = "Settings::default_timezone")]
    pub timezone: String,

    #[serde(default = "Settings::default_home_name")]
    pub home_name: String,

    /// Built-in system-prompt template; one of `PROMPT_STYLES`.
    #[serde(default = "Settings::default_prompt_style")]
    pub prompt_style: String,

    /// Full system-prompt override (beats `prompt_style`); `PromptTemplate` placeholders apply.
    #[serde(default)]
    pub custom_system_prompt: Option<String>,

    /// Extra instructions appended to the generated system prompt (max 500 chars).
    #[serde(default = "Settings::default_prompt_addendum")]
    pub prompt_addendum: String,

    // ── Model roles ────────────────────────────────────────────────────────
    /// Provider for the Chat role (fast, conversational). Default = llm_provider.
    #[serde(default = "Settings::default_llm_provider")]
    pub chat_provider: String,

    /// Model name for the Chat role. Default = active_llm_model.
    #[serde(default = "Settings::default_active_llm_model")]
    pub chat_model: String,

    /// Small GGUF in `$DATA_DIR/models/gguf/` that re-generates empty tool-call arguments.
    #[serde(default)]
    pub tool_model: Option<String>,

    // ── LLM behaviour ──────────────────────────────────────────────────────
    #[serde(default = "Settings::default_max_tokens")]
    pub llm_max_tokens: u32,

    /// Sampling temperature (0.0 = deterministic, 1.0 = creative)
    #[serde(default = "Settings::default_temperature")]
    pub llm_temperature: f32,

    /// Active LLM provider: "llamafile", "ollama", or "mock"
    #[serde(default = "Settings::default_llm_provider")]
    pub llm_provider: String,

    // ── Voice pipeline ─────────────────────────────────────────────────────
    /// Wake word / phrase detected by WhisperKeywordDetector (case-insensitive)
    #[serde(default = "Settings::default_wake_word")]
    pub voice_wake_word: String,

    /// Separate whisper server for wake-word spotting; `None` uses `voice_whisper_url`.
    #[serde(default)]
    pub voice_kws_whisper_url: Option<String>,

    /// RMS floor below which wake-word windows skip whisper; 0.0 disables the gate.
    #[serde(default = "Settings::default_kws_energy_threshold")]
    pub voice_kws_energy_threshold: f32,

    /// Silence (ms) ending post-trigger capture early; 0 always waits the full `post_trigger_ms`.
    #[serde(default = "Settings::default_kws_post_trigger_silence_ms")]
    pub voice_kws_post_trigger_silence_ms: u64,

    /// Delay (ms) before re-arming detection after an activation; stops re-triggers on TTS echo.
    #[serde(default = "Settings::default_kws_cooldown_ms")]
    pub voice_kws_cooldown_ms: u64,

    /// Wake-word variants from calibration (any matches); empty falls back to `voice_wake_word`.
    #[serde(default)]
    pub voice_wake_word_transcriptions: Vec<String>,

    /// Kokoro voice id (`af_heart`, …); a legacy Piper `.onnx` filename is still accepted.
    #[serde(default = "Settings::default_tts_voice")]
    pub voice_tts_voice: String,

    /// Pace multiplier for the synthesiser, not a percentage; the engine clamps to 0.5..=2.0.
    #[serde(default = "Settings::default_tts_speed")]
    pub voice_tts_speed: f32,

    /// Kokoro quantisation tier (`TTS_QUALITIES`); an unknown value falls back to the default.
    /// Default `q8` (92 MB) is the tier that fits beside an LLM on an 8 GB Jetson.
    #[serde(default = "Settings::default_tts_quality")]
    pub voice_tts_quality: String,

    /// End-of-speech detector (`rms` | `silero`); `silero` falls back to `rms` if it cannot load.
    /// Onset stays on the energy gate: a freshly reset Silero would clip the first word.
    #[serde(default = "Settings::default_vad_backend")]
    pub vad_backend: String,

    /// Plays a soft tone while the model works: the only sign a spoken request was heard.
    #[serde(default = "Settings::default_voice_thinking_tone_enabled")]
    pub voice_thinking_tone_enabled: bool,

    #[serde(default = "Settings::default_recording_duration")]
    pub voice_recording_duration_secs: u32,

    /// Base URL of the whisper.cpp server
    #[serde(default = "Settings::default_whisper_url")]
    pub voice_whisper_url: String,

    // ── Active model selection ──────────────────────────────────────────────
    /// Active LLM model name from the registry (e.g. "gemma-2b", "llama-1b")
    #[serde(default = "Settings::default_active_llm_model")]
    pub active_llm_model: String,

    /// Active Whisper model name from the registry (e.g. "base", "tiny", "small")
    #[serde(default = "Settings::default_active_whisper_model")]
    pub active_whisper_model: String,

    /// Active TTS model name from the registry (e.g. "piper-lessac")
    #[serde(default = "Settings::default_active_tts_model")]
    pub active_tts_model: String,

    /// Active embedding model name from the registry (e.g. "all-MiniLM-L6-v2")
    #[serde(default)]
    pub active_embedding_model: String,

    /// `gguf` (llama.cpp) is the on-device path and needs the `local-inference` build.
    /// fastembed's ONNX Runtime does not initialise on the Jetson Orin.
    #[serde(default = "Settings::default_embedding_provider")]
    pub embedding_provider: String,

    // ── Weather ────────────────────────────────────────────────────────────
    /// Whether to fetch live weather and inject it into the LLM system prompt.
    #[serde(default = "Settings::default_weather_enabled")]
    pub weather_enabled: bool,

    /// Latitude for the weather location (decimal degrees, e.g. -1.286 for Nairobi).
    #[serde(default = "Settings::default_weather_latitude")]
    pub weather_latitude: f64,

    /// Longitude for the weather location (decimal degrees, e.g. 36.817 for Nairobi).
    #[serde(default = "Settings::default_weather_longitude")]
    pub weather_longitude: f64,

    /// Human-readable location name shown in context and API responses.
    #[serde(default = "Settings::default_weather_location_name")]
    pub weather_location_name: String,

    // ── Vision (#130) ──────────────────────────────────────────────────────
    /// On-device camera capture + motion detection; needs a camera and ffmpeg.
    #[serde(default = "Settings::default_vision_enabled")]
    pub vision_enabled: bool,

    /// An `rtsp://` URL or device path like `/dev/video0`; empty = pipeline not started.
    #[serde(default = "Settings::default_vision_camera_url")]
    pub vision_camera_url: String,

    /// `camera_id` stamped on vision events; automation rules match on it.
    #[serde(default = "Settings::default_vision_camera_id")]
    pub vision_camera_id: String,

    /// Frames analysed per second; kept low to bound CPU use on the Jetson.
    #[serde(default = "Settings::default_vision_fps")]
    pub vision_fps: u32,

    /// Fraction of the frame (0.0–1.0) that must change to count as motion.
    #[serde(default = "Settings::default_vision_motion_threshold")]
    pub vision_motion_threshold: f64,

    /// ONNX detector labelling motion events; empty = bundled YOLOX-Nano, auto-downloaded.
    /// Relative paths resolve under `<data_dir>/models/vision/`; ignored without `vision-onnx`.
    #[serde(default = "Settings::default_vision_classifier_model")]
    pub vision_classifier_model: String,

    // ── Matter (#195) ──────────────────────────────────────────────────────
    #[serde(default = "Settings::default_matter_ws_url")]
    pub matter_ws_url: String,

    /// Also pair over BLE (for devices not yet on Wi-Fi); changing it restarts the controller.
    /// Off by default: needs `@stoprocent/noble` and radio permission, or macOS kills the process.
    #[serde(default = "Settings::default_matter_ble_enabled")]
    pub matter_ble_enabled: bool,

    // ── Private mesh (#132) ────────────────────────────────────────────────
    /// Starts the private P2P mesh transport; needs a `mesh` build.
    /// Its identity keypair is the `mesh_identity_secret` store key, never a `Settings` field.
    #[serde(default = "Settings::default_mesh_enabled")]
    pub mesh_enabled: bool,

    /// Breez/Spark Lightning for mesh settlement; needs a `lightning` build and `BREEZ_API_KEY`.
    /// Its wallet mnemonic is the `lightning_wallet_mnemonic` store key, not a `Settings` field.
    #[serde(default = "Settings::default_lightning_enabled")]
    pub lightning_enabled: bool,

    /// Unused exchange rate, kept only so old rows and payloads still deserialize.
    /// The live rate is `MESH_SETTLEMENT_MILLISATS_PER_TOKEN`, a constant a borrower can't lower.
    #[serde(default = "Settings::default_mesh_settlement_millisats_per_token")]
    pub mesh_settlement_millisats_per_token: u64,

    /// Max tokens lent to one peer per rolling ~15-minute window; `0` = no ceiling.
    /// A cost throttle, not a payment-verified cap: the window resets on a timer.
    #[serde(default = "Settings::default_mesh_lend_token_ceiling")]
    pub mesh_lend_token_ceiling: u64,

    // ── Privacy / sensor access ────────────────────────────────────────────
    /// Consent switch for the microphone; false forbids wake-word and ASR capture.
    #[serde(default = "Settings::default_mic_enabled")]
    pub mic_enabled: bool,

    /// Consent switch for cameras; false forbids the vision pipeline and any camera capture.
    #[serde(default = "Settings::default_cameras_enabled")]
    pub cameras_enabled: bool,

    /// Opt-in cloud spill-over on local-model failure; nothing implements the spill yet.
    #[serde(default = "Settings::default_cloud_fallback_enabled")]
    pub cloud_fallback_enabled: bool,

    // ── Data retention ─────────────────────────────────────────────────────
    /// Days to keep rows in event_log (0 = keep forever)
    #[serde(default = "Settings::default_event_log_days")]
    pub retention_event_log_days: u32,

    /// Days to keep rows in sensor_readings
    #[serde(default = "Settings::default_sensor_days")]
    pub retention_sensor_days: u32,

    /// Maximum session messages to keep per session
    #[serde(default = "Settings::default_session_messages_keep")]
    pub retention_session_messages_keep: u32,

    /// Baseline days to keep `events` rows (`0` = forever); per-category overrides win.
    #[serde(default = "Settings::default_events_days")]
    pub retention_events_days: u32,

    /// Per-`EventCategory` override (snake_case category → days) of `retention_events_days`.
    #[serde(default)]
    pub retention_events_by_category: std::collections::HashMap<String, u32>,

    /// Max days for `Sensitive`/`Secret` events, whatever their category; `0` = no extra cap.
    #[serde(default = "Settings::default_sensitive_days")]
    pub retention_sensitive_days: u32,

    // ── Thinking / Reasoning ────────────────────────────────────────────────────
    /// `auto` thinks only on models that support it (Gemma 4, Qwen3, …); `on`/`off` force it.
    #[serde(default = "Settings::default_thinking_mode")]
    pub thinking_mode: String,

    /// Forward thinking blocks to the UI as events instead of stripping them.
    #[serde(default)]
    pub show_thinking: bool,

    /// Thinking length, not whether (`thinking_mode`); `reasoning_budget_tokens` sets the count.
    /// Defaults to `brief`: on an Orin Nano every thinking token is decode-bound silence.
    #[serde(default = "Settings::default_reasoning_effort")]
    pub reasoning_effort: String,

    /// Keep turns' reasoning text in `session_thinking` for replay; opt-in, as it is unreviewed.
    /// Rows ride the session's CASCADE, not a retention policy; switching off erases nothing.
    #[serde(default)]
    pub persist_thinking: bool,

    // ── Answer Review ──────────────────────────────────────────────────────
    /// `on` reviews every answer; `auto` only Think-classified or tool-augmented ones.
    #[serde(default = "Settings::default_review_mode")]
    pub review_mode: String,

    /// Maximum review-revision rounds. 1 = one review + one optional revision.
    #[serde(default = "Settings::default_review_max_rounds")]
    pub review_max_rounds: u32,

    /// Minimum score (1-5) for the reviewer to pass an answer. Below this triggers revision.
    #[serde(default = "Settings::default_review_pass_threshold")]
    pub review_pass_threshold: u8,

    /// Caps the model's reported context window (tokens); 0 = use the model's own value.
    #[serde(default)]
    pub context_window_override: u32,

    #[serde(default)]
    pub show_turn_stats: bool,

    /// Deterministic trim + idle summary; Goose's own compaction and tool-pair summaries go off.
    /// Default on: Goose's reactive LLM compaction stalls an on-device turn mid-conversation.
    #[serde(default = "Settings::default_hybrid_compaction_enabled")]
    pub hybrid_compaction_enabled: bool,

    /// Idle seconds before a rolling-summary refresh; never at startup, and a new turn cancels it.
    #[serde(default = "Settings::default_summary_idle_secs")]
    pub summary_idle_secs: u32,

    /// Days kept verbatim before age weighting may trim harder; `0` disables age weighting.
    /// Small is the risky direction: it truncates tool results a live conversation still needs.
    #[serde(default = "Settings::default_compaction_verbatim_days")]
    pub compaction_verbatim_days: u32,

    // ── Agent behaviour ────────────────────────────────────────────────────────
    /// Agent loop; only `goose` is accepted (see `AGENT_BACKENDS`).
    #[serde(default = "Settings::default_agent_backend")]
    pub agent_backend: String,

    /// GooseMode for the agent loop: "auto" | "chat" | "smart"
    #[serde(default = "Settings::default_agent_goose_mode")]
    pub agent_goose_mode: String,

    /// Max provider calls per request; `0` = uncapped (see `UNCAPPED_MAX_TURNS`).
    #[serde(default = "Settings::default_agent_max_turns")]
    pub agent_max_turns: u32,

    /// Voice-only turn cap, clamped to `agent_max_turns`; `0` = none, same budget as text.
    #[serde(default = "Settings::default_voice_max_turns")]
    pub voice_max_turns: u32,

    /// Seconds without a stream event before a turn aborts (idle, not total, time); 0 disables.
    #[serde(default = "Settings::default_agent_timeout_secs")]
    pub agent_timeout_secs: u64,

    /// Stable system-prompt prefix + dynamic suffix, so local providers can reuse the KV-cache.
    #[serde(default = "Settings::default_prefix_cache_prompt")]
    pub prefix_cache_prompt: bool,

    /// Which tool schemas reach the prompt; other groups stay reachable via `enable_tool_group`.
    /// `all` costs ~41% of the 8,192-token local budget, `relevant` ≥9.5%, `minimal` 2.7%.
    #[serde(default = "Settings::default_tool_selection_mode")]
    pub tool_selection_mode: String,

    /// How hard `SecurityPolicy` bites; `audit` until real traffic has validated the rules.
    #[serde(default = "Settings::default_security_policy_mode")]
    pub security_policy_mode: String,

    /// How hard outbound HTTP is gated; `open` by default so an upgrade breaks nothing.
    /// `allowlist` passes only loopback and the curated list in `shared::services::egress`.
    #[serde(default = "Settings::default_network_mode")]
    pub network_mode: String,

    /// When true, recent memory fragments are injected into the system prompt each turn
    #[serde(default = "Settings::default_agent_memory_inject")]
    pub agent_memory_inject: bool,

    /// How many memory fragments to inject (most recent first)
    #[serde(default = "Settings::default_agent_memory_limit")]
    pub agent_memory_limit: u32,

    /// Semantically compress tool results (50-80% fewer tokens); off only for debugging.
    #[serde(default = "Settings::default_tool_output_compaction")]
    pub tool_output_compaction: bool,

    /// Extract durable facts from each turn into categorised memories.
    #[serde(default = "Settings::default_memory_extraction_enabled")]
    pub memory_extraction_enabled: bool,

    /// When true, a background task periodically prunes/archives decayed memories.
    #[serde(default = "Settings::default_memory_cleanup_enabled")]
    pub memory_cleanup_enabled: bool,

    /// Idle-time merge of duplicate/contradicting memories; nothing else fixes bad extractions.
    #[serde(default = "Settings::default_memory_consolidation_enabled")]
    pub memory_consolidation_enabled: bool,

    /// Let the pond retitle conversations while idle; never overwrites a name typed by hand.
    #[serde(default = "Settings::default_session_titling_enabled")]
    pub session_titling_enabled: bool,

    /// `single` (1 LLM call) or `adversarial` (3-stage Proposer/Adversary/Judge).
    #[serde(default = "Settings::default_memory_consolidation_mode")]
    pub memory_consolidation_mode: String,

    /// Experimental: retrieval also follows causal edges between memories, not only recency.
    #[serde(default)]
    pub memory_graph_enabled: bool,

    /// When true, scheduled task results are broadcast as SSE events / desktop notifications.
    #[serde(default = "Settings::default_schedule_result_notify")]
    pub schedule_result_notify: bool,

    // ── Memory tuning ────────────────────────────────────────────────────────
    /// Base decay half-life; each memory's is `base * (1 + importance)`.
    #[serde(default = "Settings::default_memory_decay_base_half_life_days")]
    pub memory_decay_base_half_life_days: f32,

    /// Decay curve steepness; lower = gentler.
    #[serde(default = "Settings::default_memory_decay_beta")]
    pub memory_decay_beta: f32,

    /// Effective decay score below which a memory is deleted.
    #[serde(default = "Settings::default_memory_prune_threshold")]
    pub memory_prune_threshold: f32,

    /// Effective decay score below which a memory is archived (hidden).
    #[serde(default = "Settings::default_memory_archive_threshold")]
    pub memory_archive_threshold: f32,

    #[serde(default = "Settings::default_memory_cleanup_interval_hours")]
    pub memory_cleanup_interval_hours: u32,

    #[serde(default = "Settings::default_memory_consolidation_interval_hours")]
    pub memory_consolidation_interval_hours: u32,

    #[serde(default = "Settings::default_memory_consolidation_batch_size")]
    pub memory_consolidation_batch_size: u32,

    /// Max facts extracted per conversation turn.
    #[serde(default = "Settings::default_memory_extraction_max_facts")]
    pub memory_extraction_max_facts: u32,

    /// Minimum seconds between extraction runs (rate limit).
    #[serde(default = "Settings::default_memory_extraction_interval_secs")]
    pub memory_extraction_interval_secs: u32,

    // ── Scheduling tuning ────────────────────────────────────────────────────
    #[serde(default = "Settings::default_schedule_max_concurrent")]
    pub schedule_max_concurrent: u32,

    /// Max execution-history entries retained per schedule.
    #[serde(default = "Settings::default_schedule_max_runs_per_task")]
    pub schedule_max_runs_per_task: u32,

    // ── Context monitoring ─────────────────────────────────────────────────
    /// Track per-session context fill and warn before the window saturates.
    #[serde(default = "Settings::default_context_monitor_enabled")]
    pub context_monitor_enabled: bool,

    /// Asks the model, once it stops calling tools, whether the request was met (~2x inferences).
    /// Not a `ModelClass` tier: that would switch it off on-device, where it helps most.
    #[serde(default = "Settings::default_goal_check_enabled")]
    pub goal_check_enabled: bool,

    // ── Cost comparison ──────────────────────────────────────────────────────
    /// Cloud API input token price per million (for savings calculation). Default 2.50 (GPT-4o).
    #[serde(default = "Settings::default_cloud_input_price_per_million")]
    pub cloud_input_price_per_million: f64,

    /// Cloud API output token price per million. Default 10.00 (GPT-4o).
    #[serde(default = "Settings::default_cloud_output_price_per_million")]
    pub cloud_output_price_per_million: f64,

    // ── Telemetry ─────────────────────────────────────────────────────────
    /// Record per-turn metrics (TTFT, token counts, tool latency, context use).
    #[serde(default = "Settings::default_telemetry_enabled")]
    pub telemetry_enabled: bool,

    // ── Experimental ────────────────────────────────────────────────────────
    /// Experimental: ToolAgent dispatches several tool intents per message concurrently.
    #[serde(default)]
    pub multi_tool_enabled: bool,
    // ── Tool call validation ────────────────────────────────────────────────
    /// Repair malformed tool-call JSON (common from 3B-4B local models) before execution.
    #[serde(default = "Settings::default_tool_call_validation")]
    pub tool_call_validation: bool,

    // ── Post-inference tool request detection ──────────────────────────
    /// Scan replies for prose tool requests ("Let me look up X"), run the tool and revise.
    #[serde(default = "Settings::default_tool_request_detection")]
    pub tool_request_detection: bool,

    // ── API keys: NOT HERE, deliberately (PAI-2 P2) ──────────────────────
    // GET /api/v1/settings returns this struct whole, so credentials go in `SecretRepository`.
    /// Self-hosted SearXNG URL; an endpoint, not a credential (`user:pass@` belongs in secrets).
    #[serde(default)]
    pub searxng_url: Option<String>,

    // ── Extension toggles ───────────────────────────────────────────────
    // Builtin MCP tool modules registered at startup.
    /// Enable the memory tools module (recall, save, forget).
    #[serde(default = "Settings::default_ext_enabled")]
    pub ext_memory_enabled: bool,

    /// Enable the scheduling tools module (create, delete, pause, resume, list, run_now, get_runs).
    #[serde(default = "Settings::default_ext_enabled")]
    pub ext_schedule_enabled: bool,

    #[serde(default = "Settings::default_ext_enabled")]
    pub ext_weather_enabled: bool,

    /// Enable the knowledge tools module (Wikipedia search, article fetch).
    #[serde(default = "Settings::default_ext_enabled")]
    pub ext_knowledge_enabled: bool,

    /// Enable the system tools module (shell, files, system info, notifications).
    #[serde(default = "Settings::default_ext_enabled")]
    pub ext_system_enabled: bool,

    /// Enable the device/profile tools module (devices, profile, model config).
    #[serde(default = "Settings::default_ext_enabled")]
    pub ext_device_enabled: bool,

    /// Enable the sensor tools module (query stored IoT sensor readings).
    #[serde(default = "Settings::default_ext_enabled")]
    pub ext_sensor_enabled: bool,

    /// Enables `delegate`, which runs a saved role as a child agent with no approval path.
    /// Defaults off via its own fn: the shared `default_ext_enabled` would enable it on upgrade.
    #[serde(default = "Settings::default_ext_orchestrator_enabled")]
    pub ext_orchestrator_enabled: bool,

    /// May the pond speak without having been spoken to?
    #[serde(default = "Settings::default_unprompted_speech_enabled")]
    pub unprompted_speech_enabled: bool,

    /// Start of quiet hours (no unprompted speech), local `"HH:MM"`; wraps midnight if after end.
    /// Malformed bounds mean silence, never "no quiet hours" (see `chat::quiet_hours_cover`).
    #[serde(default = "Settings::default_quiet_hours_start")]
    pub quiet_hours_start: String,

    /// End of quiet hours, local `"HH:MM"`; see [`Settings::quiet_hours_start`].
    #[serde(default = "Settings::default_quiet_hours_end")]
    pub quiet_hours_end: String,

    /// Comma-separated `Notification.category` values that may be spoken; typos match nothing.
    #[serde(default = "Settings::default_unprompted_speech_categories")]
    pub unprompted_speech_categories: String,

    /// May the pond start a turn of its own to review what has happened?
    /// Also needs [`Settings::ext_orchestrator_enabled`], which does not imply it.
    #[serde(default = "Settings::default_proactive_review_enabled")]
    pub proactive_review_enabled: bool,

    /// May sensor and camera events become per-member context items the model is shown?
    /// Off stops ingest only; items already ingested stay recallable.
    #[serde(default = "Settings::default_context_ingest_enabled")]
    pub context_ingest_enabled: bool,

    /// Offers the model `search_context` and `get_recent_context` over the context corpus.
    /// Off by default: two more schemas in every prompt, useless until a source is connected.
    #[serde(default = "Settings::default_ext_context_enabled")]
    pub ext_context_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            primary_profile_id: None,
            assistant_name: Self::default_assistant_name(),
            assistant_personality: Self::default_assistant_personality(),
            user_name: Self::default_user_name(),
            timezone: Self::default_timezone(),
            home_name: Self::default_home_name(),
            prompt_style: Self::default_prompt_style(),
            custom_system_prompt: None,
            prompt_addendum: Self::default_prompt_addendum(),
            chat_provider: Self::default_llm_provider(),
            chat_model: Self::default_active_llm_model(),
            tool_model: None,
            llm_max_tokens: Self::default_max_tokens(),
            llm_temperature: Self::default_temperature(),
            llm_provider: Self::default_llm_provider(),
            voice_wake_word: Self::default_wake_word(),
            voice_kws_whisper_url: None,
            voice_kws_energy_threshold: Self::default_kws_energy_threshold(),
            voice_kws_post_trigger_silence_ms: Self::default_kws_post_trigger_silence_ms(),
            voice_kws_cooldown_ms: Self::default_kws_cooldown_ms(),
            voice_wake_word_transcriptions: Vec::new(),
            voice_tts_voice: Self::default_tts_voice(),
            voice_tts_speed: Self::default_tts_speed(),
            voice_tts_quality: Self::default_tts_quality(),
            vad_backend: Self::default_vad_backend(),
            voice_thinking_tone_enabled: Self::default_voice_thinking_tone_enabled(),
            voice_recording_duration_secs: Self::default_recording_duration(),
            voice_whisper_url: Self::default_whisper_url(),
            active_llm_model: Self::default_active_llm_model(),
            active_whisper_model: Self::default_active_whisper_model(),
            active_tts_model: Self::default_active_tts_model(),
            active_embedding_model: String::new(),
            embedding_provider: Self::default_embedding_provider(),
            weather_enabled: Self::default_weather_enabled(),
            weather_latitude: Self::default_weather_latitude(),
            weather_longitude: Self::default_weather_longitude(),
            weather_location_name: Self::default_weather_location_name(),
            vision_enabled: Self::default_vision_enabled(),
            vision_camera_url: Self::default_vision_camera_url(),
            vision_camera_id: Self::default_vision_camera_id(),
            vision_fps: Self::default_vision_fps(),
            vision_motion_threshold: Self::default_vision_motion_threshold(),
            vision_classifier_model: Self::default_vision_classifier_model(),
            matter_ws_url: Self::default_matter_ws_url(),
            matter_ble_enabled: Self::default_matter_ble_enabled(),
            mesh_enabled: Self::default_mesh_enabled(),
            lightning_enabled: Self::default_lightning_enabled(),
            mesh_settlement_millisats_per_token: Self::default_mesh_settlement_millisats_per_token(
            ),
            mesh_lend_token_ceiling: Self::default_mesh_lend_token_ceiling(),
            mic_enabled: Self::default_mic_enabled(),
            cameras_enabled: Self::default_cameras_enabled(),
            cloud_fallback_enabled: Self::default_cloud_fallback_enabled(),
            retention_event_log_days: Self::default_event_log_days(),
            retention_sensor_days: Self::default_sensor_days(),
            retention_session_messages_keep: Self::default_session_messages_keep(),
            retention_events_days: Self::default_events_days(),
            retention_events_by_category: std::collections::HashMap::new(),
            retention_sensitive_days: Self::default_sensitive_days(),
            thinking_mode: Self::default_thinking_mode(),
            show_thinking: false,
            reasoning_effort: Self::default_reasoning_effort(),
            persist_thinking: false,
            review_mode: Self::default_review_mode(),
            review_max_rounds: Self::default_review_max_rounds(),
            review_pass_threshold: Self::default_review_pass_threshold(),
            context_window_override: 0,
            show_turn_stats: false,
            hybrid_compaction_enabled: Self::default_hybrid_compaction_enabled(),
            summary_idle_secs: Self::default_summary_idle_secs(),
            compaction_verbatim_days: Self::default_compaction_verbatim_days(),
            agent_backend: Self::default_agent_backend(),
            agent_goose_mode: Self::default_agent_goose_mode(),
            agent_max_turns: Self::default_agent_max_turns(),
            voice_max_turns: Self::default_voice_max_turns(),
            agent_timeout_secs: Self::default_agent_timeout_secs(),
            prefix_cache_prompt: Self::default_prefix_cache_prompt(),
            tool_selection_mode: Self::default_tool_selection_mode(),
            security_policy_mode: Self::default_security_policy_mode(),
            network_mode: Self::default_network_mode(),
            agent_memory_inject: Self::default_agent_memory_inject(),
            agent_memory_limit: Self::default_agent_memory_limit(),
            tool_output_compaction: Self::default_tool_output_compaction(),
            memory_extraction_enabled: true,
            memory_cleanup_enabled: true,
            memory_consolidation_enabled: Self::default_memory_consolidation_enabled(),
            session_titling_enabled: Self::default_session_titling_enabled(),
            memory_consolidation_mode: Self::default_memory_consolidation_mode(),
            memory_graph_enabled: false,
            schedule_result_notify: Self::default_schedule_result_notify(),
            memory_decay_base_half_life_days: Self::default_memory_decay_base_half_life_days(),
            memory_decay_beta: Self::default_memory_decay_beta(),
            memory_prune_threshold: Self::default_memory_prune_threshold(),
            memory_archive_threshold: Self::default_memory_archive_threshold(),
            memory_cleanup_interval_hours: Self::default_memory_cleanup_interval_hours(),
            memory_consolidation_interval_hours: Self::default_memory_consolidation_interval_hours(
            ),
            memory_consolidation_batch_size: Self::default_memory_consolidation_batch_size(),
            memory_extraction_max_facts: Self::default_memory_extraction_max_facts(),
            memory_extraction_interval_secs: Self::default_memory_extraction_interval_secs(),
            schedule_max_concurrent: Self::default_schedule_max_concurrent(),
            schedule_max_runs_per_task: Self::default_schedule_max_runs_per_task(),
            context_monitor_enabled: Self::default_context_monitor_enabled(),
            goal_check_enabled: Self::default_goal_check_enabled(),
            cloud_input_price_per_million: Self::default_cloud_input_price_per_million(),
            cloud_output_price_per_million: Self::default_cloud_output_price_per_million(),
            telemetry_enabled: Self::default_telemetry_enabled(),
            multi_tool_enabled: false,
            tool_call_validation: Self::default_tool_call_validation(),
            tool_request_detection: Self::default_tool_request_detection(),
            searxng_url: None,
            ext_memory_enabled: true,
            ext_schedule_enabled: true,
            ext_weather_enabled: true,
            ext_knowledge_enabled: true,
            ext_system_enabled: true,
            ext_device_enabled: true,
            ext_sensor_enabled: true,
            // Must stay `false`, not `Self::default_ext_enabled()`.
            ext_orchestrator_enabled: false,
            unprompted_speech_enabled: false,
            quiet_hours_start: Self::default_quiet_hours_start(),
            quiet_hours_end: Self::default_quiet_hours_end(),
            unprompted_speech_categories: Self::default_unprompted_speech_categories(),
            proactive_review_enabled: false,
            context_ingest_enabled: false,
            ext_context_enabled: false,
        }
    }
}

impl Settings {
    fn default_prompt_style() -> String {
        "balanced".to_string()
    }
    fn default_prompt_addendum() -> String {
        "".to_string()
    }
    fn default_assistant_name() -> String {
        "Goose".to_string()
    }
    fn default_assistant_personality() -> String {
        "friendly and concise".to_string()
    }
    fn default_user_name() -> String {
        "Friend".to_string()
    }
    fn default_timezone() -> String {
        "UTC".to_string()
    }
    fn default_home_name() -> String {
        "".to_string()
    }
    // Harmony models (Gemma 4, gpt-oss) spend hundreds of tokens before the visible reply.
    fn default_max_tokens() -> u32 {
        4096
    }
    fn default_temperature() -> f32 {
        0.7
    }
    fn default_llm_provider() -> String {
        "".to_string()
    }
    fn default_wake_word() -> String {
        "goose".to_string()
    }
    fn default_kws_energy_threshold() -> f32 {
        0.003
    }
    fn default_kws_post_trigger_silence_ms() -> u64 {
        400
    }
    fn default_kws_cooldown_ms() -> u64 {
        2000
    }
    fn default_tts_voice() -> String {
        "".to_string()
    }
    fn default_tts_speed() -> f32 {
        1.0
    }
    fn default_tts_quality() -> String {
        "q8".to_string()
    }
    /// A one-time 2 MB download beats shipping a detector that can't tell a fridge from a voice.
    fn default_vad_backend() -> String {
        "silero".to_string()
    }
    fn default_voice_thinking_tone_enabled() -> bool {
        true
    }
    fn default_recording_duration() -> u32 {
        3
    }
    fn default_whisper_url() -> String {
        "http://127.0.0.1:9000".to_string()
    }
    fn default_active_llm_model() -> String {
        "".to_string()
    }
    fn default_active_whisper_model() -> String {
        "".to_string()
    }
    fn default_active_tts_model() -> String {
        "".to_string()
    }
    fn default_embedding_provider() -> String {
        "fastembed".to_string()
    }
    fn default_weather_enabled() -> bool {
        false
    }
    fn default_weather_latitude() -> f64 {
        0.0
    }
    fn default_weather_longitude() -> f64 {
        0.0
    }
    fn default_weather_location_name() -> String {
        "".to_string()
    }
    fn default_vision_enabled() -> bool {
        false
    }
    fn default_vision_camera_url() -> String {
        "".to_string()
    }
    fn default_vision_camera_id() -> String {
        "camera-1".to_string()
    }
    fn default_vision_fps() -> u32 {
        2
    }
    fn default_vision_motion_threshold() -> f64 {
        0.05
    }
    fn default_vision_classifier_model() -> String {
        "".to_string()
    }
    fn default_matter_ws_url() -> String {
        DEFAULT_MATTER_WS_URL.to_string()
    }
    fn default_matter_ble_enabled() -> bool {
        false
    }
    fn default_mesh_enabled() -> bool {
        false
    }
    fn default_lightning_enabled() -> bool {
        false
    }
    fn default_mesh_settlement_millisats_per_token() -> u64 {
        0
    }
    fn default_mesh_lend_token_ceiling() -> u64 {
        0
    }
    fn default_mic_enabled() -> bool {
        true
    }
    fn default_cameras_enabled() -> bool {
        true
    }
    fn default_cloud_fallback_enabled() -> bool {
        false
    }
    fn default_event_log_days() -> u32 {
        30
    }
    fn default_sensor_days() -> u32 {
        7
    }
    fn default_session_messages_keep() -> u32 {
        500
    }
    fn default_events_days() -> u32 {
        30
    }
    fn default_sensitive_days() -> u32 {
        7
    }
    fn default_thinking_mode() -> String {
        "auto".to_string()
    }
    fn default_reasoning_effort() -> String {
        "brief".to_string()
    }
    fn default_review_mode() -> String {
        "off".to_string()
    }
    fn default_review_max_rounds() -> u32 {
        1
    }
    fn default_review_pass_threshold() -> u8 {
        3
    }
    fn default_summary_idle_secs() -> u32 {
        120
    }

    fn default_compaction_verbatim_days() -> u32 {
        crate::models::services::context::turn_trimmer::DEFAULT_VERBATIM_DAYS
    }

    fn default_hybrid_compaction_enabled() -> bool {
        true
    }

    fn default_agent_backend() -> String {
        "goose".to_string()
    }
    fn default_agent_goose_mode() -> String {
        "auto".to_string()
    }
    /// Room for multi-step requests; cancellation, timeout and context abort are the real rails.
    fn default_agent_max_turns() -> u32 {
        50
    }
    /// No voice cap: idle timeout, thinking tone and barge-in bound the wait better than steps.
    fn default_voice_max_turns() -> u32 {
        0
    }

    /// Turn cap for a request; a non-zero voice cap binds even when text is uncapped.
    pub fn effective_max_turns(&self, voice: bool) -> u32 {
        if voice && self.voice_max_turns > 0 {
            if self.agent_max_turns == 0 {
                self.voice_max_turns
            } else {
                self.voice_max_turns.min(self.agent_max_turns)
            }
        } else if self.agent_max_turns == 0 {
            UNCAPPED_MAX_TURNS
        } else {
            self.agent_max_turns
        }
    }

    /// Whether the budget is the uncapped sentinel, which must not be quoted as a step count.
    pub fn turns_are_uncapped(&self, voice: bool) -> bool {
        self.effective_max_turns(voice) == UNCAPPED_MAX_TURNS
    }

    /// Whether `relevant` selection is on; only the exact string counts, so a typo never narrows.
    pub fn tool_selection_is_relevant(&self) -> bool {
        self.tool_selection_mode == TOOL_SELECTION_MODE_RELEVANT
    }

    /// Whether only the toolkit escape hatch is offered; exact match only, as for `relevant`.
    pub fn tool_selection_is_minimal(&self) -> bool {
        self.tool_selection_mode == TOOL_SELECTION_MODE_MINIMAL
    }

    /// Whether any narrowing is on: the gate for the per-session tool-group machinery.
    pub fn tool_selection_narrows(&self) -> bool {
        self.tool_selection_is_relevant() || self.tool_selection_is_minimal()
    }
    fn default_agent_timeout_secs() -> u64 {
        300
    }
    fn default_tool_selection_mode() -> String {
        // "all" so existing installs see no change.
        TOOL_SELECTION_MODE_ALL.to_string()
    }
    fn default_security_policy_mode() -> String {
        SECURITY_POLICY_MODE_AUDIT.to_string()
    }
    fn default_network_mode() -> String {
        NETWORK_MODE_OPEN.to_string()
    }
    fn default_prefix_cache_prompt() -> bool {
        true
    }
    fn default_agent_memory_inject() -> bool {
        true
    }
    fn default_agent_memory_limit() -> u32 {
        5
    }
    fn default_schedule_result_notify() -> bool {
        true
    }
    fn default_memory_decay_base_half_life_days() -> f32 {
        11.25
    }
    fn default_memory_decay_beta() -> f32 {
        0.8
    }
    fn default_memory_prune_threshold() -> f32 {
        0.05
    }
    fn default_memory_archive_threshold() -> f32 {
        0.15
    }
    fn default_tool_output_compaction() -> bool {
        true
    }
    fn default_memory_extraction_enabled() -> bool {
        true
    }
    fn default_memory_cleanup_enabled() -> bool {
        true
    }
    fn default_memory_consolidation_enabled() -> bool {
        true
    }
    fn default_session_titling_enabled() -> bool {
        true
    }
    fn default_memory_consolidation_mode() -> String {
        "single".to_string()
    }
    fn default_memory_cleanup_interval_hours() -> u32 {
        6
    }
    fn default_memory_consolidation_interval_hours() -> u32 {
        24
    }
    fn default_memory_consolidation_batch_size() -> u32 {
        50
    }
    fn default_memory_extraction_max_facts() -> u32 {
        3
    }
    fn default_memory_extraction_interval_secs() -> u32 {
        10
    }
    fn default_schedule_max_concurrent() -> u32 {
        2
    }
    fn default_schedule_max_runs_per_task() -> u32 {
        50
    }
    fn default_goal_check_enabled() -> bool {
        true
    }
    fn default_context_monitor_enabled() -> bool {
        true
    }
    fn default_cloud_input_price_per_million() -> f64 {
        2.50
    }
    fn default_cloud_output_price_per_million() -> f64 {
        10.00
    }
    fn default_telemetry_enabled() -> bool {
        true
    }
    fn default_tool_call_validation() -> bool {
        true
    }
    fn default_tool_request_detection() -> bool {
        true
    }
    fn default_ext_enabled() -> bool {
        true
    }

    /// Deliberately not [`Self::default_ext_enabled`]; named so a test can assert it is `false`.
    fn default_ext_orchestrator_enabled() -> bool {
        false
    }

    fn default_unprompted_speech_enabled() -> bool {
        false
    }

    /// Set by default, so switching speech on does not also require remembering quiet hours.
    fn default_quiet_hours_start() -> String {
        "22:00".to_string()
    }

    fn default_quiet_hours_end() -> String {
        "07:00".to_string()
    }

    /// Not `info`: it carries every completed scheduled task, i.e. the pond's own cron log.
    fn default_unprompted_speech_categories() -> String {
        "alert".to_string()
    }

    fn default_proactive_review_enabled() -> bool {
        false
    }

    fn default_context_ingest_enabled() -> bool {
        false
    }

    fn default_ext_context_enabled() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checks an adoption registry's structure, returning every problem so each rule is testable.
    /// The newest-literal rule is pond-infra's: `serde_json` renders floats unlike the adapter.
    fn validate_adoptions(entries: &[DefaultAdoption], defaults: &Settings) -> Vec<String> {
        let value = serde_json::to_value(defaults).expect("serialize Settings");
        let obj = value.as_object().expect("Settings is a JSON object");
        let mut problems = Vec::new();
        // Last entry seen for each key.
        let mut previous: std::collections::BTreeMap<&str, &DefaultAdoption> =
            std::collections::BTreeMap::new();

        for entry in entries {
            if !obj.contains_key(entry.key) {
                problems.push(format!(
                    "DEFAULT_ADOPTIONS names `{}`, which is not a Settings field",
                    entry.key
                ));
                continue;
            }
            if entry.old_default == entry.new_default {
                problems.push(format!(
                    "`{}` adoption ({}) is a no-op — old and new defaults are identical. \
                     A default that did not move needs no entry.",
                    entry.key, entry.migration
                ));
            }
            if let Some(prev) = previous.get(entry.key) {
                if prev.migration >= entry.migration {
                    problems.push(format!(
                        "`{}` adoptions are out of order: migration {} is listed before {}. \
                         DEFAULT_ADOPTIONS is oldest-first.",
                        entry.key, prev.migration, entry.migration
                    ));
                }
                if prev.new_default != entry.old_default {
                    problems.push(format!(
                        "`{}` adoptions do not chain: migration {} leaves the stored value \
                         at `{}`, but migration {} only fires on `{}`, so every install that \
                         already ran {} is skipped. Set old_default to `{}`.",
                        entry.key,
                        prev.migration,
                        prev.new_default,
                        entry.migration,
                        entry.old_default,
                        prev.migration,
                        prev.new_default
                    ));
                }
            }
            previous.insert(entry.key, entry);
        }

        problems
    }

    #[test]
    fn default_adoptions_are_structurally_sound() {
        let problems = validate_adoptions(DEFAULT_ADOPTIONS, &Settings::default());
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn a_chained_adoption_is_accepted() {
        let defaults = Settings::default();
        let chained = [
            DefaultAdoption {
                key: "agent_max_turns",
                old_default: "20",
                new_default: "50",
                migration: "0035",
            },
            DefaultAdoption {
                key: "agent_max_turns",
                old_default: "50",
                new_default: "80",
                migration: "0041",
            },
        ];
        assert!(
            validate_adoptions(&chained, &defaults).is_empty(),
            "chaining a second change to the same key must be expressible"
        );
    }

    #[test]
    fn adoption_defects_are_rejected() {
        let defaults = Settings::default();

        // Restating the ORIGINAL old value skips installs that already ran the first migration.
        let problems = validate_adoptions(
            &[
                DefaultAdoption {
                    key: "agent_max_turns",
                    old_default: "20",
                    new_default: "50",
                    migration: "0035",
                },
                DefaultAdoption {
                    key: "agent_max_turns",
                    old_default: "20",
                    new_default: "80",
                    migration: "0041",
                },
            ],
            &defaults,
        );
        assert!(
            problems.iter().any(|p| p.contains("do not chain")),
            "a broken chain must be reported, got {problems:?}"
        );

        // Not a Settings field at all.
        let problems = validate_adoptions(
            &[DefaultAdoption {
                key: "not_a_setting",
                old_default: "a",
                new_default: "b",
                migration: "0035",
            }],
            &defaults,
        );
        assert!(
            problems.iter().any(|p| p.contains("not a Settings field")),
            "an unknown key must be reported, got {problems:?}"
        );

        // No-op entry.
        let problems = validate_adoptions(
            &[DefaultAdoption {
                key: "tool_selection_mode",
                old_default: TOOL_SELECTION_MODE_ALL,
                new_default: TOOL_SELECTION_MODE_ALL,
                migration: "0035",
            }],
            &defaults,
        );
        assert!(
            problems.iter().any(|p| p.contains("no-op")),
            "an entry whose default never moved must be reported, got {problems:?}"
        );

        // Descending migration order.
        let problems = validate_adoptions(
            &[
                DefaultAdoption {
                    key: "agent_max_turns",
                    old_default: "20",
                    new_default: "50",
                    migration: "0041",
                },
                DefaultAdoption {
                    key: "agent_max_turns",
                    old_default: "50",
                    new_default: "50",
                    migration: "0035",
                },
            ],
            &defaults,
        );
        assert!(
            problems.iter().any(|p| p.contains("out of order")),
            "descending migration order must be reported, got {problems:?}"
        );
    }

    #[test]
    fn default_settings_have_expected_values() {
        let s = Settings::default();
        assert_eq!(s.assistant_name, "Goose");
        assert_eq!(s.llm_max_tokens, 4096);
        assert_eq!(s.llm_temperature, 0.7);
        assert_eq!(s.voice_wake_word, "goose");
        assert_eq!(s.retention_event_log_days, 30);
        assert!(s.tool_call_validation);
    }

    #[test]
    fn settings_roundtrip_via_json() {
        let mut s = Settings::default();
        s.assistant_name = "Duck".to_string();
        s.llm_max_tokens = 2048;
        let json = serde_json::to_string(&s).unwrap();
        let s2: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(s2.assistant_name, "Duck");
        assert_eq!(s2.llm_max_tokens, 2048);
        assert_eq!(s2.llm_temperature, 0.7);
    }

    #[test]
    fn default_prompt_style_is_balanced() {
        let s = Settings::default();
        assert_eq!(s.prompt_style, "balanced");
        assert!(s.custom_system_prompt.is_none());
        assert_eq!(s.prompt_addendum, "");
    }

    #[test]
    fn partial_json_with_prompt_fields() {
        let json = r#"{"prompt_style":"concise","prompt_addendum":"Be brief."}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.prompt_style, "concise");
        assert_eq!(s.prompt_addendum, "Be brief.");
        assert!(s.custom_system_prompt.is_none());
    }

    #[test]
    fn custom_system_prompt_roundtrips() {
        let json = r#"{"custom_system_prompt":"You are {{assistant_name}}."}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.custom_system_prompt,
            Some("You are {{assistant_name}}.".to_string())
        );
        let json2 = serde_json::to_string(&s).unwrap();
        let s2: Settings = serde_json::from_str(&json2).unwrap();
        assert_eq!(s2.custom_system_prompt, s.custom_system_prompt);
    }

    #[test]
    fn partial_json_fills_missing_with_defaults() {
        let json = r#"{"assistant_name": "Pond"}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.assistant_name, "Pond");
        assert_eq!(s.llm_max_tokens, 4096);
        assert_eq!(s.timezone, "UTC");
    }

    #[test]
    fn tool_call_validation_toggleable() {
        let json = r#"{"tool_call_validation": false}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(!s.tool_call_validation);
        assert!(s.memory_extraction_enabled);
    }

    #[test]
    fn extension_toggles_default_to_true() {
        let s = Settings::default();
        assert!(s.ext_memory_enabled);
        assert!(s.ext_schedule_enabled);
        assert!(s.ext_weather_enabled);
        assert!(s.ext_knowledge_enabled);
        assert!(s.ext_system_enabled);
        assert!(s.ext_device_enabled);
    }

    #[test]
    fn extension_toggles_deserialize_from_partial_json() {
        let json = r#"{"ext_memory_enabled": false, "ext_weather_enabled": false}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(!s.ext_memory_enabled);
        assert!(!s.ext_weather_enabled);
        assert!(s.ext_schedule_enabled);
        assert!(s.ext_knowledge_enabled);
        assert!(s.ext_system_enabled);
        assert!(s.ext_device_enabled);
    }

    #[test]
    fn searxng_url_defaults_to_none() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(s.searxng_url.is_none());
    }

    /// Old clients still send `api_key_guardian`; a 422 would break every save they make.
    #[test]
    fn a_legacy_api_key_field_is_ignored_not_fatal() {
        let json = r#"{"api_key_guardian": "legacy-key", "searxng_url": "http://localhost:8888"}"#;
        let s: Settings =
            serde_json::from_str(json).expect("unknown fields must not fail the save");
        assert_eq!(s.searxng_url, Some("http://localhost:8888".to_string()));
        let round_tripped = serde_json::to_value(&s).unwrap();
        assert!(
            round_tripped.get("api_key_guardian").is_none(),
            "a legacy key must not survive a deserialize/serialize round trip"
        );
    }

    #[test]
    fn privacy_and_home_fields_default_correctly() {
        let s = Settings::default();
        assert!(s.mic_enabled);
        assert!(s.cameras_enabled);
        assert!(!s.cloud_fallback_enabled);
        assert_eq!(s.home_name, "");
    }

    #[test]
    fn privacy_and_home_fields_roundtrip_via_json() {
        let mut s = Settings::default();
        s.mic_enabled = false;
        s.cameras_enabled = false;
        s.cloud_fallback_enabled = true;
        s.home_name = "The Anyumba Home".to_string();
        let json = serde_json::to_string(&s).unwrap();
        let s2: Settings = serde_json::from_str(&json).unwrap();
        assert!(!s2.mic_enabled);
        assert!(!s2.cameras_enabled);
        assert!(s2.cloud_fallback_enabled);
        assert_eq!(s2.home_name, "The Anyumba Home");
    }

    #[test]
    fn privacy_fields_deserialize_from_partial_json() {
        // Simulates the TS client sending only the privacy toggles on PUT.
        let json = r#"{"mic_enabled": false, "home_name": "Home"}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(!s.mic_enabled);
        assert_eq!(s.home_name, "Home");
        assert!(s.cameras_enabled);
        assert!(!s.cloud_fallback_enabled);
    }

    #[test]
    fn partial_overrides_preserve_new_defaults() {
        let json = r#"{"ext_weather_enabled": false}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(!s.ext_weather_enabled);
        assert!(s.ext_memory_enabled);
        assert!(s.ext_schedule_enabled);
        assert!(s.searxng_url.is_none());
    }

    #[test]
    fn a_spoken_request_gets_the_same_budget_as_a_typed_one() {
        let s = Settings::default();
        assert_eq!(
            s.effective_max_turns(false),
            50,
            "text uses agent_max_turns"
        );
        assert_eq!(
            s.effective_max_turns(true),
            s.effective_max_turns(false),
            "voice must not be quietly given a smaller budget than text"
        );
    }

    #[test]
    fn a_configured_voice_cap_still_binds() {
        let mut s = Settings::default();
        s.voice_max_turns = 8;
        assert_eq!(s.effective_max_turns(true), 8);
        assert_eq!(s.effective_max_turns(false), 50, "text is unaffected");
    }

    /// The engine gets the sentinel, not 0, which would end the loop before its first turn.
    #[test]
    fn zero_agent_max_turns_means_uncapped() {
        let mut s = Settings::default();
        s.agent_max_turns = 0;
        assert_eq!(s.effective_max_turns(false), UNCAPPED_MAX_TURNS);
        assert!(s.turns_are_uncapped(false));
        assert!(UNCAPPED_MAX_TURNS.checked_mul(2).is_some());
    }

    #[test]
    fn uncapped_text_budget_still_honours_the_voice_cap() {
        let mut s = Settings::default();
        s.agent_max_turns = 0;
        s.voice_max_turns = 8;
        assert_eq!(s.effective_max_turns(true), 8);
        assert!(!s.turns_are_uncapped(true));
        // Both zero: voice inherits the uncapped text budget.
        s.voice_max_turns = 0;
        assert_eq!(s.effective_max_turns(true), UNCAPPED_MAX_TURNS);
        assert!(s.turns_are_uncapped(true));
    }

    #[test]
    fn tool_selection_defaults_to_all_and_only_exact_opt_in_narrows() {
        let mut s = Settings::default();
        assert_eq!(s.tool_selection_mode, TOOL_SELECTION_MODE_ALL);
        assert!(!s.tool_selection_is_relevant());

        s.tool_selection_mode = TOOL_SELECTION_MODE_RELEVANT.to_string();
        assert!(s.tool_selection_is_relevant());

        for bogus in ["Relevant", "relevent", "semantic", "", "true"] {
            s.tool_selection_mode = bogus.to_string();
            assert!(
                !s.tool_selection_is_relevant(),
                "'{bogus}' must not enable narrowing"
            );
        }
    }

    #[test]
    fn minimal_narrows_and_only_the_exact_string_does() {
        let mut s = Settings::default();
        assert!(!s.tool_selection_is_minimal(), "the default is not minimal");
        assert!(!s.tool_selection_narrows(), "the default must not narrow");

        s.tool_selection_mode = TOOL_SELECTION_MODE_MINIMAL.to_string();
        assert!(s.tool_selection_is_minimal());
        assert!(s.tool_selection_narrows(), "minimal is a narrowing mode");
        assert!(
            !s.tool_selection_is_relevant(),
            "minimal must not read as relevant -- they take different paths"
        );

        s.tool_selection_mode = TOOL_SELECTION_MODE_RELEVANT.to_string();
        assert!(s.tool_selection_narrows(), "relevant is a narrowing mode");
        assert!(!s.tool_selection_is_minimal());

        for bogus in ["Minimal", "minimum", "none", "hatch", "", "true"] {
            s.tool_selection_mode = bogus.to_string();
            assert!(
                !s.tool_selection_is_minimal() && !s.tool_selection_narrows(),
                "'{bogus}' must not narrow anything"
            );
        }
    }

    #[test]
    fn every_tool_selection_mode_is_a_mode_some_predicate_recognises() {
        for mode in TOOL_SELECTION_MODES {
            let mut s = Settings::default();
            s.tool_selection_mode = (*mode).to_string();
            let recognised = *mode == TOOL_SELECTION_MODE_ALL
                || s.tool_selection_is_relevant()
                || s.tool_selection_is_minimal();
            assert!(
                recognised,
                "'{mode}' is in TOOL_SELECTION_MODES, so PUT /settings accepts it, but no \
                 predicate recognises it -- it would store and then behave as \"all\""
            );
        }
    }

    #[test]
    fn a_real_cap_is_not_uncapped() {
        let s = Settings::default();
        assert!(!s.turns_are_uncapped(false));
        assert!(!s.turns_are_uncapped(true));
    }

    #[test]
    fn effective_max_turns_never_exceeds_agent_max_turns() {
        let mut s = Settings::default();
        s.agent_max_turns = 5;
        s.voice_max_turns = 50;
        assert_eq!(s.effective_max_turns(true), 5);
    }

    #[test]
    fn effective_max_turns_zero_disables_voice_cap() {
        let mut s = Settings::default();
        s.voice_max_turns = 0;
        assert_eq!(s.effective_max_turns(true), s.agent_max_turns);
    }

    /// This file's source, so guards can compare DECLARED fields against SERIALIZED ones.
    const SETTINGS_SOURCE: &str = include_str!("settings.rs");

    /// Credential words, matched per `_` segment: a suffix test misses `api_key_guardian`.
    const SECRET_WORDS: &[&str] = &[
        "key",
        "keys",
        "token",
        "tokens",
        "secret",
        "secrets",
        "password",
        "passwords",
        "credential",
        "credentials",
        "apikey",
        "passphrase",
    ];

    fn is_secret_shaped(field: &str) -> bool {
        field.split('_').any(|seg| SECRET_WORDS.contains(&seg))
    }

    /// Keys with secret-shaped names but non-credential values; keep this list very short.
    const NOT_ACTUALLY_SECRET: &[&str] = &[
        // A token BUDGET (a `u32`), not a bearer token.
        "llm_max_tokens",
        // An exchange RATE (millisats per usage-token), not a bearer token.
        "mesh_settlement_millisats_per_token",
        // A token COUNT ceiling, not a bearer token.
        "mesh_lend_token_ceiling",
    ];

    #[test]
    fn no_settings_field_is_secret_shaped() {
        // Positive control first: a detector that matches nothing would pass vacuously.
        assert!(
            is_secret_shaped("api_key_guardian"),
            "the detector must flag the shape that actually leaked"
        );
        assert!(is_secret_shaped("gmail_refresh_token"));
        assert!(is_secret_shaped("db_password"));
        assert!(is_secret_shaped("client_secret"));
        assert!(!is_secret_shaped("home_name"));
        assert!(!is_secret_shaped("voice_wake_word"));
        assert!(!is_secret_shaped("searxng_url"));

        let value = serde_json::to_value(Settings::default()).expect("serialize Settings");
        let keys: Vec<String> = value
            .as_object()
            .expect("Settings serializes to a JSON object")
            .keys()
            .cloned()
            .collect();

        // No stale exemptions, and no useless ones.
        for exempt in NOT_ACTUALLY_SECRET {
            assert!(
                keys.iter().any(|k| k == exempt),
                "exempt field `{exempt}` is not a real Settings field (stale entry — remove it)"
            );
            assert!(
                is_secret_shaped(exempt),
                "field `{exempt}` is not secret-shaped, so exempting it is noise — remove it"
            );
        }

        let offenders: Vec<&String> = keys
            .iter()
            .filter(|k| is_secret_shaped(k) && !NOT_ACTUALLY_SECRET.contains(&k.as_str()))
            .collect();
        assert!(
            offenders.is_empty(),
            "secret-shaped Settings field(s) {offenders:?}. GET /api/v1/settings serialises this \
             struct wholesale, so the value would be returned in a REST response body, and it \
             would sit in plaintext in pond_system.db. Put credential material in \
             SecretRepository (security/ports/secret.rs) and expose it through /api/v1/secrets, \
             which returns key names only. If the value is genuinely not a credential, add it to \
             NOT_ACTUALLY_SECRET with a reason."
        );
    }

    /// Field names declared on `pub struct Settings`, parsed from source.
    fn declared_field_names() -> Vec<String> {
        let start = SETTINGS_SOURCE.find("pub struct Settings {").expect(
            "could not find `pub struct Settings {` — this parser is broken, not the struct",
        );
        let body = &SETTINGS_SOURCE[start..];
        let end = body
            .find("\n}")
            .expect("could not find the end of the Settings struct");
        body[..end]
            .lines()
            .filter_map(|line| {
                let name = line.trim().strip_prefix("pub ")?.split(':').next()?.trim();
                (!name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
                .then(|| name.to_string())
            })
            .collect()
    }

    /// Other guards only see serialized keys, which a `skip_serializing_if` field escapes.
    #[test]
    fn every_declared_settings_field_is_serialized() {
        let declared = declared_field_names();
        // The parser must not silently match nothing.
        assert!(
            declared.len() > 90,
            "source parse found only {} fields — the parser is broken",
            declared.len()
        );

        let value = serde_json::to_value(Settings::default()).expect("serialize Settings");
        let serialized: std::collections::BTreeSet<String> = value
            .as_object()
            .expect("Settings serializes to a JSON object")
            .keys()
            .cloned()
            .collect();

        let missing: Vec<&String> = declared
            .iter()
            .filter(|d| !serialized.contains(*d))
            .collect();
        assert!(
            missing.is_empty(),
            "declared but not serialized {missing:?}. A `skip_serializing`, `skip_serializing_if` \
             or `rename` on one of these hides it from no_settings_field_is_secret_shaped and \
             from every_settings_field_is_dispositioned, which are the only things standing \
             between a new credential field and GET /api/v1/settings."
        );
        assert_eq!(
            declared.len(),
            serialized.len(),
            "serialized keys {serialized:?} do not match declared fields {declared:?}"
        );
    }

    /// Every field must be UI_WIRED (control + `types.ts` mirror) or HEADLESS_BY_DESIGN.
    /// A new field fails until classified, so it cannot be silently dropped on save.
    #[test]
    fn every_settings_field_is_dispositioned() {
        const HEADLESS_BY_DESIGN: &[&str] = &[
            // No desktop control yet; UI_WIRED would claim one exists.
            "goal_check_enabled",
            // A UI would invite enforcing a half-validated policy matrix.
            "security_policy_mode",
            // Owed an open/allowlist/offline control on the Privacy section.
            "network_mode",
            // Compaction pipeline operator knobs; no UI planned.
            "hybrid_compaction_enabled",
            "summary_idle_secs",
            // A UI would invite shortening it, the damaging direction, for an invisible effect.
            "compaction_verbatim_days",
            "retention_events_days",
            "retention_events_by_category",
            "retention_sensitive_days",
            // Operator latency knob.
            "voice_max_turns",
            // No control yet.
            "vad_backend",
            // Needs a `vision-onnx` build; its UI comes with the Models-tab vision section.
            "vision_classifier_model",
            // No control yet; owed one on the Privacy section, beside `network_mode`.
            "context_ingest_enabled",
            "ext_context_enabled",
            // No UI yet; needs a `lightning` build and `BREEZ_API_KEY`.
            "lightning_enabled",
            // Vestigial; nothing reads it.
            "mesh_settlement_millisats_per_token",
            // No UI until a reasonable ceiling is decided.
            "mesh_lend_token_ceiling",
        ];
        const UI_WIRED: &[&str] = &[
            // Without a `mesh` build the toggle is a no-op (the transport builder warns).
            "mesh_enabled",
            "active_embedding_model",
            "active_llm_model",
            "active_tts_model",
            "active_whisper_model",
            "agent_backend",
            "agent_goose_mode",
            "agent_max_turns",
            "agent_memory_inject",
            "agent_memory_limit",
            "agent_timeout_secs",
            "assistant_name",
            "assistant_personality",
            "cameras_enabled",
            "chat_model",
            "chat_provider",
            "cloud_fallback_enabled",
            "cloud_input_price_per_million",
            "cloud_output_price_per_million",
            "context_monitor_enabled",
            "context_window_override",
            "custom_system_prompt",
            "embedding_provider",
            "ext_device_enabled",
            "ext_knowledge_enabled",
            "ext_memory_enabled",
            "ext_orchestrator_enabled",
            "ext_schedule_enabled",
            "ext_sensor_enabled",
            "ext_system_enabled",
            "ext_weather_enabled",
            "home_name",
            "llm_max_tokens",
            "llm_provider",
            "llm_temperature",
            "matter_ble_enabled",
            "matter_ws_url",
            "memory_archive_threshold",
            "memory_cleanup_enabled",
            "memory_cleanup_interval_hours",
            "memory_consolidation_batch_size",
            "memory_consolidation_enabled",
            "memory_consolidation_interval_hours",
            "memory_consolidation_mode",
            "memory_decay_base_half_life_days",
            "memory_decay_beta",
            "memory_extraction_enabled",
            "memory_extraction_interval_secs",
            "memory_extraction_max_facts",
            "memory_graph_enabled",
            "memory_prune_threshold",
            "mic_enabled",
            "multi_tool_enabled",
            "persist_thinking",
            "prefix_cache_prompt",
            "primary_profile_id",
            "prompt_addendum",
            "prompt_style",
            // Quiet hours are free text: a picker shows an unparseable, silencing bound as unset.
            "proactive_review_enabled",
            "quiet_hours_end",
            "quiet_hours_start",
            "unprompted_speech_categories",
            "unprompted_speech_enabled",
            "retention_event_log_days",
            "retention_sensor_days",
            "retention_session_messages_keep",
            "review_max_rounds",
            "review_mode",
            "review_pass_threshold",
            "schedule_max_concurrent",
            "schedule_max_runs_per_task",
            "schedule_result_notify",
            "session_titling_enabled",
            "reasoning_effort",
            "show_thinking",
            "show_turn_stats",
            "telemetry_enabled",
            "thinking_mode",
            "timezone",
            "tool_call_validation",
            "tool_model",
            "tool_output_compaction",
            "tool_selection_mode",
            "tool_request_detection",
            "user_name",
            "vision_camera_id",
            "vision_camera_url",
            "vision_enabled",
            "vision_fps",
            "vision_motion_threshold",
            "voice_kws_cooldown_ms",
            "voice_kws_energy_threshold",
            "voice_kws_post_trigger_silence_ms",
            "voice_kws_whisper_url",
            "voice_recording_duration_secs",
            "voice_thinking_tone_enabled",
            "voice_tts_quality",
            "voice_tts_speed",
            "voice_tts_voice",
            "voice_wake_word",
            "voice_wake_word_transcriptions",
            "voice_whisper_url",
            "weather_enabled",
            "weather_latitude",
            "weather_location_name",
            "weather_longitude",
            // No UI row while its only reader, `search_web`, is unregistered; the value persists.
            "searxng_url",
        ];

        let value = serde_json::to_value(Settings::default()).expect("serialize Settings");
        let obj = value
            .as_object()
            .expect("Settings serializes to a JSON object");
        let keys: std::collections::BTreeSet<&str> = obj.keys().map(|k| k.as_str()).collect();

        // 1. The two lists are disjoint.
        for k in UI_WIRED {
            assert!(
                !HEADLESS_BY_DESIGN.contains(k),
                "field `{k}` is in both UI_WIRED and HEADLESS_BY_DESIGN"
            );
        }
        // 2. No stale/typo entries — every listed field is a real serialized key.
        for k in UI_WIRED.iter().chain(HEADLESS_BY_DESIGN.iter()) {
            assert!(
                keys.contains(k),
                "listed field `{k}` is not an actual Settings field (stale entry — remove it)"
            );
        }
        // 3. Every serialized field is dispositioned.
        for k in &keys {
            assert!(
                UI_WIRED.contains(k) || HEADLESS_BY_DESIGN.contains(k),
                "Settings field `{k}` is not dispositioned. Add it to UI_WIRED \
                 (and wire it into pond-desktop Settings.tsx + types.ts) or to \
                 HEADLESS_BY_DESIGN in this test."
            );
        }
        // 4. Counts add up (guards against an accidental double-count).
        assert_eq!(
            keys.len(),
            UI_WIRED.len() + HEADLESS_BY_DESIGN.len(),
            "settings field count mismatch: {} serialized vs {} classified",
            keys.len(),
            UI_WIRED.len() + HEADLESS_BY_DESIGN.len()
        );
    }

    #[test]
    fn the_orchestrator_toggle_defaults_off_by_its_own_route() {
        assert!(
            !Settings::default().ext_orchestrator_enabled,
            "the struct default is what a FAILED settings read produces via \
             unwrap_or_default(); on failure, access narrows"
        );

        let from_nothing: Settings =
            serde_json::from_str("{}").expect("every Settings field has a serde default");
        assert!(
            !from_nothing.ext_orchestrator_enabled,
            "the serde default is what a settings payload written before this field existed \
             deserializes to -- i.e. every pond that upgrades into this release"
        );

        assert!(
            Settings::default_ext_enabled(),
            "vacuity control: the shared extension default really does return true, so \
             `default_ext_orchestrator_enabled` diverging from it is a decision and not a \
             coincidence"
        );
        assert!(!Settings::default_ext_orchestrator_enabled());
    }

    #[test]
    fn the_unprompted_speech_toggle_defaults_off_by_its_own_route() {
        assert!(
            !Settings::default().unprompted_speech_enabled,
            "the struct default is what a FAILED settings read produces via \
             unwrap_or_default(); on failure the pond stays quiet"
        );

        let from_nothing: Settings =
            serde_json::from_str("{}").expect("every Settings field has a serde default");
        assert!(
            !from_nothing.unprompted_speech_enabled,
            "the serde default is what a settings payload written before this field existed \
             deserializes to -- i.e. every pond that upgrades into this release"
        );
        assert!(!Settings::default_unprompted_speech_enabled());

        // Vacuity control: the empty payload really populated every other field.
        assert_eq!(
            from_nothing.quiet_hours_start,
            Settings::default_quiet_hours_start(),
            "an empty payload must fill every other field from its default too"
        );
        assert!(
            from_nothing.tool_call_validation,
            "vacuity control: a serde default that is genuinely `true` survives the same \
             empty payload, so `unprompted_speech_enabled` being false is a decision"
        );
    }

    #[test]
    fn the_context_ingest_toggle_defaults_off_by_its_own_route() {
        assert!(
            !Settings::default().context_ingest_enabled,
            "the struct default is what a FAILED settings read produces via \
             unwrap_or_default(); on failure the pond copies nothing"
        );

        let from_nothing: Settings =
            serde_json::from_str("{}").expect("every Settings field has a serde default");
        assert!(
            !from_nothing.context_ingest_enabled,
            "the serde default is what a settings payload written before this field existed \
             deserializes to -- i.e. every pond that upgrades into this release"
        );
        assert!(!Settings::default_context_ingest_enabled());

        // Vacuity controls, as in the speech-toggle test above.
        assert_eq!(
            from_nothing.quiet_hours_start,
            Settings::default_quiet_hours_start()
        );
        assert!(
            from_nothing.tool_call_validation,
            "vacuity control: a `true` serde default survives the same empty payload, so \
             `context_ingest_enabled` being false is a decision"
        );
    }

    #[test]
    fn a_fresh_pond_already_has_a_quiet_window_and_the_narrowest_category() {
        let s = Settings::default();
        assert_eq!(s.quiet_hours_start, "22:00");
        assert_eq!(s.quiet_hours_end, "07:00");
        assert_eq!(
            s.unprompted_speech_categories, "alert",
            "`info` carries every completed scheduled task, so a default including it turns \
             this feature into the pond reading out its own cron log"
        );
    }

    /// Only these `ext_*` toggles may ship off, each for the reason given here:
    /// - `ext_orchestrator_enabled`: consent to autonomous `GooseMode::Auto` agents.
    /// - `ext_context_enabled`: consent, and two tool schemas in every turn's prompt.
    #[test]
    fn only_the_deliberate_extension_toggles_ship_switched_off() {
        let value = serde_json::to_value(Settings::default()).expect("serialize Settings");
        let mut off: Vec<&str> = value
            .as_object()
            .expect("Settings serializes to a JSON object")
            .iter()
            .filter(|(k, v)| k.starts_with("ext_") && *v == &serde_json::Value::Bool(false))
            .map(|(k, _)| k.as_str())
            .collect();
        // Sorted: `serde_json/preserve_order` is on in workspace builds only, so map order varies.
        off.sort_unstable();
        assert_eq!(
            off,
            vec!["ext_context_enabled", "ext_orchestrator_enabled"],
            "the set of extension toggles that ship OFF changed. Adding one is a deliberate \
             decision and belongs in this test's doc comment with its reason; losing \
             `ext_orchestrator_enabled` means delegation is now on by default on every install, \
             and losing `ext_context_enabled` means two personal-context tools are in every \
             turn's prompt on every install"
        );
    }
}
