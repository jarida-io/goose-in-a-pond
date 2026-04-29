// ────────────────────────────────────────────────────────────
// GIAP Desktop — API Type Definitions
// Mirror of pond-server REST API shapes
// ────────────────────────────────────────────────────────────

export interface HealthResponse {
  status: string;
  version?: string;
  uptime_seconds?: number;
}

// ── Settings ─────────────────────────────────────────────────
export interface Settings {
  // Identity
  primary_profile_id?: string | null;
  assistant_name: string;
  user_name: string;
  assistant_personality?: string;
  location?: string;       // legacy alias
  personality?: string;    // legacy alias
  timezone?: string;

  // Voice pipeline
  voice_wake_word?: string;
  wake_word?: string;      // legacy alias
  voice_wake_word_transcriptions?: string[];
  voice_recording_duration_secs?: number;
  voice_whisper_url?: string;
  active_whisper_model?: string;
  active_tts_model?: string;
  voice_tts_voice?: string;

  // Model roles
  chat_provider?: string;
  chat_model?: string;
  tool_model?: string | null;
  thinking_mode?: string;
  show_thinking?: boolean;
  review_mode?: string;
  review_max_rounds?: number;
  review_pass_threshold?: number;
  llm_provider?: string;
  llm_temperature?: number;
  llm_max_tokens?: number;
  active_llm_model?: string;

  // Prompts
  prompt_style: string;
  custom_system_prompt?: string | null;
  prompt_addendum?: string;

  // Location / Weather
  weather_enabled?: boolean;
  weather_location_name?: string;
  weather_latitude?: number;
  weather_longitude?: number;
  lat?: number;  // legacy alias
  lon?: number;  // legacy alias

  // Agent behaviour
  agent_goose_mode?: string;
  agent_max_turns?: number;
  agent_memory_inject: boolean;
  agent_memory_limit?: number;

  // Data retention
  retention_event_log_days?: number;
  retention_sensor_days?: number;
  retention_session_messages_keep?: number;
}

// ── Devices ───────────────────────────────────────────────────
export interface Device {
  id: string;
  name: string;
  device_type?: string;
  room?: string;
  is_online: boolean;
  last_seen?: string;
  metadata?: Record<string, unknown>;
}

// ── Schedules ─────────────────────────────────────────────────
export interface Schedule {
  id: string;
  name: string;
  cron: string;
  prompt: string;
  enabled: boolean;
  created_at?: string;
}

// ── Memory ────────────────────────────────────────────────────
export interface MemoryFragment {
  id: string;
  content: string;
  tags?: string[];
  created_at: string;
}

// ── User Skills ───────────────────────────────────────────────
export interface UserSkill {
  id: string;
  name: string;
  content: string;
  active: boolean;    // backend field name
  enabled?: boolean;  // alias — some code uses this; prefer active
  created_at?: string;
}

// ── Models ────────────────────────────────────────────────────
export interface ModelEntry {
  id: string;
  provider: string;
  name: string;
  display_name?: string;
  is_active: boolean;
  ram_estimate_mb?: number;
  recommended_role?: string;
}

export interface ModelMemoryStatus {
  total_mb: number;
  available_for_llm_mb: number;
  loaded_model: string | null;
}

export interface ModelCapabilities {
  thinking: boolean;
  vision: boolean;
  audio_input: boolean;
  context_window_tokens: number;
  structured_output: boolean;
}

// ── Prompt Templates ──────────────────────────────────────────
export interface PromptTemplate {
  name: string;
  content: string;
  is_system: boolean;
  updated_at?: string;
}

// ── Agent ─────────────────────────────────────────────────────
export interface PromptExtra {
  key: string;
  content: string;
  enabled: boolean;
}

export interface AgentTool {
  extension: string;
  name: string;
  description?: string;
}

export interface AgentRecipe {
  name: string;
  description?: string;
  yaml: string;
}

// ── Chat / Streaming ──────────────────────────────────────────
export type ChatEventType = "text" | "thinking" | "tool_call" | "tool_result" | "done" | "error" | "status" | "review_status" | "review_revision";

