//! Configurable settings for GIAP, organized into four categories.
//!
//! Each field maps to a key in the `settings` SQLite table (flat key-value store).
//! Defaults are the factory values used when a key is not present in the store.

use serde::{Deserialize, Serialize};

/// Where GIAP's own Matter controller listens.
///
/// The path names the protocol the controller speaks, so an address left over
/// from an earlier release fails at the handshake with a sentence naming the
/// problem rather than half-working.
pub const DEFAULT_MATTER_WS_URL: &str = "ws://127.0.0.1:5580/giap";

/// The default this setting had before the controller spoke `giap-matter`.
///
/// Every install predating that release holds this string, and it points at a
/// path the current controller does not serve — so without rewriting it,
/// upgrading would silently break Matter for everyone who never touched the
/// field. Only the exact old default is migrated: an address the user typed
/// themselves is their own and is left alone.
pub const LEGACY_MATTER_WS_URL: &str = "ws://127.0.0.1:5580/ws";

/// Rewrite the superseded default, leaving anything user-chosen alone.
pub fn migrate_matter_ws_url(stored: &str) -> String {
    if stored.trim() == LEGACY_MATTER_WS_URL {
        DEFAULT_MATTER_WS_URL.to_string()
    } else {
        stored.to_string()
    }
}

/// The turn budget handed to the agent engine when `agent_max_turns == 0`
/// ("uncapped").
///
/// The engine enforces its budget as a bare `turns_taken > max_turns` compare on
/// a `u32` and also renders the number into its per-turn context block, so
/// "uncapped" has to be a number rather than an absence. `u32::MAX` is the
/// obvious choice and the wrong one: any arithmetic on it (a percentage, a
/// remaining-turns subtraction, a `+ 1`) overflows or formats absurdly.
/// 100_000 provider calls is unreachable for a real request — the idle timeout
/// or the context limit lands first — while staying safe to do maths on.
pub const UNCAPPED_MAX_TURNS: u32 = 100_000;

/// `tool_selection_mode`: send every registered extension's tools every turn.
pub const TOOL_SELECTION_MODE_ALL: &str = "all";
/// `tool_selection_mode`: core groups plus the groups scored relevant to the
/// session, chosen once at session start (Phase D2).
pub const TOOL_SELECTION_MODE_RELEVANT: &str = "relevant";
/// `tool_selection_mode`: the toolkit escape hatch and nothing else — every
/// other group arrives only when the model calls `enable_tool_group`.
///
/// This is the only mode that fits the 4%-of-prompt-budget target. The prompt
/// budget for a local provider is `LOCAL_PROMPT_CLAMP` = 8,192 tokens, so 4% is
/// 327 tokens, and a tool costs ~74 characters of JSON envelope before it says
/// anything at all: 27 tools breach the target with empty schemas. "relevant"
/// cannot reach it either — its core floor (memory + system + toolkit) is 778
/// tokens, 9.5%. Two tools, 222 tokens, 2.7%, is what is left.
///
/// The date and time ride in `<system-context>` every turn, so the most-asked
/// capability does not need a tool to be present for it.
pub const TOOL_SELECTION_MODE_MINIMAL: &str = "minimal";

/// `security_policy_mode`: no evaluation, no audit trail. Debugging only.
pub const SECURITY_POLICY_MODE_OFF: &str = "off";
/// `security_policy_mode`: evaluate and record every decision, block none.
pub const SECURITY_POLICY_MODE_AUDIT: &str = "audit";
/// `security_policy_mode`: denials bite.
pub const SECURITY_POLICY_MODE_ENFORCE: &str = "enforce";

/// The accepted values of `security_policy_mode`, for validation and for the
/// error message a rejected write gets back.
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

/// The accepted values of `network_mode`, for validation and for the error
/// message a rejected write gets back.
pub const NETWORK_MODES: &[&str] = &[
    NETWORK_MODE_OPEN,
    NETWORK_MODE_ALLOWLIST,
    NETWORK_MODE_OFFLINE,
];

/// The accepted values of `reasoning_effort`, for validation and for the error
/// message a rejected write gets back.
///
/// The behaviour behind each value lives in
/// `models::services::context::context_budget::ReasoningEffort`, and a test
/// there (`reasoning_effort_strings_agree_with_settings`) fails if either side
/// grows a value alone.
pub const REASONING_EFFORTS: &[&str] = &["brief", "balanced", "thorough"];

// ── The remaining closed vocabularies ───────────────────────────────────────
//
// Written down here, beside the three that already were, because a value set
// that lives only in the desktop's `catalogue.ts` is a value set the server
// does not enforce — and every writer that is not the catalogue (a raw PUT, the
// CLI, a future mobile client) bypassed it entirely. See
// `settings_validation::FIELD_RULES`, which is what actually applies them.

/// How many tools a turn is offered. See `tool_selection_mode`.
pub const TOOL_SELECTION_MODES: &[&str] = &[
    TOOL_SELECTION_MODE_ALL,
    TOOL_SELECTION_MODE_RELEVANT,
    TOOL_SELECTION_MODE_MINIMAL,
];

/// The agent loop that serves turns.
///
/// One value, and that is the point: `pond` is quarantined (Q2-05) and the API
/// refuses it. It was refused by NAME, though — a denylist of exactly one
/// string — so `gosse` or `ollama` was accepted, stored, survived the startup
/// heal, and then failed the `!= "goose"` test that selects the real agent,
/// leaving every request answered by `MockAgent` with no error anywhere.
pub const AGENT_BACKENDS: &[&str] = &["goose"];

/// Backends that exist but must never be selected. Named separately so the
/// refusal can say *why* rather than only listing what is allowed.
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

/// Which detector decides that a frame is speech.
///
/// `rms` is the energy gate that has always shipped: cheap, and unable to tell
/// a fridge from a voice. `silero` runs a 2 MB ONNX model that can — measured,
/// steady noise at twice the energy threshold scores 0.08 where speech averages
/// 0.945 — at about 1.6% of one core on a Jetson.
pub const VAD_BACKENDS: &[&str] = &["rms", "silero"];

/// One factory default that CHANGED after installs already existed.
///
/// Settings are a flat key-value table and a default only applies when the key
/// is ABSENT. Any install that ever saved a settings snapshot has every key
/// pinned to whatever the default was on that day, so a later default change is
/// invisible there forever. Each entry here records one such change and is
/// adopted, once, by the named migration.
///
/// The adoption rule is deliberately narrow: adopt only where the stored value
/// is still byte-equal to `old_default`, which means the user never chose
/// anything different. The one accepted false positive is a user who
/// deliberately picked exactly the old value — nothing in the store
/// distinguishes them from a user who never chose, so they are moved to the new
/// default once (and their next explicit save marks the key user-set, which
/// exempts it from every future adoption).
///
/// What does NOT belong here: a key whose default never actually moved. The
/// entry only fires where the stored value equals `old_default`, so if the
/// factory default is unchanged there is nothing to adopt and the entry would
/// be a no-op (`tool_selection_mode` is still `"all"`, and stored `"all"` rows
/// exist — the exclusion is "the default did not move", not "the key is new").
/// A stored value that never was any default is likewise out of reach:
/// `embedding_provider` has defaulted to `"fastembed"` since it was
/// introduced, so a stored `"none"` matches no `old_default` and can only be
/// changed by hand or by a UI prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultAdoption {
    /// The `settings` table key.
    pub key: &'static str,
    /// How the OLD default was rendered into the store.
    pub old_default: &'static str,
    /// How the CURRENT default is rendered into the store.
    pub new_default: &'static str,
    /// Numeric prefix of the migration that performs the adoption.
    pub migration: &'static str,
}

/// Every shipped default change that needs to reach existing installs, oldest
/// first.
///
/// A default may move more than once: entries for the same key CHAIN, each
/// one's `old_default` picking up where the previous one's `new_default` left
/// off, and only the last entry for a key states the value
/// `Settings::default()` produces today. `default_adoptions_are_structurally_sound`
/// (below) enforces the chaining, so changing a registered default again forces
/// a new entry plus a new migration rather than an edit to a migration that
/// already ran.
///
/// The other half — that `new_default` is the exact literal the store holds for
/// today's default — is enforced in **pond-infra**
/// (`every_adoption_entry_states_the_literal_the_adapter_writes`), not here.
/// It cannot be checked in this crate: the domain has no way to render a field
/// the way the adapter does, and the obvious stand-in disagrees. The adapter
/// writes numbers with `Display`, while a `serde_json` round-trip widens every
/// `f32` to `f64` (`0.05f32` renders as `0.05`, but as `0.05000000074505806`
/// through JSON). A checker built on the second would demand a literal the
/// migration could never match.
pub const DEFAULT_ADOPTIONS: &[DefaultAdoption] = &[
    // The 20-turn cap stranded multi-step research and home-automation
    // requests mid-task; the rails that actually protect the device are
    // cancellation, `agent_timeout_secs`, and context-overflow abort.
    DefaultAdoption {
        key: "agent_max_turns",
        old_default: "20",
        new_default: "50",
        migration: "0035",
    },
    // Deterministic trimming replaced Goose's reactive LLM auto-compaction
    // once C1-C3 landed; off, an on-device conversation still stalls
    // mid-turn to summarise itself.
    DefaultAdoption {
        key: "hybrid_compaction_enabled",
        old_default: "false",
        new_default: "true",
        migration: "0035",
    },
    // The energy gate cannot tell a fridge from a voice, so it holds the
    // microphone open on room noise until the hard cap. Opt-in, it was never
    // going to be on anywhere it mattered.
    DefaultAdoption {
        key: "vad_backend",
        old_default: "rms",
        new_default: "silero",
        migration: "0052",
    },
    // Voice held the tightest turn budget in the pond while text was raised to
    // 50 for exactly the requests a household speaks rather than types. The
    // same ask finished when typed and gave up six turns in when spoken.
    DefaultAdoption {
        key: "voice_max_turns",
        old_default: "8",
        new_default: "0",
        migration: "0053",
    },
];