export interface ChatEvent {
  type: ChatEventType;
  content?: string;         // for "text" events
  token?: string;           // legacy backend alias for content
  tool?: string;            // for "tool_call" events
  result?: unknown;         // for "tool_call" events
  error?: string;           // for "error" events
  done?: boolean;
  session_id?: string;      // present on done events
  model_role?: string;      // present on done events (chat | think | task)
  model_name?: string;      // present on done events — name of the model that responded
  usage?: {                 // token usage — present on done events when provider reports it
    prompt_tokens: number;
    completion_tokens: number;
  };
}

// ── Transcription ─────────────────────────────────────────────
export interface TranscribeResponse {
  text: string;
}

// ── Wake-word Calibration ────────────────────────────────────
export interface CalibrateResponse {
  transcript:    string;
  normalized:    string;
  all_variants:  string[];
  sample_count:  number;
  target_count:  number;
  complete:      boolean;
}

// ── Auth / Handshake ──────────────────────────────────────────
export interface HandshakeResponse {
  token: string;
  expires_in?: number;
}

// ── Model Role Assignments ────────────────────────────────────
export interface ModelRoleAssignment {
  provider: string;
  model: string;
}

export interface ModelActiveRoles {
  chat:  ModelRoleAssignment | null;
  tool:  { model: string | null } | null;
  asr:   ModelRoleAssignment | null;
  tts:   ModelRoleAssignment | null;
}

// ── Sessions ──────────────────────────────────────────────────
export interface SessionSummary {
  id: string;
  title?: string;
  created_at: string;
  updated_at: string;
  message_count?: number;
}

export interface SessionMessage {
  id: string;
  session_id: string;
  role: "user" | "assistant";
  content: string;
  created_at: string;
}

// ── HuggingFace / Model Download ──────────────────────────────
export interface HfModel {
  id: string;
  downloads: number;
  likes: number;
  tags: string[];
  url: string;
}

export interface HfModelFile {
  filename: string;
  size_mb?: number;
  url: string;
}

export interface DownloadEntry {
  filename: string;
  category: string;
  downloaded_bytes: number;
  total_bytes: number | null;
  status: "downloading" | "done" | "error";
  error?: string;
}

// ── Ollama ────────────────────────────────────────────────────
export interface OllamaModel {
  name: string;
  size?: number;           // bytes
  modified_at?: string;
  details?: {
    family?: string;
    parameter_size?: string;
    quantization_level?: string;
  };
}

// ── Llamafile GitHub Releases ─────────────────────────────────
export interface LlamafileRelease {
  name: string;            // filename e.g. "gemma-2b-it.llamafile"
  size_mb?: number;
  download_url: string;
  tag: string;             // GitHub release tag e.g. "0.9.1"
}

// ── Extensions / Tools ───────────────────────────────────────
export interface Extension {
  name: string;
  kind: string;
  description: string;
  tools: string[];
  enabled: boolean;
}

export interface AddExtensionRequest {
  name: string;
  kind: string;
  command?: string;
  uri?: string;
  description?: string;
  args?: string[];
  env?: Record<string, string>;
}

// ── Logs / Telemetry ─────────────────────────────────────────
export interface LogEntry {
  id: number;
  timestamp: string;
  level: string;      // INFO | WARN | ERROR
  source: string;
  message: string;
  metadata?: string;  // JSON string
}

// ── Face Recognition models (auto-managed status) ──────────────
//
// Mirrors the JSON returned by GET /api/v1/faces/models. The desktop
// Models section renders this read-only — pond-server downloads the
// three files automatically on first boot when built with `face-onnx`.
export interface FaceModelEntry {
  name: string;        // file basename, e.g. "w600k_r50.onnx"
  label: string;       // human-readable, e.g. "ArcFace R50"
  role: "embedding" | "detector" | "antispoof";
  expected_mb: number;
  size_mb: number | null;   // null when missing on disk
  downloaded: boolean;
  path: string | null;
}

export interface FaceModelsResponse {
  feature_enabled: boolean;  // false when pond-server lacks --features face-onnx
  models_dir: string | null;
  models: FaceModelEntry[];
}

// ── API Error ─────────────────────────────────────────────────
export class ApiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}