/// All configurable settings for GIAP.
///
/// Serializes to/from JSON via serde. Each field has a default via
/// the `Default` impl and companion `serde(default)` attributes,
/// so deserializing a partial JSON object fills missing fields with defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    // ── Assistant identity ──────────────────────────────────────────────────
    /// UUID of the primary household profile created during onboarding.
    #[serde(default)]
    pub primary_profile_id: Option<String>,

    /// Display name the assistant uses (default: "Goose")
    #[serde(default = "Settings::default_assistant_name")]
    pub assistant_name: String,

    /// Personality hint injected into the system prompt
    #[serde(default = "Settings::default_assistant_personality")]
    pub assistant_personality: String,

    /// Primary user's name, used to personalize responses
    #[serde(default = "Settings::default_user_name")]
    pub user_name: String,

    /// IANA timezone string, e.g. "Africa/Nairobi"
    #[serde(default = "Settings::default_timezone")]
    pub timezone: String,

    /// Human-readable name for this household/home (e.g. "The Anyumba Home").
    /// Shown in the UI and used to personalise greetings. Empty by default.
    #[serde(default = "Settings::default_home_name")]
    pub home_name: String,

    /// Prompt style — selects the built-in system prompt template.
    /// Accepted values: "balanced" (default) | "concise" | "technical" | "warm"
    #[serde(default = "Settings::default_prompt_style")]
    pub prompt_style: String,

    /// Advanced: fully replace the system prompt. Supports {{assistant_name}},
    /// {{user_name}}, {{personality}}, {{timezone}}, {{location}},
    /// {{prompt_addendum}} placeholders. When Some, overrides prompt_style.
    #[serde(default)]
    pub custom_system_prompt: Option<String>,

    /// Extra instructions appended to the generated system prompt (max 500 chars).
    /// Example: "Always respond in French." or "Mention upcoming schedules proactively."
    #[serde(default = "Settings::default_prompt_addendum")]
    pub prompt_addendum: String,

    // ── Model roles ────────────────────────────────────────────────────────
    /// Provider for the Chat role (fast, conversational). Default = llm_provider.
    #[serde(default = "Settings::default_llm_provider")]
    pub chat_provider: String,

    /// Model name for the Chat role. Default = active_llm_model.
    #[serde(default = "Settings::default_active_llm_model")]
    pub chat_model: String,

    /// GGUF model for the dedicated tool-calling specialist.
    /// When set, empty tool-call arguments are re-generated by this small model
    /// instead of relying on the main LLM to format them correctly.
    /// Must be in $DATA_DIR/models/gguf/. Example: "functiongemma-270m-q4_k_m.gguf"
    #[serde(default)]
    pub tool_model: Option<String>,

    // ── LLM behaviour ──────────────────────────────────────────────────────
    /// Maximum tokens the LLM may generate per response
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

    /// Separate whisper server URL for keyword-spotting (wake-word detection only).
    /// When set, `WhisperKeywordDetector` sends sliding windows here while
    /// `WhisperInput` uses `voice_whisper_url` for accurate command transcription.
    /// Recommended: point this at a `tiny`-model server (39 MB, ~0.3s inference)
    /// for fast detection while keeping `base`/`small` for ASR accuracy.
    /// Defaults to `voice_whisper_url` when `None`.
    #[serde(default)]
    pub voice_kws_whisper_url: Option<String>,

    /// Minimum RMS energy for the wake-word detector to call whisper.
    /// Windows quieter than this are skipped, eliminating ~90% of whisper calls
    /// during silence. Default: 0.01 (~−40 dBFS). Set to 0.0 to disable.
    #[serde(default = "Settings::default_kws_energy_threshold")]
    pub voice_kws_energy_threshold: f32,

    /// Milliseconds of consecutive silence that terminates post-trigger audio capture.
    /// Enables early exit from the fixed `post_trigger_ms` wait when the user has
    /// finished speaking. Default: 400 ms. Set to 0 to always wait the full window.
    #[serde(default = "Settings::default_kws_post_trigger_silence_ms")]
    pub voice_kws_post_trigger_silence_ms: u64,

    /// Milliseconds to sleep before re-arming detection after each activation.
    /// Prevents re-triggering on TTS echo or room noise. Default: 2000 ms.
    #[serde(default = "Settings::default_kws_cooldown_ms")]
    pub voice_kws_cooldown_ms: u64,

    /// Whisper transcription variants collected during wake-word calibration.
    ///
    /// Empty → detector falls back to raw normalized `voice_wake_word` as the sole pattern.
    /// Non-empty → detector matches against any variant in this list (OR logic), enabling
    /// robust detection across Whisper's inconsistent output ("hey goose" / "hey, goose" /
    /// "a goose" etc.).
    #[serde(default)]
    pub voice_wake_word_transcriptions: Vec<String>,

    /// Suggestion kinds the household never wants offered on Home.
    ///
    /// Holds [`suggestion`](crate::user_data::services::suggestion) suggestor
    /// ids. Per KIND and not per instance, deliberately: a suggestion is
    /// derived on every read and carries no durable id, so an instance-level
    /// dismissal would be a key that never matched again -- the same shape as
    /// the memory-edge table, which has a writer and no reachable reader.
    /// "Never suggest the weather" is also what a household actually means.
    ///
    /// Empty is the shipped state: nothing is muted until somebody mutes it.
    #[serde(default)]
    pub suggestions_muted: Vec<String>,

    /// Selected TTS voice.
    ///
    /// Kokoro voice id (`af_heart`, `bm_george`, …) since the engine swap; a
    /// Piper `.onnx` filename on installs that predate it. Resolution accepts
    /// both, so an upgrade does not silence a pond whose stored value is still
    /// a Piper filename.
    #[serde(default = "Settings::default_tts_voice")]
    pub voice_tts_voice: String,

    /// Speaking pace, as the multiplier the synthesiser's `speed` input takes.
    ///
    /// 1.0 is the voice as trained; the engine clamps to 0.5..=2.0. Stored as
    /// the multiplier rather than a percentage so there is no conversion
    /// between this row and the tensor — the UI converts at its own edge,
    /// where getting it backwards is visible.
    #[serde(default = "Settings::default_tts_speed")]
    pub voice_tts_speed: f32,

    /// Voice quality tier — a Kokoro quantisation (`q8` | `q8f16` | `q4f16` |
    /// `fp16` | `fp32`).
    ///
    /// Picking a tier picks an `.onnx` file and nothing else. Defaults to `q8`
    /// (92 MB), the only tier that sits comfortably beside an LLM on an 8 GB
    /// Jetson. An unknown value resolves to the default rather than failing —
    /// a mistyped tier must not leave the pond unable to speak.
    #[serde(default = "Settings::default_tts_quality")]
    pub voice_tts_quality: String,

    /// Which detector decides that a frame is speech (`rms` | `silero`).
    ///
    /// Only the *endpoint* — deciding the user has stopped talking — goes
    /// through this. Speech onset stays on the energy gate, deliberately: a
    /// freshly reset Silero scores 0.27 on a window of unambiguous speech
    /// because its recurrent state needs a window or two of context, which is
    /// harmless when looking for silence and would clip the first word when
    /// looking for the start of one.
    ///
    /// Defaults to `silero`, which fetches its 2 MB model on first use, and
    /// falls back to `rms` whenever that cannot be had — no network, no ONNX
    /// runtime, a load that times out. `rms` remains selectable as the escape
    /// hatch for a board whose runtime is broken. An unknown value falls back
    /// too, and says so: a mistyped backend must not leave the pond deaf.
    #[serde(default = "Settings::default_vad_backend")]
    pub vad_backend: String,

    /// Whether the soft ambient tone plays while the model is working.
    ///
    /// The tone is the only signal that a spoken request was heard and is being
    /// worked on — without it a slow turn is indistinguishable from a turn that
    /// was never heard at all. It was deleted outright once for reading as "an
    /// annoying background beep"; that is a preference, not a defect, so it is
    /// a setting rather than a decision made for every household.
    ///
    /// Defaults ON: the silence it fills is a real gap, and a household that
    /// dislikes the tone can find this switch, whereas one that never hears the
    /// tone has nothing to go looking for.
    #[serde(default = "Settings::default_voice_thinking_tone_enabled")]
    pub voice_thinking_tone_enabled: bool,

    /// Microphone recording duration in seconds for each whisper capture
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

    /// Embedding provider: "fastembed" (default, local ONNX), "gguf" (llama.cpp,
    /// the on-device path — fastembed's ONNX Runtime does not initialise on the
    /// Jetson Orin), or "none". "gguf" requires the `local-inference` build.
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
    /// Whether to run the on-device vision pipeline (camera capture + motion
    /// detection feeding camera_events / the EventBus). Off by default —
    /// requires a camera and ffmpeg on the device.
    #[serde(default = "Settings::default_vision_enabled")]
    pub vision_enabled: bool,

    /// Camera input for the vision pipeline: an `rtsp://` URL or a local
    /// device path like `/dev/video0`. Empty = pipeline not started.
    #[serde(default = "Settings::default_vision_camera_url")]
    pub vision_camera_url: String,

    /// The camera_id stamped on emitted vision events (matched by automation
    /// rules and shown in the activity feed).
    #[serde(default = "Settings::default_vision_camera_id")]
    pub vision_camera_id: String,

    /// Frames per second to analyse (low on purpose — motion detection does
    /// not need full frame rate, and this bounds CPU use on the Jetson).
    #[serde(default = "Settings::default_vision_fps")]
    pub vision_fps: u32,

    /// Fraction of the frame (0.0–1.0) that must change to count as motion.
    #[serde(default = "Settings::default_vision_motion_threshold")]
    pub vision_motion_threshold: f64,

    /// ONNX detector that labels motion events (person/pet/package). Empty
    /// (default) = the bundled YOLOX-Nano, auto-downloaded on first serve of
    /// a `vision-onnx` build; a value names an operator-managed file
    /// (relative paths resolve under `<data_dir>/models/vision/`, no
    /// auto-download). Builds without the `vision-onnx` feature ignore this
    /// and emit plain "motion". Needs the ONNX Runtime library at startup.
    #[serde(default = "Settings::default_vision_classifier_model")]
    pub vision_classifier_model: String,

    // ── Matter (#195) ──────────────────────────────────────────────────────
    /// WebSocket URL of the Matter controller.
    #[serde(default = "Settings::default_matter_ws_url")]
    pub matter_ws_url: String,

    /// Whether the Matter controller pairs over Bluetooth as well as IP.
    ///
    /// Off by default, and the default is not timidity. BLE is the only way a
    /// device that has never been on the network can be paired at all — out of
    /// its box it holds no Wi-Fi credentials, so it cannot advertise on mDNS,
    /// and the commissioner hands the credentials over during the BLE
    /// conversation. But the radio needs a native module that may not be
    /// installed (`@stoprocent/noble`, optional twice over) and permission a
    /// headless service does not have by default: `cap_net_raw` on Linux, and on
    /// macOS an `NSBluetoothAlwaysUsageDescription` in the bundle's Info.plist,
    /// without which the OS kills the process outright rather than refusing the
    /// radio. Turning a radio on, and risking that, is not something to do to
    /// someone's machine because a controller started.
    ///
    /// Changing it restarts the controller: it is an argument to that process.
    #[serde(default = "Settings::default_matter_ble_enabled")]
    pub matter_ble_enabled: bool,

    // ── Private mesh (#132) ────────────────────────────────────────────────
    /// Whether to start the private mesh transport (a trust-scoped P2P link
    /// to this Pond's own other devices / trusted peers). Off by default —
    /// no UI yet (no MCP tool or route consumes the transport this
    /// milestone), and requires a `pond-server` build with the `mesh`
    /// feature. The mesh identity keypair is stored separately via
    /// `SettingsRepository::get_key`/`set_key` under `mesh_identity_secret`,
    /// not as a `Settings` field — it's an internal secret, not a setting.
    #[serde(default = "Settings::default_mesh_enabled")]
    pub mesh_enabled: bool,

    /// Whether to connect to the Breez/Spark Lightning network for
    /// mesh-peer settlement. Off by default — no UI yet, and requires a
    /// `pond-server` build with the `lightning` feature plus a
    /// `BREEZ_API_KEY` env var. The wallet mnemonic is stored separately via
    /// `SettingsRepository::get_key`/`set_key` under
    /// `lightning_wallet_mnemonic`, not as a `Settings` field — it's an
    /// internal secret, not a setting (mirrors `mesh_identity_secret`).
    #[serde(default = "Settings::default_lightning_enabled")]
    pub lightning_enabled: bool,

    /// Vestigial — no longer read by anything. The exchange rate is now
    /// `pond_core::mesh::domain::settlement::MESH_SETTLEMENT_MILLISATS_PER_TOKEN`,
    /// a fixed dev-decided constant, not a per-Pond setting (a borrower
    /// reading its own number here could simply set it to pay less). Kept
    /// on `Settings` only so old persisted rows/API payloads still
    /// (de)serialize; setting it via the API does nothing.
    #[serde(default = "Settings::default_mesh_settlement_millisats_per_token")]
    pub mesh_settlement_millisats_per_token: u64,

    /// Most tokens this Pond will lend a single trusted peer within one
    /// rolling ~15-minute window before refusing further requests until it
    /// resets. Default `0` means "not configured", same no-op convention as
    /// `mesh_settlement_millisats_per_token`. A throttle against runaway
    /// local-inference cost, not a payment-verified cap — the window resets
    /// on a timer, not on confirmed payment (see `MeshInferenceService`'s
    /// lend-window docs).
    #[serde(default = "Settings::default_mesh_lend_token_ceiling")]
    pub mesh_lend_token_ceiling: u64,

    // ── Privacy / sensor access ────────────────────────────────────────────
    /// User-controlled privacy toggle for microphone access. When false, the
    /// voice pipeline (wake-word + ASR capture) is not permitted to record.
    /// The device has a mic; this is the user's consent switch. Default: true.
    #[serde(default = "Settings::default_mic_enabled")]
    pub mic_enabled: bool,

    /// User-controlled privacy toggle for camera access. When false, the vision
    /// pipeline and any camera capture are not permitted. The device has
    /// cameras; this is the user's consent switch. Default: true.
    #[serde(default = "Settings::default_cameras_enabled")]
    pub cameras_enabled: bool,

    /// When true, requests may spill over to a cloud model on local-model
    /// failure. Privacy-first: OFF by default (local-only, opt-in). Persisted
    /// here to gate a future failure-only cloud-spill path — no spill logic yet.
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

    /// Baseline days to keep rows in the unified `events` log (#117).
    /// `0` = keep forever. Per-category overrides take precedence.
    #[serde(default = "Settings::default_events_days")]
    pub retention_events_days: u32,

    /// Per-`EventCategory` retention override (snake_case category → days),
    /// e.g. `{"network": 14, "sensor": 7}`. Categories absent here fall back to
    /// `retention_events_days`. Empty by default.
    #[serde(default)]
    pub retention_events_by_category: std::collections::HashMap<String, u32>,

    /// Privacy-by-default cap: events classified `Sensitive` or `Secret` are
    /// purged after at most this many days, regardless of category (#117).
    /// `0` = no extra cap.
    #[serde(default = "Settings::default_sensitive_days")]
    pub retention_sensitive_days: u32,

    // ── Thinking / Reasoning ────────────────────────────────────────────────────
    /// Thinking/reasoning mode: "auto" | "on" | "off"
    /// "auto" (default): enable for models that support it (Gemma 4, Qwen3, etc.)
    /// "on": always attempt to enable thinking
    /// "off": never use thinking mode
    #[serde(default = "Settings::default_thinking_mode")]
    pub thinking_mode: String,

    /// When true, thinking/reasoning blocks are forwarded to the UI as events
    /// instead of being silently stripped. Off by default.
    #[serde(default)]
    pub show_thinking: bool,

    /// How much room the model is told it may spend thinking: "brief" |
    /// "balanced" | "thorough" (default "brief").
    ///
    /// A preference, not a token count. The number it becomes is derived from
    /// the active compaction profile's output reserve — see
    /// `context_budget::reasoning_budget_tokens` — because the right value is a
    /// function of the window and the device, not of what somebody typed.
    ///
    /// `brief` by default because the shipped target is a Jetson Orin Nano,
    /// where reasoning tokens are decode tokens and decode is
    /// memory-bandwidth-bound: every thinking token is silence before the
    /// answer starts. `thorough` is the right choice on an HTTP provider.
    ///
    /// Orthogonal to `thinking_mode`: this says how LONG, that says WHETHER.
    /// `thinking_mode = "off"` removes the whole section, budget and all.
    #[serde(default = "Settings::default_reasoning_effort")]
    pub reasoning_effort: String,

    /// Whether the reasoning TEXT a turn produced is written to
    /// `session_thinking` and replayed into the thinking panel on reload.
    /// **False by default.**
    ///
    /// Orthogonal to both neighbours above: `thinking_mode` says whether the
    /// model thinks, `show_thinking` says whether the live stream shows it, and
    /// this says whether it SURVIVES the stream. Showing something once and
    /// keeping it forever are different consents, and a pond that conflates
    /// them has decided on the user's behalf.
    ///
    /// Off by default because this is the least reviewed text the model
    /// produces -- the passage where it tries the wrong answer, names a
    /// household member it then decides not to mention, or reasons about
    /// something the user only implied. It is also the passage nothing else
    /// prunes: `retention_session_messages_keep` bounds the transcript, and
    /// these rows ride the transcript's CASCADE rather than a policy of their
    /// own. Opt-in is the only defensible polarity for it.
    ///
    /// Turning it OFF stops new writes; it does not erase what is already
    /// there. `DELETE /api/v1/sessions/{id}` still cascades, which is the
    /// erasure path that exists today.
    #[serde(default)]
    pub persist_thinking: bool,

    // ── Answer Review ──────────────────────────────────────────────────────
    /// Review mode: "off" (default) | "on" | "auto"
    /// "off": no review — answers stream directly to the user
    /// "on": every answer is reviewed by the adversarial critic before delivery
    /// "auto": only review factual/analytical questions (Think-classified or tool-augmented)
    #[serde(default = "Settings::default_review_mode")]
    pub review_mode: String,

    /// Maximum review-revision rounds. 1 = one review + one optional revision.
    #[serde(default = "Settings::default_review_max_rounds")]
    pub review_max_rounds: u32,

    /// Minimum score (1-5) for the reviewer to pass an answer. Below this triggers revision.
    #[serde(default = "Settings::default_review_pass_threshold")]
    pub review_pass_threshold: u8,

    /// Override the model's reported context window (tokens).
    /// 0 (default) = use the model's own value.
    /// Non-zero = cap at this value (useful for memory-constrained deployments).
    #[serde(default)]
    pub context_window_override: u32,

    /// Show the per-turn inference stats footer (TTFT, tok/s, context) under
    /// assistant messages in the desktop/web chat UIs.
    #[serde(default)]
    pub show_turn_stats: bool,

    /// GIAP-owned hybrid compaction (deterministic in-turn trim + idle rolling
    /// summary). When true, Goose's own auto-compaction and its background
    /// tool-pair summarization are disabled for the live path — GIAP owns
    /// history pruning end to end.
    ///
    /// Default TRUE. Off, the only defences against a full context on-device are
    /// Goose's reactive LLM auto-compaction (a mid-conversation stall the user
    /// waits through) and the 200K file-spill threshold. On, pruning is
    /// deterministic and costs no inference: whole oldest turns are dropped,
    /// oversized tool results are truncated head+tail, and the idle-refreshed
    /// rolling summary is spliced in.
    #[serde(default = "Settings::default_hybrid_compaction_enabled")]
    pub hybrid_compaction_enabled: bool,

    /// Idle seconds before the rolling-summary refresh may run (never at
    /// startup; a new turn cancels an in-flight refresh).
    #[serde(default = "Settings::default_summary_idle_secs")]
    pub summary_idle_secs: u32,

    /// Days of history the in-turn trimmer keeps *verbatim* before age
    /// weighting is allowed to degrade it harder than the flat caps do.
    ///
    /// PAI-4 P3. `0` disables age weighting entirely, and is the only way to;
    /// there is no separate boolean that could fall out of step with the
    /// number. The rung it controls
    /// (`turn_trimmer::AGED_TOOL_RESULT_MAX_BYTES`) fires only when a
    /// conversation is already over budget, so a *large* value costs nothing
    /// beyond today's behaviour. Small is the damaging direction — a horizon
    /// inside the span of a live conversation would hard-truncate tool results
    /// the model is still reasoning about.
    #[serde(default = "Settings::default_compaction_verbatim_days")]
    pub compaction_verbatim_days: u32,

    // ── Agent behaviour ────────────────────────────────────────────────────────
    /// Agent backend engine: "goose" (default, full-featured) | "pond" (independent, KV-cache reuse).
    /// "goose" uses Block's Goose framework with all MCP extensions, cloud provider support.
    /// "pond" uses PondAgent + LlamaCppEngine directly for minimal latency on local models.
    #[serde(default = "Settings::default_agent_backend")]
    pub agent_backend: String,

    /// GooseMode for the agent loop: "auto" | "chat" | "smart"
    #[serde(default = "Settings::default_agent_goose_mode")]
    pub agent_goose_mode: String,

    /// Maximum agentic loop turns per request (a turn = one provider call).
    /// `0` means UNCAPPED: the model reasons and calls tools for as long as the
    /// task needs, bounded only by cancellation, the idle timeout
    /// (`agent_timeout_secs`), and context-overflow abort.
    #[serde(default = "Settings::default_agent_max_turns")]
    pub agent_max_turns: u32,

    /// Maximum agentic loop turns for VOICE requests (#105).
    ///
    /// Defaults to `0` — no voice-specific cap, so a spoken request gets the
    /// same `agent_max_turns` budget a typed one does. See
    /// `default_voice_max_turns` for why the 8 it used to be was making voice
    /// look unreliable. A non-zero value restores the trade — completeness for
    /// latency — and is never raised above `agent_max_turns`.
    #[serde(default = "Settings::default_voice_max_turns")]
    pub voice_max_turns: u32,

    /// Maximum seconds of SILENCE (no stream event) before an agent turn
    /// is aborted. This bounds a stalled stream, NOT total generation time,
    /// so slow reasoning models that stream continuously are never killed.
    /// The deadline is reset on every stream event. Set to 0 to disable.
    /// Default: 300 (5 minutes of no progress).
    #[serde(default = "Settings::default_agent_timeout_secs")]
    pub agent_timeout_secs: u64,

    /// When true, the system prompt is partitioned into a stable static prefix
    /// and a dynamic suffix. The static prefix is only rebuilt when settings,
    /// capabilities, or device state change — allowing local inference providers
    /// to reuse their KV-cache for the stable portion across turns.
    /// Default: true (recommended for local models on memory-constrained devices).
    #[serde(default = "Settings::default_prefix_cache_prompt")]
    pub prefix_cache_prompt: bool,

    /// Which extension tool SCHEMAS reach the model: `"all"` (default) |
    /// `"relevant"` | `"minimal"`.
    ///
    /// `"all"` sends every registered `giap-*` tool on every turn — 27 tools,
    /// ~3,339 tokens, 40.8% of the 8,192-token local prompt budget spent before
    /// the conversation starts.
    ///
    /// `"relevant"` keeps a small always-on core (memory, system, and the
    /// toolkit escape hatch — 778 tokens, 9.5%) plus the groups scored relevant
    /// to the session's opening message, chosen ONCE per session so the KV
    /// prompt prefix stays reusable across turns.
    ///
    /// `"minimal"` keeps only the hatch — 222 tokens, 2.7%, the one setting
    /// that fits a 4% ceiling — and every group arrives when the model asks.
    ///
    /// The model can pull in any dormant group itself via `enable_tool_group`,
    /// so nothing becomes unreachable under either narrowing mode — and this
    /// never decides WHETHER tools are used, only which schemas are in the
    /// prompt.
    ///
    /// Defaults to `"all"`: existing installs see no behaviour change until the
    /// operator opts in.
    #[serde(default = "Settings::default_tool_selection_mode")]
    pub tool_selection_mode: String,

    /// How hard the `SecurityPolicy` bites: `"off" | "audit" | "enforce"`.
    ///
    /// Defaults to `"audit"`, and that default is the whole design. A rules
    /// matrix written from first principles is wrong in ways only real traffic
    /// reveals, and an authorisation regression in a home assistant does not
    /// look like a 403 — it looks like the lights not turning on. So every
    /// decision is evaluated and recorded, and none of them block, until the
    /// audit log says what `enforce` would actually have broken.
    ///
    /// - `off` — no evaluation, no audit entries. For debugging only.
    /// - `audit` — evaluate, record the verdict, never block.
    /// - `enforce` — denials bite.
    ///
    /// Flipping the default to `enforce` is PAI-2 P8 and is gated on a release
    /// spent in `audit` with telemetry to read.
    #[serde(default = "Settings::default_security_policy_mode")]
    pub security_policy_mode: String,

    /// How hard outbound HTTP is gated: `"open" | "allowlist" | "offline"`.
    ///
    /// - `open` - every outbound call is recorded, none is refused.
    /// - `allowlist` - refuse hosts that classify as privacy-`Sensitive`,
    ///   which is every host that is neither loopback nor on the curated
    ///   public-API list in `shared::services::egress`.
    /// - `offline` - refuse everything except loopback, which turns "prove it
    ///   is not phoning home" into one setting rather than a packet capture.
    ///
    /// Defaults to `open` because that is what every existing install already
    /// does; nothing was gated before this landed, so any other default would
    /// break a working pond on upgrade. That makes this a scope-WIDENING
    /// default only in the sense that it preserves the status quo -- the
    /// narrowing this phase owes is at the edge: `PUT /settings` refuses an
    /// unrecognised value rather than absorbing it.
    ///
    /// Read `docs/architecture/pai/02-privacy-and-security-guardrails.md` 3.5
    /// before widening the curated list: the fail-`Sensitive` default is what
    /// makes `allowlist` mean anything.
    #[serde(default = "Settings::default_network_mode")]
    pub network_mode: String,

    /// When true, recent memory fragments are injected into the system prompt each turn
    #[serde(default = "Settings::default_agent_memory_inject")]
    pub agent_memory_inject: bool,

    /// How many memory fragments to inject (most recent first)
    #[serde(default = "Settings::default_agent_memory_limit")]
    pub agent_memory_limit: u32,

    /// When true, tool outputs (weather, Wikipedia, schedules, devices) are
    /// semantically compressed before injection into the LLM context. Saves
    /// 50-80% of tokens on tool results with negligible information loss.
    /// Disable only for debugging raw tool output.
    #[serde(default = "Settings::default_tool_output_compaction")]
    pub tool_output_compaction: bool,

    /// When true, durable facts are automatically extracted from each conversation
    /// turn and stored as categorised memories (segment, importance, decay).
    #[serde(default = "Settings::default_memory_extraction_enabled")]
    pub memory_extraction_enabled: bool,
    /// Compose questions out of the household's own memories, on the lane.
    ///
    /// Separate from `memory_extraction_enabled` because they are different
    /// bargains: extraction decides what the pond REMEMBERS, and this decides
    /// what it OFFERS. A household that wants to be remembered and not
    /// suggested to is a coherent position, and folding the two would make
    /// turning off the offers also stop the remembering.
    #[serde(default = "Settings::default_suggestion_generation_enabled")]
    pub suggestion_generation_enabled: bool,

    /// When true, a background task periodically prunes/archives decayed memories.
    #[serde(default = "Settings::default_memory_cleanup_enabled")]
    pub memory_cleanup_enabled: bool,

    /// When true, a background task periodically merges duplicate/contradicting memories.
    ///
    /// Default ON since 2026-07-28. Off, nothing ever removes what extraction
    /// gets wrong: a device store audited that day was 87% noise — the
    /// assistant's own self-description filed as the user's identity, five
    /// third-party biography facts, and clock readings kept forever. The
    /// consolidation prompt already targets exactly that ("general knowledge,
    /// info already in the system prompt, anything the assistant said rather
    /// than a user fact"), it had simply never been allowed to run. It is
    /// inactivity-triggered and interruptible, so it costs a turn nothing.
    #[serde(default = "Settings::default_memory_consolidation_enabled")]
    pub memory_consolidation_enabled: bool,

    /// Let the pond rename conversations while it is idle.
    ///
    /// A session's first title is the first six words of the first thing said
    /// in it, which is reliable and unmemorable. When this is on, a background
    /// pass replaces those with a name worth reading in the sidebar, and
    /// revisits a name once its conversation has moved substantially past it.
    ///
    /// Shares the inactivity contract with memory consolidation -- never at
    /// startup, only after the idle threshold, and abandoned the instant
    /// somebody comes back -- because both spend the same single on-device
    /// inference slot. A title is never worth taking that slot from a person.
    ///
    /// A name typed by hand is never overwritten, whatever this is set to.
    #[serde(default = "Settings::default_session_titling_enabled")]
    pub session_titling_enabled: bool,

    /// Consolidation mode: "single" (1 LLM call) or "adversarial" (3-stage Proposer/Adversary/Judge).
    #[serde(default = "Settings::default_memory_consolidation_mode")]
    pub memory_consolidation_mode: String,

    /// When true, memory retrieval uses causal graph traversal (experimental).
    /// Edges between memories are followed to inject causally relevant context
    /// rather than only recency-based results.
    #[serde(default)]
    pub memory_graph_enabled: bool,

    /// When true, scheduled task results are broadcast as SSE events / desktop notifications.
    #[serde(default = "Settings::default_schedule_result_notify")]
    pub schedule_result_notify: bool,

    // ── Memory tuning ────────────────────────────────────────────────────────
    /// Base half-life for memory decay in days. Higher-importance memories get
    /// a longer half-life: `adaptive_half_life = base * (1 + importance)`.
    /// Default: 11.25 days.
    #[serde(default = "Settings::default_memory_decay_base_half_life_days")]
    pub memory_decay_base_half_life_days: f32,

    /// Decay curve steepness factor. Lower = gentler decay. Default: 0.8.
    #[serde(default = "Settings::default_memory_decay_beta")]
    pub memory_decay_beta: f32,

    /// Memory decay: effective score below this → prune (delete). Default 0.05.
    #[serde(default = "Settings::default_memory_prune_threshold")]
    pub memory_prune_threshold: f32,

    /// Memory decay: effective score below this → archive (hide). Default 0.15.
    #[serde(default = "Settings::default_memory_archive_threshold")]
    pub memory_archive_threshold: f32,

    /// Memory cleanup background task interval in hours. Default 6.
    #[serde(default = "Settings::default_memory_cleanup_interval_hours")]
    pub memory_cleanup_interval_hours: u32,

    /// Memory consolidation background task interval in hours. Default 24.
    #[serde(default = "Settings::default_memory_consolidation_interval_hours")]
    pub memory_consolidation_interval_hours: u32,

    /// Max memories to process per consolidation batch. Default 50.
    #[serde(default = "Settings::default_memory_consolidation_batch_size")]
    pub memory_consolidation_batch_size: u32,

    /// Most memories one WINDOW may produce. Default 3.
    ///
    /// Per window, not per turn: the unit changed when extraction did. Three
    /// out of twenty messages is deliberately tight -- the prompt says "fewer
    /// is better" -- because a window that yields three memories every time is
    /// a model padding, and padding is what fills a store with rows nobody
    /// wants recalled.
    #[serde(default = "Settings::default_memory_extraction_max_facts")]
    pub memory_extraction_max_facts: u32,

    /// Floor under the gap between batch extraction passes. Default 60.
    #[serde(default = "Settings::default_memory_extraction_interval_secs")]
    pub memory_extraction_interval_secs: u32,

    // ── Batch memory extraction ──────────────────────────────────────────────
    //
    // The batch engine reads one WINDOW of one conversation per lane slot,
    // in the pond's idle time, instead of one turn after every turn. These
    // are its dials. All of them are headless: they tune how often the pond
    // reads its own history and how close two notes have to be before one
    // counts as a restatement of the other, and neither is a question a
    // household can answer from a slider.
    /// Conversations examined per pass. Default 3.
    ///
    /// One window each, so this is also windows per pass. Three at roughly
    /// 10-15 s of inference apiece is ~30-45 s, which is about as long as a
    /// background job should hold the single inference slot before the next
    /// tick reconsiders.
    #[serde(default = "Settings::default_memory_extraction_sessions_per_pass")]
    pub memory_extraction_sessions_per_pass: u32,

    /// Messages in one extraction window. Default 20.
    ///
    /// Bounded by three things at once and the smallest wins: this count, a
    /// character budget, and never splitting a user-assistant pair. A whole
    /// session does not fit the prompt-side clamp, and a turn is the unit this
    /// engine exists to stop using.
    #[serde(default = "Settings::default_memory_extraction_window_messages")]
    pub memory_extraction_window_messages: u32,

    /// How quiet the household must be before a pass may start, in seconds.
    /// Default 900.
    ///
    /// Its own value rather than the shared chore threshold because this job is
    /// the most expensive in the lane and the least urgent: a conversation from
    /// last March does not get staler while the pond waits.
    #[serde(default = "Settings::default_memory_extraction_idle_secs")]
    pub memory_extraction_idle_secs: u32,

    /// Cosine at or above which a candidate is the SAME memory as one already
    /// stored. Default 0.94.
    ///
    /// **This number is a proposal, not a measurement.** It is what the shadow
    /// pass exists to replace: the engine bands every candidate it sees and
    /// logs the histogram without writing anything, so the threshold can be
    /// picked off a real distribution of real wordings from the device's own
    /// model rather than off an intuition.
    #[serde(default = "Settings::default_memory_reinforce_threshold")]
    pub memory_reinforce_threshold: f32,

    /// Cosine at or above which a candidate is ABOUT the same thing as one
    /// already stored, without being the same note. Default 0.78.
    ///
    /// Same caveat as the reinforce threshold above, and more sharply: 0.78 is
    /// the number the whole shadow phase was designed to buy evidence for.
    #[serde(default = "Settings::default_memory_relate_threshold")]
    pub memory_relate_threshold: f32,

    /// Whether a dated utterance becomes a proposal in the suggestion queue.
    /// Default true.
    ///
    /// Dates never become memories — a memory is read six months later with no
    /// conversation around it, and "next Tuesday" is then a lie. The
    /// destination is the proposal queue, which is a thing that exists; it is
    /// not a calendar write and not a sticky note, neither of which does.
    #[serde(default = "Settings::default_memory_date_proposals_enabled")]
    pub memory_date_proposals_enabled: bool,

    /// What the batch engine is allowed to do: `shadow`, `write`, or
    /// `reinforce`. Default `write`.
    ///
    /// Internal state, written by the engine and by whoever is rolling it out —
    /// not a user-facing control. An unrecognised value reads as `shadow`,
    /// which is the narrowing direction: an unreadable mode must not be able to
    /// start writing to the household's memory store.
    ///
    /// `shadow` is what an operator selects to re-measure the two thresholds
    /// against a real history without touching the store. It is no longer the
    /// default: with the per-turn path gone, a pond left in `shadow` reads its
    /// own conversations and remembers nothing.
    #[serde(default = "Settings::default_memory_extraction_mode")]
    pub memory_extraction_mode: String,

    /// When the batch engine first completed a pass, RFC3339; empty until it
    /// has. Internal state, written once by the engine.
    ///
    /// It is the epoch the first-sighting rule is measured against: a memory
    /// the per-turn path wrote before this moment must not be able to reinforce
    /// itself into looking like a habit the first time the backlog re-reads the
    /// conversation it came from.
    #[serde(default = "Settings::default_memory_extraction_first_pass_at")]
    pub memory_extraction_first_pass_at: String,

    // ── Scheduling tuning ────────────────────────────────────────────────────
    /// Max concurrent scheduled task executions. Default 2.
    #[serde(default = "Settings::default_schedule_max_concurrent")]
    pub schedule_max_concurrent: u32,

    /// Max execution history entries retained per schedule. Default 50.
    #[serde(default = "Settings::default_schedule_max_runs_per_task")]
    pub schedule_max_runs_per_task: u32,

    // ── Context monitoring ─────────────────────────────────────────────────
    /// When true, tracks context window fill rate per session and emits
    /// warnings before the context window saturates. Default true.
    #[serde(default = "Settings::default_context_monitor_enabled")]
    pub context_monitor_enabled: bool,

    /// Ask the model, once it stops calling tools, whether the request has
    /// actually been met — and let it keep working if not. Default true.
    ///
    /// Without this the agent loop ends when the model stops asking for tools,
    /// which is not the same as the question being answered. Measured on a Mac
    /// 2026-08-12 with "how old are each of the former Kenyan Presidents?":
    /// gemma-4-E4B went from 4 tool calls and "I was unable to find a list of
    /// the ages" to 7 tool calls and the actual ages. On "what time is it in
    /// the first 10 states alphabetically?" it went from a flat refusal to a
    /// per-state table, having finally found `world_clock`.
    ///
    /// **It costs roughly twice the inferences per turn**, because the check
    /// re-arms every time the model does more work — 3 nudges on one measured
    /// turn, not 1. That is the mechanism rather than a defect: capping it at
    /// one check would have stopped the Kenyan-presidents turn around its
    /// fourth tool call, back at "unable to find".
    ///
    /// A setting and NOT a `ModelClass` tier, though that enum is precisely
    /// "how expensive an extra model call is on this box". Its only cheap tier
    /// is `Large`, which means *served from another box*, so gating on it would
    /// switch this off for every on-device pond — exactly where it was measured
    /// to help most. The axis that actually predicted benefit was model
    /// capability (E4B gained, E2B barely), and GIAP has no honest signal for
    /// that, so this is the household's call rather than a heuristic pretending
    /// to be one.
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
    /// When true, per-turn telemetry metrics (TTFT, token counts, tool latency,
    /// context utilization) are recorded for each chat turn.
    #[serde(default = "Settings::default_telemetry_enabled")]
    pub telemetry_enabled: bool,

    // ── Experimental ────────────────────────────────────────────────────────
    /// When true, the ToolAgent detects multiple tool intents per message
    /// and dispatches them concurrently via `tokio::join_all`.
    /// Experimental — off by default.
    #[serde(default)]
    pub multi_tool_enabled: bool,
    // ── Tool call validation ────────────────────────────────────────────────
    /// When true, LLM tool call outputs are validated and repaired before
    /// execution. Catches common JSON formatting errors from small local models
    /// (3B-4B). Disable if tool calls are already reliable or handled upstream.
    #[serde(default = "Settings::default_tool_call_validation")]
    pub tool_call_validation: bool,

    // ── Post-inference tool request detection ──────────────────────────
    /// When true, the LLM's response is scanned for natural language tool
    /// requests (e.g. "Let me look up X"). If detected, the tool is executed
    /// and the response is revised with the tool data.
    #[serde(default = "Settings::default_tool_request_detection")]
    pub tool_request_detection: bool,

    // ── API keys: NOT HERE, deliberately (PAI-2 P2) ──────────────────────
    //
    // `api_key_guardian`, `api_key_gnews`, `api_key_finnhub` and
    // `api_key_coingecko` used to live on this struct. `GET /api/v1/settings`
    // does `serde_json::to_value(settings)`, so every configured key was in the
    // response body, and at the time this moved `PUT /settings` was on
    // `PUBLIC_ROUTES` for onboarding, so a caller with no token could write one
    // and (before PAI-2 P0) read it straight back. Even once that write path is
    // closed, a credential on this struct is a credential in a REST response
    // body and a plaintext row in `pond_system.db`.
    //
    // Credential material now lives in `SecretRepository`
    // (`pond-core/src/security/ports/secret.rs`), which returns key NAMES and
    // existence only, behind the protected `/api/v1/secrets` routes. The secret
    // names are `GUARDIAN_API_KEY`, `GNEWS_API_KEY`, `FINNHUB_API_KEY` and
    // `COINGECKO_API_KEY`; `pond_infra::secret_migration` moves whatever an
    // existing pond had in its settings table across on first start.
    //
    // `no_settings_field_is_secret_shaped` fails the build if anyone adds one
    // back. Do not silence it with `skip_serializing_if` —
    // `every_declared_settings_field_is_serialized` fails on that too.
    /// Self-hosted SearXNG instance URL for web/news search.
    ///
    /// Stays on `Settings`: it is an endpoint the user needs to see and edit,
    /// not a credential. If a deployment ever needs `user:pass@host` in this
    /// URL it belongs in the secret store instead, and this comment is where
    /// that decision gets revisited.
    #[serde(default)]
    pub searxng_url: Option<String>,

    // ── Extension toggles ───────────────────────────────────────────────
    // Controls which builtin MCP tool modules are registered at startup.
    // External extensions are managed separately via the MCP server repository.
    /// Enable the memory tools module (recall, save, forget).
    #[serde(default = "Settings::default_ext_enabled")]
    pub ext_memory_enabled: bool,

    /// Enable the scheduling tools module (create, delete, pause, resume, list, run_now, get_runs).
    #[serde(default = "Settings::default_ext_enabled")]
    pub ext_schedule_enabled: bool,

    /// Enable the weather tool module.
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

    /// Enable the orchestration tools module — the `delegate` tool, which runs a
    /// saved role as a child agent (PAI-6 P5).
    ///
    /// **Its own default fn, and the only extension toggle that is OFF.** Every
    /// other `ext_*` field reuses [`Settings::default_ext_enabled`], which
    /// returns `true`; reusing it here would ship autonomous multi-turn agents,
    /// running under `GooseMode::Auto` with no approval path, switched on for
    /// every existing install on the next upgrade. Nobody asked for that by
    /// upgrading.
    ///
    /// The direction is also the safe one for a read failure: a settings load
    /// that fails falls back to [`Settings::default`], and a `settings` row that
    /// is absent leaves this field at its default, so both mean OFF.
    #[serde(default = "Settings::default_ext_orchestrator_enabled")]
    pub ext_orchestrator_enabled: bool,

    /// May the pond speak without having been spoken to (PAI-7 P6)?
    ///
    /// **Its own default fn, returning `false`, for the same reason
    /// [`Settings::default_ext_orchestrator_enabled`] has one.** Every
    /// `voice_output.speak()` in this tree today is downstream of a user
    /// utterance or an explicit `/tts` request. An assistant that starts talking
    /// on its own after an upgrade is a bad surprise in a way that a new button
    /// is not, and nobody asked for it by upgrading.
    ///
    /// The direction is also the safe one for a failed read: an unreadable
    /// settings row leaves this at its default, and the gate refuses outright on
    /// a read it could not perform. Both mean silence.
    #[serde(default = "Settings::default_unprompted_speech_enabled")]
    pub unprompted_speech_enabled: bool,

    /// Start of the nightly window in which the pond never speaks unprompted,
    /// local `"HH:MM"` (PAI-7 P6, invariant 6 -- quiet hours are ABSOLUTE).
    ///
    /// Introduced here rather than earlier on purpose: `TimeBoundary` records
    /// that quiet hours "do not exist … P6 introduces them together with the
    /// speech gating that gives them meaning", and a window nothing consults is
    /// a setting that lies.
    ///
    /// Wraps midnight when start > end, which is the normal case and the one
    /// `22:00`/`07:00` takes. Malformed bounds mean silence, never "no quiet
    /// hours" -- see `chat::quiet_hours_cover`.
    #[serde(default = "Settings::default_quiet_hours_start")]
    pub quiet_hours_start: String,

    /// End of the quiet-hours window, local `"HH:MM"`. See
    /// [`Settings::quiet_hours_start`].
    #[serde(default = "Settings::default_quiet_hours_end")]
    pub quiet_hours_end: String,

    /// Which notification categories may be SPOKEN unprompted, comma-separated
    /// (PAI-7 3.4's category gating, over `Notification.category`).
    ///
    /// Defaults to `"alert"` alone -- the narrowest value that leaves the
    /// feature worth switching on. `info` is the category the schedule bridge
    /// uses for every completed task, so a default including it would turn "let
    /// the pond speak" into "the pond reads out every cron line".
    ///
    /// A comma-separated string rather than a `Vec` because the store is a flat
    /// key-value table and the adapter writes one row per field; an unknown or
    /// blank entry is not a category and is dropped, so a typo silences that
    /// category rather than opening the rest.
    #[serde(default = "Settings::default_unprompted_speech_categories")]
    pub unprompted_speech_categories: String,

    /// May the pond reason about what has happened, unasked (PAI-7 P4)?
    ///
    /// **A second toggle rather than a reuse of
    /// [`Settings::ext_orchestrator_enabled`], because they are different
    /// questions.** That one asks whether a model may hand work to a subagent
    /// during a turn the user started. This one asks whether the pond may start
    /// a turn of its own. Somebody who switches delegation on has said the
    /// first, and folding the second into it would have the pond begin forming
    /// opinions about their house as a side effect.
    ///
    /// The reviewer needs BOTH: `should_review` refuses on the orchestrator
    /// toggle first, because with it off there is no `delegate` machinery to
    /// run a child at all, and then on this one through
    /// [`GateInputs::enabled`](crate::user_data::services::consolidation_schedule::GateInputs).
    ///
    /// Its own default fn returning `false`, for the third time in this
    /// workstream and for the same reason each time: nobody asked for it by
    /// upgrading. On the target hardware there is a second cost — a review
    /// holds the only GPU the household's next turn needs.
    ///
    /// [`GateInputs::enabled`]: crate::user_data::services::consolidation_schedule::GateInputs
    #[serde(default = "Settings::default_proactive_review_enabled")]
    pub proactive_review_enabled: bool,

    /// May the pond turn what its own sensors and cameras report into personal
    /// context items (PAI-8's on-pond producer)?
    ///
    /// This is not the same question as "is the camera on". The camera already
    /// records events, and the sensor already records readings; both live in
    /// their own tables under their own retention. This toggle asks whether a
    /// household member's [`ContextSource`] may turn those events into a durable,
    /// per-member corpus that is read back into a model's prompt. That is a
    /// second copy, under a second owner, with a second retention window, and it
    /// is the copy the assistant quotes.
    ///
    /// Its own `default_*` fn returning `false`, which is now the fourth time in
    /// this programme and the reason has not changed: nobody asked for it by
    /// upgrading, and reusing a shared `true`-returning default is precisely how
    /// `ext_orchestrator_enabled` would have shipped delegation on for every
    /// install. There is a second cost specific to this one — the corpus grows
    /// on a Jetson with an 8 GB budget, and the growth is proportional to how
    /// many devices a member follows.
    ///
    /// Off means the producer refuses every event with
    /// [`NotIngested::Disabled`], including through the batch entry point. It
    /// does NOT disable retrieval: a pond that ingested while it was on and then
    /// switched off keeps and can still recall what it has, which is the honest
    /// behaviour for a store the user can also empty by disconnecting the source.
    ///
    /// [`ContextSource`]: crate::context::domain::ContextSource
    /// [`NotIngested::Disabled`]: crate::context::producer::NotIngested::Disabled
    #[serde(default = "Settings::default_context_ingest_enabled")]
    pub context_ingest_enabled: bool,

    /// May the model READ the household's personal-context corpus (PAI-8 P2)?
    ///
    /// Separate from [`Settings::context_ingest_enabled`], which decides whether
    /// anything is STORED, and off for a different reason. Ingest off means an
    /// empty corpus; this off means the corpus exists and `search_context` and
    /// `get_recent_context` are not in the model's tool set.
    ///
    /// **That second thing is not free, which is why it has its own switch.**
    /// Every registered tool's schema goes into every turn's prompt, and on the
    /// target hardware tool schemas are already about 88% of a 4 096-token
    /// window. Two tools that can only ever answer "nothing found" -- which is
    /// every pond until somebody connects a source -- would be a per-turn cost
    /// forever, paid on the device least able to afford it.
    ///
    /// Its own named default fn returning `false`, for the reason the other
    /// off-by-default switches in this file have one: nobody asked for it by
    /// upgrading.
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
            suggestions_muted: Vec::new(),
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
            suggestion_generation_enabled: Self::default_suggestion_generation_enabled(),
            memory_cleanup_enabled: true,
            memory_consolidation_enabled: Self::default_memory_consolidation_enabled(),
            session_titling_enabled: Self::default_session_titling_enabled(),
            memory_consolidation_mode: Self::default_memory_consolidation_mode(),
            memory_graph_enabled: false, // experimental causal graph retrieval
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
            memory_extraction_sessions_per_pass: Self::default_memory_extraction_sessions_per_pass(
            ),
            memory_extraction_window_messages: Self::default_memory_extraction_window_messages(),
            memory_extraction_idle_secs: Self::default_memory_extraction_idle_secs(),
            memory_reinforce_threshold: Self::default_memory_reinforce_threshold(),
            memory_relate_threshold: Self::default_memory_relate_threshold(),
            memory_date_proposals_enabled: Self::default_memory_date_proposals_enabled(),
            memory_extraction_mode: Self::default_memory_extraction_mode(),
            memory_extraction_first_pass_at: Self::default_memory_extraction_first_pass_at(),
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
            // The one `false` in this block, and it must stay a literal `false`
            // rather than `Self::default_ext_enabled()`. See the field.
            ext_orchestrator_enabled: false,
            // PAI-7 P6. Off, and quiet hours already set, so switching speech
            // on later does not also have to remember to set a window.
            unprompted_speech_enabled: false,
            quiet_hours_start: Self::default_quiet_hours_start(),
            quiet_hours_end: Self::default_quiet_hours_end(),
            unprompted_speech_categories: Self::default_unprompted_speech_categories(),
            // PAI-7 P4. Off, like the two above it and for the same reason.
            proactive_review_enabled: false,
            // PAI-8's on-pond producer. Off, and it must stay a literal `false`
            // here as well as in its `default_*` fn: `serde` reads one of the
            // two and `Settings::default()` the other, so a pond can be built
            // through either door.
            context_ingest_enabled: false,
            // PAI-8 P2. Off, like the ingest toggle above it, and for a prompt
            // budget reason as well as a consent one -- see the field.
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
    // 4096 covers most practical assistant replies.  The previous 1024 cap
    // truncated long answers mid-sentence — especially for Harmony-channel
    // models (Gemma 4 / gpt-oss) whose internal `<|channel>thought ...
    // <channel|>` reasoning preamble already eats hundreds of tokens before
    // the visible reply even starts, so 1024 left only ~500 for the answer.
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
    /// 1.0 — the voice at the pace it was trained on.
    fn default_tts_speed() -> f32 {
        1.0
    }
    /// `q8`. See the field docs: the tier that fits beside the language model.
    fn default_tts_quality() -> String {
        "q8".to_string()
    }
    /// `silero`. See the field docs. It was `rms` for exactly one commit, on
    /// the theory that a download should be opt-in; but the download is 2 MB
    /// and happens once, and leaving it opt-in meant every pond shipped with
    /// the detector that cannot tell a fridge from a voice.
    fn default_vad_backend() -> String {
        "silero".to_string()
    }
    /// ON. See the field docs: a household that dislikes the tone can switch it
    /// off, but one that never hears it has nothing to go looking for.
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
    /// Off. See [`Settings::matter_ble_enabled`] for why that is the default
    /// rather than a hedge.
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
        // Brief: the shipped target is an Orin Nano and thinking tokens are
        // decode tokens. See the field docs.
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

    /// Three days — the single source is the constant the trimmer itself uses,
    /// for the same reason the other duration defaults read their
    /// gate's constant: the setting's default and the code's cannot drift.
    fn default_compaction_verbatim_days() -> u32 {
        crate::models::services::context::turn_trimmer::DEFAULT_VERBATIM_DAYS
    }

    /// True since the C1-C3 work landed: the engine session is now hydrated
    /// after a restart, the env knobs follow settings changes, and tool-result
    /// truncation actually reaches the model — so the deterministic trimmer is
    /// the better default than Goose's reactive LLM compaction, which stalls a
    /// turn mid-conversation on-device.
    fn default_hybrid_compaction_enabled() -> bool {
        true
    }

    fn default_agent_backend() -> String {
        "goose".to_string()
    }
    fn default_agent_goose_mode() -> String {
        "auto".to_string()
    }
    /// 50 rather than 20: a multi-step research or home-automation request
    /// routinely needs more than 20 provider calls, and hitting the cap
    /// mid-task strands the user. The rails that actually protect the device
    /// are cancellation, `agent_timeout_secs`, and context-overflow abort — not
    /// a low turn count. `0` opts out of the cap entirely.
    fn default_agent_max_turns() -> u32 {
        50
    }
    /// `0` — no voice-specific cap. Voice gets the same budget as text.
    ///
    /// It was 8, chosen against the #105 harness on the reasoning that chained
    /// two-action utterances complete in 3-5 turns and a runaway loop must not
    /// keep the speaker silent for 20 rounds. Both halves were true; the
    /// conclusion stopped being. `agent_max_turns` moved 20 -> 50 in migration
    /// 0035 precisely because "the 20-turn cap stranded multi-step research and
    /// home-automation requests mid-task" — and voice, where the household
    /// actually asks for those, kept the tightest budget in the pond. The same
    /// request that finishes when typed gives up six times sooner when spoken,
    /// which reads as the assistant being unreliable rather than as a setting.
    ///
    /// The latency worry is now covered by things that bound the wait directly
    /// rather than by proxy: `agent_timeout_secs` stops a stalled turn, the
    /// thinking tone means the wait is not silent, and a spoken barge-in stops
    /// a turn that has gone wrong. Capping *steps* to bound *time* also priced
    /// a cheap tool round the same as an expensive one.
    ///
    /// Still settable: a household that would rather be cut off than wait can
    /// put a number back, and it is still clamped to `agent_max_turns`.
    fn default_voice_max_turns() -> u32 {
        0
    }

    /// The agent-loop turn cap for a request, honouring the voice-specific
    /// tuning (#105): voice requests use the tighter `voice_max_turns` so a
    /// chained command still completes but a runaway loop can't keep the
    /// speaker silent for the full text-chat budget. `voice_max_turns == 0`
    /// disables the voice-specific cap; `agent_max_turns == 0` means uncapped
    /// text reasoning (rendered as [`UNCAPPED_MAX_TURNS`]).
    ///
    /// An uncapped text budget does NOT lift a voice cap: voice latency is a
    /// separate concern, so a non-zero `voice_max_turns` still binds.
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

    /// Whether this request's reasoning length is effectively unbounded — i.e.
    /// [`effective_max_turns`] returned the sentinel rather than a real budget.
    /// Callers use it to tell the model "keep going until the task is done"
    /// instead of quoting a meaningless step count.
    ///
    /// [`effective_max_turns`]: Settings::effective_max_turns
    pub fn turns_are_uncapped(&self, voice: bool) -> bool {
        self.effective_max_turns(voice) == UNCAPPED_MAX_TURNS
    }

    /// Whether per-session tool-relevance selection (Phase D2) is active.
    ///
    /// Anything other than the exact opt-in string means "all tools" — an
    /// unrecognised value must never silently narrow the model's tool surface.
    pub fn tool_selection_is_relevant(&self) -> bool {
        self.tool_selection_mode == TOOL_SELECTION_MODE_RELEVANT
    }

    /// Whether the session is offered the toolkit escape hatch and nothing else.
    ///
    /// Same exact-string discipline as `tool_selection_is_relevant`: an
    /// unrecognised value must never silently narrow the tool surface, and this
    /// mode narrows it further than any other.
    pub fn tool_selection_is_minimal(&self) -> bool {
        self.tool_selection_mode == TOOL_SELECTION_MODE_MINIMAL
    }

    /// Whether ANY narrowing is in force — the gate on the per-session group
    /// machinery (resolution, persistence, the dormant-groups note, and the
    /// `enable_tool_group` bound).
    ///
    /// Both narrowing modes need that machinery: "minimal" needs it more, since
    /// every capability past the hatch is reached through it. Gating on
    /// `tool_selection_is_relevant()` alone would leave "minimal" sessions
    /// unable to persist a group the model had just enabled.
    pub fn tool_selection_narrows(&self) -> bool {
        self.tool_selection_is_relevant() || self.tool_selection_is_minimal()
    }
    fn default_agent_timeout_secs() -> u64 {
        300
    }
    fn default_tool_selection_mode() -> String {
        // "all" so this first landing changes nothing for existing installs.
        TOOL_SELECTION_MODE_ALL.to_string()
    }
    fn default_security_policy_mode() -> String {
        // Audit, never enforce, on a first landing. See the field docs.
        SECURITY_POLICY_MODE_AUDIT.to_string()
    }
    fn default_network_mode() -> String {
        // Open: nothing was gated before this landed, so anything stricter
        // breaks a working pond on upgrade. See the field docs.
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
    /// On by default.
    ///
    /// The template tier answers whether or not this runs, so the cost of it
    /// being on is one model call per idle period and the cost of it being off
    /// is a household reading the same three questions forever — which is the
    /// complaint this whole surface was built from.
    fn default_suggestion_generation_enabled() -> bool {
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
    /// Sixty, matching the lane's own tick.
    ///
    /// This was ten: the rate limit on a per-turn extractor that ran after
    /// every exchange and needed stopping from spending inference twice in a
    /// chatty minute. That reader is gone, and the key now means the one thing
    /// left for it to mean -- the floor under how often a batch pass may take
    /// the inference slot. Ten would let a pass start on every tick; sixty is
    /// the tick, so the floor and the poll agree by default and the key only
    /// ever makes passes RARER.
    fn default_memory_extraction_interval_secs() -> u32 {
        60
    }
    fn default_memory_extraction_sessions_per_pass() -> u32 {
        3
    }
    fn default_memory_extraction_window_messages() -> u32 {
        20
    }
    fn default_memory_extraction_idle_secs() -> u32 {
        900
    }
    fn default_memory_reinforce_threshold() -> f32 {
        0.94
    }
    fn default_memory_relate_threshold() -> f32 {
        0.78
    }
    fn default_memory_date_proposals_enabled() -> bool {
        true
    }
    /// `write`, because the per-turn extraction path is gone.
    ///
    /// This was `shadow` while both paths were live: a pond upgrading into the
    /// release that first contained the engine must not start writing to the
    /// household's memory store because a new background job appeared, and the
    /// per-turn path was still there doing the writing.
    ///
    /// That argument inverts at the cutover. With nothing else extracting,
    /// `shadow` would mean a pond that reads its own conversations, bands every
    /// candidate, and remembers nothing at all -- forever, silently, with a
    /// memory section that never grows. `shadow` remains a mode an operator can
    /// select to re-measure a threshold; it is no longer a safe default,
    /// because the thing it was safe relative to no longer exists.
    ///
    /// An unrecognised value still reads as `shadow`. That has not changed and
    /// must not: a typo in a settings row may cost a pass, never a write.
    fn default_memory_extraction_mode() -> String {
        "write".to_string()
    }
    fn default_memory_extraction_first_pass_at() -> String {
        String::new()
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

    /// PAI-6 P5. Deliberately NOT [`Self::default_ext_enabled`].
    ///
    /// A separate function rather than a `#[serde(default)]` (which would also
    /// give `false`) so that the divergence is a named thing a reader trips over
    /// while adding the next toggle, and so that
    /// `the_orchestrator_toggle_defaults_off_by_its_own_route` has a symbol to
    /// assert on rather than only a value.
    fn default_ext_orchestrator_enabled() -> bool {
        false
    }

    /// PAI-7 P6. Deliberately NOT a bare `#[serde(default)]`, for the reason
    /// [`Self::default_ext_orchestrator_enabled`] is not one either: a named
    /// function is a symbol
    /// `the_unprompted_speech_toggle_defaults_off_by_its_own_route` can assert
    /// on, so flipping this on becomes an edit to a failing test rather than a
    /// one-character change nothing notices.
    fn default_unprompted_speech_enabled() -> bool {
        false
    }

    /// 22:00 local. Quiet hours exist on a fresh install rather than having to
    /// be discovered: the field that decides whether the pond speaks at all is
    /// the one that is off, and a household that switches speech on should not
    /// have to also remember to switch silence on.
    fn default_quiet_hours_start() -> String {
        "22:00".to_string()
    }

    /// 07:00 local.
    fn default_quiet_hours_end() -> String {
        "07:00".to_string()
    }

    /// `alert` only. See the field: `info` carries every completed scheduled
    /// task, so including it by default would turn this feature into the pond
    /// reading out its own cron log.
    fn default_unprompted_speech_categories() -> String {
        "alert".to_string()
    }

    /// PAI-7 P4. A named function for the third time, and by now the pattern is
    /// the point: every switch in this workstream that lets the pond act on its
    /// own has a symbol a test can assert is `false`, so turning one on is an
    /// edit to a failing test rather than a character somebody changed.
    fn default_proactive_review_enabled() -> bool {
        false
    }

    /// PAI-8's on-pond producer. A named function for the fourth time, for the
    /// reason the third one records: turning one of these on must be an edit to
    /// a failing test, not a character somebody changed.
    fn default_context_ingest_enabled() -> bool {
        false
    }

    /// PAI-8 P2. See the field: off for a prompt-budget reason as well as a
    /// consent one, which is why it is separate from the ingest toggle rather
    /// than folded into it.
    fn default_ext_context_enabled() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Validate the STRUCTURE of an adoption registry against the field set
    /// `Settings` has today. Returns every problem found, so the synthetic
    /// tests below can prove each rule actually fires.
    ///
    /// The rules, and why each one exists:
    ///
    /// - The key must be a real `Settings` field, or the migration UPDATEs a
    ///   row nothing ever reads.
    /// - `old_default != new_default`, or the entry is a no-op.
    /// - Entries for the same key CHAIN in ascending migration order, each
    ///   `old_default` picking up where the previous `new_default` left off.
    ///   Without that, an install that already adopted the first change is
    ///   skipped by the second.
    ///
    /// Deliberately NOT here: "the newest entry states today's default". That
    /// needs the literal the STORE holds, and this crate can only guess at it —
    /// a `serde_json` render disagrees with the adapter's `Display` render for
    /// every float, so the rule would reject correct float entries and demand
    /// a widened literal no migration could ever match. pond-infra's
    /// `every_adoption_entry_states_the_literal_the_adapter_writes` asserts it
    /// against the real write instead.
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

    /// A default is allowed to move twice: two entries for one key chain. No
    /// live example exists yet, so prove the checker accepts the shape the docs
    /// tell people to write.
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

    /// Each rule must actually fire — a checker that accepts everything is
    /// worse than none, because the docs promise it catches these.
    #[test]
    fn adoption_defects_are_rejected() {
        let defaults = Settings::default();

        // A second entry that restates the ORIGINAL old value skips every
        // install that already ran the first migration.
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
        assert!(s.tool_call_validation); // on by default
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
        // Unchanged fields keep defaults
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
        assert_eq!(s.llm_max_tokens, 4096); // bumped from 1024 to fit Harmony preambles + long replies
        assert_eq!(s.timezone, "UTC"); // default
    }

    #[test]
    fn tool_call_validation_toggleable() {
        let json = r#"{"tool_call_validation": false}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(!s.tool_call_validation);
        // Other fields keep defaults
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
        // Non-specified fields keep defaults
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

    /// An old client (or a stale phone build) still sends `api_key_guardian`.
    /// That must be ignored, not rejected: the field is gone, and a 422 here
    /// would break every settings save from a client that has not been updated.
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
        // Devices exist but the user controls privacy — mic/cameras default ON.
        assert!(s.mic_enabled);
        assert!(s.cameras_enabled);
        // Privacy-first: cloud fallback is OFF (opt-in) by default.
        assert!(!s.cloud_fallback_enabled);
        // Home name is empty until the user sets one.
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
        // Unspecified fields keep defaults.
        assert!(s.cameras_enabled);
        assert!(!s.cloud_fallback_enabled);
    }

    #[test]
    fn partial_overrides_preserve_new_defaults() {
        let json = r#"{"ext_weather_enabled": false}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(!s.ext_weather_enabled);
        // Other toggles keep their defaults
        assert!(s.ext_memory_enabled);
        assert!(s.ext_schedule_enabled);
        // searxng_url still None
        assert!(s.searxng_url.is_none());
    }

    /// Out of the box, a spoken request gets the same budget as a typed one.
    ///
    /// This asserted `8` for voice against `50` for text — #105's trade of
    /// completeness for latency. The trade is still available (see the test
    /// below) but is no longer the default: the same multi-step request
    /// completing when typed and stopping six turns in when spoken is
    /// indistinguishable, from the room, from the assistant being unreliable.
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

    /// The voice cap still works — it is defaulted off, not removed.
    ///
    /// A household that would rather be cut off than wait can set one, and it
    /// must still bind. Without this, defaulting the value to 0 could silently
    /// become "the voice cap is ignored" and nobody would notice until someone
    /// set it and nothing changed.
    #[test]
    fn a_configured_voice_cap_still_binds() {
        let mut s = Settings::default();
        s.voice_max_turns = 8;
        assert_eq!(s.effective_max_turns(true), 8);
        assert_eq!(s.effective_max_turns(false), 50, "text is unaffected");
    }

    /// B1: `agent_max_turns = 0` means uncapped reasoning — the engine gets the
    /// sentinel, not 0 (which would stop the loop before its first turn).
    #[test]
    fn zero_agent_max_turns_means_uncapped() {
        let mut s = Settings::default();
        s.agent_max_turns = 0;
        assert_eq!(s.effective_max_turns(false), UNCAPPED_MAX_TURNS);
        assert!(s.turns_are_uncapped(false));
        // Sanity: the sentinel is safe to do arithmetic on.
        assert!(UNCAPPED_MAX_TURNS.checked_mul(2).is_some());
    }

    /// B1: an uncapped TEXT budget must not lift the voice cap — voice latency
    /// is a separate concern and `voice_max_turns` keeps its own meaning.
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

    /// D2: the default must not narrow anything, and only the exact opt-in
    /// string may. A typo'd or future value silently shrinking the model's tool
    /// surface would be the worst failure mode of this feature.
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

    /// The same exactness rule for "minimal", and the gate that decides whether
    /// the per-session group machinery runs at all.
    ///
    /// `tool_selection_narrows()` returning false for "minimal" would put those
    /// sessions back on the unnarrowed path with the full tool surface -- the
    /// feature silently off, the setting still reading "minimal". Nothing
    /// exercised either method when they were added beside
    /// `tool_selection_is_relevant`, which is how a gate like that stays broken.
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

    /// Every accepted value must be reachable through the validator, or the
    /// mode exists in the domain and is refused at the API.
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

    /// A real cap is never reported as uncapped.
    #[test]
    fn a_real_cap_is_not_uncapped() {
        let s = Settings::default();
        assert!(!s.turns_are_uncapped(false));
        assert!(!s.turns_are_uncapped(true));
    }

    /// #105: the voice cap can only tighten the budget, never extend it.
    #[test]
    fn effective_max_turns_never_exceeds_agent_max_turns() {
        let mut s = Settings::default();
        s.agent_max_turns = 5;
        s.voice_max_turns = 50;
        assert_eq!(s.effective_max_turns(true), 5);
    }

    /// #105: 0 disables the voice-specific cap (falls back to agent_max_turns).
    #[test]
    fn effective_max_turns_zero_disables_voice_cap() {
        let mut s = Settings::default();
        s.voice_max_turns = 0;
        assert_eq!(s.effective_max_turns(true), s.agent_max_turns);
    }

    /// The source of this file, so the structural guards below can compare what
    /// is DECLARED against what is SERIALIZED. `include_str!` resolves relative
    /// to this file, so this is this file. It is textual inclusion into a string
    /// literal, so there is no module recursion.
    const SETTINGS_SOURCE: &str = include_str!("settings.rs");

    /// Words that mark a field name as carrying credential material.
    ///
    /// Matched against `_`-separated SEGMENTS, not as a suffix. The shape that
    /// actually leaked was `api_key_guardian`, which ends in neither `_key` nor
    /// `_token`; a suffix test would have missed all four.
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

    /// Serialized keys whose NAME is secret-shaped but whose VALUE is not
    /// credential material. Every entry is a deliberate exemption and should be
    /// argued in review; the list is meant to stay very short.
    const NOT_ACTUALLY_SECRET: &[&str] = &[
        // A token BUDGET (a `u32`), not a bearer token.
        "llm_max_tokens",
        // An exchange RATE (millisats per usage-token, a `u64`), not a
        // bearer token — see the field's own doc comment (#132 Milestone 6).
        "mesh_settlement_millisats_per_token",
        // A token COUNT ceiling (a `u64`), not a bearer token — see the
        // field's own doc comment.
        "mesh_lend_token_ceiling",
    ];

    /// PAI-2 P2, section 3.2 item 2.
    ///
    /// `GET /api/v1/settings` serialises this whole struct, so a secret-shaped
    /// field on `Settings` is a secret in a REST response body. Four
    /// `api_key_*` fields were exactly that. This test is what stops the next
    /// person adding a fifth.
    #[test]
    fn no_settings_field_is_secret_shaped() {
        // Positive control FIRST: prove the detector detects. A guard whose
        // predicate silently matches nothing passes for the wrong reason, and
        // this programme has four recorded instances of exactly that.
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

    /// Closes the hole in every other guard in this file.
    ///
    /// `no_settings_field_is_secret_shaped` and
    /// `every_settings_field_is_dispositioned` both enumerate the SERIALIZED
    /// keys of `Settings::default()`. A field carrying
    /// `#[serde(skip_serializing_if = "Option::is_none")]` is absent from that
    /// enumeration whenever it is `None` — which is exactly what
    /// `Settings::default()` is. So a future `api_key_*` field with that
    /// attribute would pass both guards and still be serialised, in plaintext,
    /// the moment a user configured it. Comparing declarations against
    /// serialized keys is what makes the other two mean what they say.
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

    /// Completeness / disposition guard (Phase 4 — "Consistent").
    ///
    /// Every field serialized from `Settings` MUST be classified as either
    /// UI_WIRED (surfaced in the desktop Settings/onboarding UI, with a TS
    /// mirror in `pond-desktop/src/api/types.ts`) or HEADLESS_BY_DESIGN (an
    /// advanced/backend-managed knob with no UI). Adding a new `Settings` field
    /// fails this test until it is placed in one of the two lists — forcing a
    /// conscious decision (wire UI + TS mirror, or document it as headless) and
    /// preventing the "TS-only / silently-dropped-on-save" class of bug that
    /// Phase 1 fixed (mic/cameras/cloud_fallback/home_name).
    ///
    /// To update after adding a field: add its serialized key to UI_WIRED (and
    /// wire it into `Settings.tsx` + `types.ts`) or to HEADLESS_BY_DESIGN.
    #[test]
    fn every_settings_field_is_dispositioned() {
        // Advanced retention knobs, tuned via backend/config — intentionally no UI.
        const HEADLESS_BY_DESIGN: &[&str] = &[
            // No control in the desktop app yet. HEADLESS_BY_DESIGN rather than
            // UI_WIRED for the reason the note above gives: this list asserts
            // whether a control EXISTS, and claiming one that does not is how
            // twenty-two switches came to render without being operable.
            "goal_check_enabled",
            // The privacy policy's rollout lever (PAI-2 P1). An operator knob
            // while the matrix is being validated against real households; it
            // gets a UI only if it survives to `enforce` (PAI-2 P8), and giving
            // it one now would invite flipping a half-validated matrix on.
            "security_policy_mode",
            // The egress gate's rollout lever (PAI-2 P5). Headless because
            // while any real-egress call site was still ungated (see
            // crates/pond-core/tests/egress_guard.rs), a UI switch labelled
            // "offline" would have promised more than the code delivered. The
            // condition for giving it a control was "when UNGATED_SENDERS is
            // empty".
            //
            // THAT CONDITION IS NOW MET. PAI-2 P6b gated the last file,
            // `pond-api/src/routes.rs`; `UNGATED_SENDERS` is empty and
            // `MAX_UNGATED` is 0. This stays HEADLESS_BY_DESIGN only because
            // shipping the control is a `Settings.tsx` + `types.ts` change that
            // belongs to whoever owns those files, not because the reason still
            // holds. Reclassifying it here without that UI would be worse than
            // leaving it: the completeness test checks the CLASSIFICATION, not
            // whether a control exists, so a premature UI_WIRED would assert
            // something untrue and silence the only guard on it. The follow-up
            // is a three-value control (open / allowlist / offline) on the
            // Privacy section, and it is the last thing PAI-2 P6 owes.
            //
            // Read UNGATED_SENDERS rather than this comment for the current
            // count -- a number written in prose is the thing that goes stale.
            "network_mode",
            // Hybrid-compaction rollout flags: operator knobs for the
            // deterministic-trim + idle-summary pipeline; flipped via the
            // settings API during on-device burn-in, no UI control planned.
            "hybrid_compaction_enabled",
            "summary_idle_secs",
            // PAI-4 P4's threshold. Headless for the same reason its two
            // neighbours are: it tunes when the pipeline reshapes history, not
            // what the household can see or decide. A control would also be a
            // trap — the damaging direction is *shorter*, and a slider inviting
            // "compact more often" would invite exactly that.
            // PAI-4 P3's verbatim horizon. Headless with its neighbours, and
            // for a sharper version of the same reason: the damaging direction
            // is *shorter*, and the only honest UI label for it ("how many days
            // of history stay full-fidelity") describes a rung that fires only
            // under budget pressure a household cannot see. Exposing a number
            // whose effect is invisible most of the time invites tuning by
            // superstition.
            "compaction_verbatim_days",
            "retention_events_days",
            "retention_events_by_category",
            "retention_sensitive_days",
            // Voice-latency tuning knob (#105): the default is derived from the
            // command-chaining harness; operators override via the settings API.
            "voice_max_turns",
            // Vision classifier model file (#130 follow-up): an operator knob
            // that also requires a `vision-onnx` build; UI wiring comes with
            // the Models-tab vision section, not before.
            // No control yet: the model is a download and the backend is opt-in,
            // so this ships headless and moves to UI_WIRED in the same change
            // that adds the switch. Claiming a control that does not exist is
            // how twenty-two switches came to render without being operable.
            "vad_backend",
            "vision_classifier_model",
            // PAI-8's on-pond producer, headless for the same reason and owing
            // a UI for a sharper one: this switch decides whether what the
            // household's cameras and sensors saw is copied into a per-member
            // corpus the assistant quotes back. It is off, so nothing is
            // reachable without an API call -- but "which of my devices feed my
            // context" is a question a person should be able to answer in the
            // app, and the source list that answers it has no UI either. The
            // control belongs on the Privacy section next to `network_mode`,
            // and shipping it is a `Settings.tsx` + `types.ts` change owned by
            // whoever owns those files.
            "context_ingest_enabled",
            "ext_context_enabled",
            // Private mesh Lightning settlement (#132 Milestone 5): connects
            // to Breez/Spark for mesh-peer invoices. No UI yet; requires a
            // `pond-server` build with the `lightning` feature plus a
            // `BREEZ_API_KEY` env var.
            "lightning_enabled",
            // Private mesh settlement exchange rate (#132 Milestone 6): the
            // credit-to-sats conversion is an open product decision, not yet
            // made — see SettlementService's own docs. No UI until it is.
            "mesh_settlement_millisats_per_token",
            // Private mesh lend-side throttle: what number is reasonable is
            // an open product decision, same as the rate above. No UI yet.
            "mesh_lend_token_ceiling",
            // The batch memory-extraction engine's dials. Headless as a group,
            // for two different reasons.
            //
            // The three cadence knobs tune how much of the single inference
            // slot the pond spends reading its own history. A control for them
            // would be a slider whose effect a household cannot observe, which
            // invites tuning by superstition -- the same argument that keeps
            // `compaction_verbatim_days` headless.
            //
            // The two thresholds are worse than unobservable: they are
            // UNMEASURED. 0.94 and 0.78 are proposals the shadow pass exists to
            // replace with numbers off a real histogram. Shipping a control for
            // a number nobody has measured would invite a household to tune a
            // dial whose units do not mean anything yet.
            "memory_extraction_sessions_per_pass",
            "memory_extraction_window_messages",
            "memory_extraction_idle_secs",
            "memory_reinforce_threshold",
            "memory_relate_threshold",
            // The date destination. It gets a UI in the same change that gives
            // the proposal queue an executor -- until approving a proposal
            // actually does something, a switch labelled "turn reminders on"
            // would promise more than the code delivers.
            "memory_date_proposals_enabled",
            // Engine state rather than settings: `mode` is the rollout lever
            // (shadow -> write -> reinforce) and `first_pass_at` is an epoch
            // the engine stamps itself. Neither is a preference, and giving a
            // household a control that flips an engine straight from reading to
            // writing its memory store is the opposite of a rollout.
            "memory_extraction_mode",
            "memory_extraction_first_pass_at",
        ];
        // Everything else is surfaced in the desktop UI (Settings tabs / hub
        // views / onboarding) and mirrored in the TS Settings type.
        const UI_WIRED: &[&str] = &[
            // The suggestion engine's per-kind mute. UI_WIRED rather than
            // HEADLESS_BY_DESIGN because a control that writes it really
            // exists and a household can operate it -- but note WHERE it is:
            // "Don't suggest this" on the Home suggestion card, not a row in
            // Settings.tsx. This list asserts that a control exists, which is
            // true; it does not assert which screen holds it. The TS mirror is
            // in `pond-desktop/src/api/types.ts` like every other entry here.
            "suggestions_muted",
            // Private mesh (#132 Milestone 2): the Mesh section's toggle
            // (Mesh.tsx) starts/stops the real libp2p MeshTransport. Requires
            // a `pond-server` build with the `mesh` feature — flipping it on
            // a build without that feature is a no-op the transport-builder
            // warns about, not a UI error.
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
            "suggestion_generation_enabled",
            "memory_graph_enabled",
            "memory_prune_threshold",
            "mic_enabled",
            "multi_tool_enabled",
            "persist_thinking",
            "prefix_cache_prompt",
            "primary_profile_id",
            "prompt_addendum",
            "prompt_style",
            // PAI-7 P4 and P6's five. They were HEADLESS_BY_DESIGN for a day
            // with a note saying they owed a UI more than the tuning knobs did,
            // because `unprompted_speech_enabled` decides whether the assistant
            // talks to you unasked and a household cannot consent to a feature
            // it cannot see. They have one now, under "Speaking and acting
            // unprompted" in `Settings.tsx`, with the quiet-hours bounds as
            // free text rather than a time picker -- a value the server cannot
            // parse means silence, and a picker renders such a value as blank,
            // which reads as "not set".
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
            // Headless since 2026-08-13, and only until `search_web` comes back:
            // it is the sole reader, and it is no longer registered as a tool
            // (`pond-mcp-server/src/discovery.rs`). The row was removed from
            // Settings.tsx rather than left inert -- an input that cannot affect
            // anything is the "switches that were not switches" defect, and the
            // value is still persisted, so restoring the tool restores the
            // setting with whatever was last typed into it.
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

    /// PAI-6 P5. `ext_orchestrator_enabled` is the one extension toggle that
    /// defaults OFF, and there are three separate places it could silently
    /// become `true`, so all three are asserted.
    ///
    /// The third is the one that matters: every other `ext_*` field reuses
    /// [`Settings::default_ext_enabled`], which returns `true`. Writing
    /// `#[serde(default = "Settings::default_ext_enabled")]` on this field —
    /// the obvious copy-paste — compiles, passes the disposition test above,
    /// passes the settings roundtrip in `pond-infra`, and turns delegation on
    /// for every install that upgrades into it.
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

    /// PAI-7 P6, and the same three routes as the orchestrator toggle above,
    /// because this one has the same shape: a `bool` that must be `false` on
    /// every pond that upgrades into the release containing it.
    ///
    /// The difference is what "on" costs. A delegation toggle switched on
    /// wrongly runs an agent nobody asked for; a speech toggle switched on
    /// wrongly means a machine starts talking in somebody's house.
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

        // Vacuity control: the deserialization above really did produce a
        // populated Settings rather than something that answers `false` to
        // everything. Without this, the assertion holds against a struct whose
        // every bool is false for the wrong reason.
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

    /// PAI-8's on-pond producer, and the same three routes for the fourth time.
    ///
    /// What "on" costs here is different again from its two neighbours: not an
    /// agent nobody asked for and not a machine talking, but a second durable
    /// copy of everything the household's cameras and sensors saw, owned by one
    /// named member and quoted back into a model's prompt. A pond that upgrades
    /// into this release must not start making that copy.
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

        // Vacuity controls: the empty payload really did populate the struct,
        // and a serde default that is genuinely `true` survives it. Without
        // these, "false" holds against a value that is false for every field.
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

    /// Quiet hours ship SET rather than empty, and the categories ship at their
    /// narrowest. Both are what a household gets the moment somebody enables
    /// speech, and neither is something they will be prompted to choose.
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

    /// The other direction, and the one that survives a field being added
    /// tomorrow: derive the set of extension toggles that default off from the
    /// serialized struct rather than from a list written today.
    ///
    /// It fails if one of them flips ON (the set shrinks), and it fails if a NEW
    /// `ext_*` toggle arrives defaulting off (the set grows) — which is a
    /// decision that should be made deliberately rather than inherited.
    ///
    /// **It has fired once, and worked.** It was
    /// `exactly_one_extension_toggle_ships_switched_off` until PAI-8 P2 added
    /// `ext_context_enabled`, and updating it was the deliberate decision it
    /// exists to force. The two are off for related but distinct reasons, worth
    /// keeping separate because a later reader will be tempted to collapse them:
    ///
    /// - `ext_orchestrator_enabled` — turning it on means an autonomous
    ///   multi-turn agent running under `GooseMode::Auto` on the household's own
    ///   hardware. A consent question.
    /// - `ext_context_enabled` — turning it on puts two tool schemas into every
    ///   turn's prompt, and until somebody connects a source they can only
    ///   answer "nothing found". A consent question AND a prompt-budget one, on
    ///   a device where tool schemas are already ~88% of a 4 096-token window.
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
        // Sorted, because the map's own order is not this test's business and
        // is not even stable across builds: something in the workspace enables
        // `serde_json/preserve_order`, so a `Value::Object` iterates in STRUCT
        // DECLARATION order under a workspace build and ALPHABETICALLY when
        // pond-core is built alone. This assertion is about the set that ships
        // off; comparing an unsorted vec made the whole gate red on `cargo test
        // -p pond-core -p pond-api …` and green on `-p pond-core --lib`, which
        // reads as a flake and trains a reader to re-run rather than look.
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
