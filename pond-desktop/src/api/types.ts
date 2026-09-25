// Types mirroring the pond-server REST API shapes.

export interface HealthResponse {
  status: string;
  version?: string;
  uptime_seconds?: number;
}

// ── Weather ──────────────────────────────────────────────────
export interface WeatherForecastDayResponse {
  d: string;
  i: string;
  t: number;
}

export interface WeatherApiResponse {
  enabled: boolean;
  location_name?: string;
  temp?: number;
  cond?: string;
  icon?: string;
  hi?: number;
  lo?: number;
  hum?: number;
  wind?: number;
  sunrise?: string;
  sunset?: string;
  forecast?: WeatherForecastDayResponse[];
}

// ── Music ────────────────────────────────────────────────────
export interface NowPlayingApiResponse {
  connected: boolean;
  playing?: boolean;
  track?: string;
  artist?: string;
  album_art?: string | null;
  progress_ms?: number;
  duration_ms?: number;
  /** Why Spotify refused: "unauthorized" | "forbidden" | "rate_limited" | "unavailable". */
  error?: string;
  /** Spotify's HTTP status; absent on transport errors, so only real 4XXs stop polling. */
  upstream_status?: number;
  /** Human-readable explanation for `error`, safe to show as-is. */
  message?: string;
}

export type MusicControlAction = "play" | "pause" | "next" | "previous";

// ── OAuth ────────────────────────────────────────────────────
/** One OAuth flow's outcome, keyed by `state` nonce. `unknown` is not terminal (the server may
 *  have restarted mid-flow): keep waiting until the caller's own timeout. */
export interface OAuthFlowStatus {
  status: "pending" | "completed" | "failed" | "unknown";
  error?: string;
}

// ── Settings ─────────────────────────────────────────────────
export interface Settings {
  // Identity
  primary_profile_id?: string | null;
  assistant_name: string;
  user_name: string;
  assistant_personality?: string;
  timezone?: string;

  // Voice pipeline
  voice_wake_word?: string;
  voice_wake_word_transcriptions?: string[];
  voice_recording_duration_secs?: number;
  voice_whisper_url?: string;
  active_whisper_model?: string;
  active_tts_model?: string;
  voice_tts_voice?: string;
  /** Pace multiplier 0.5–2.0 (1.0 = as trained), fed as-is to the engine's `speed` tensor. */
  voice_tts_speed?: number;
  /** Kokoro quantization (`q8` | `q8f16` | `q4f16` | `fp16` | `fp32`); picks the `.onnx` file. */
  voice_tts_quality?: string;
  voice_thinking_tone_enabled?: boolean;

  // Model roles
  chat_provider?: string;
  chat_model?: string;
  tool_model?: string | null;
  thinking_mode?: string;
  reasoning_effort?: string;
  show_thinking?: boolean;
  /** Persist reasoning text after the stream; a separate consent from `show_thinking` (live). */
  persist_thinking?: boolean;
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


  // Active model selection
  active_embedding_model?: string;
  embedding_provider?: string;

  // Voice KWS tuning
  voice_kws_whisper_url?: string | null;
  voice_kws_energy_threshold?: number;
  voice_kws_post_trigger_silence_ms?: number;
  voice_kws_cooldown_ms?: number;

  // Context
  context_window_override?: number;

  // Agent behaviour
  agent_backend?: string;
  agent_goose_mode?: string;
  agent_max_turns?: number;
  agent_timeout_secs?: number;
  prefix_cache_prompt?: boolean;
  agent_memory_inject: boolean;
  agent_memory_limit?: number;
  tool_output_compaction?: boolean;
  /** "all" (default) | "relevant" — which extension tool schemas reach the model. */
  tool_selection_mode?: string;

  // Memory lifecycle
  memory_extraction_enabled?: boolean;
  memory_cleanup_enabled?: boolean;
  memory_consolidation_enabled?: boolean;
  /** Let the pond rename conversations while idle. Never touches a name you typed. */
  session_titling_enabled?: boolean;
  memory_graph_enabled?: boolean;

  // Memory tuning
  memory_prune_threshold?: number;
  memory_archive_threshold?: number;
  memory_decay_base_half_life_days?: number;
  memory_decay_beta?: number;
  memory_cleanup_interval_hours?: number;
  memory_consolidation_interval_hours?: number;
  memory_consolidation_batch_size?: number;
  memory_consolidation_mode?: string;
  memory_extraction_max_facts?: number;
  memory_extraction_interval_secs?: number;

  // Scheduling tuning
  schedule_result_notify?: boolean;
  schedule_max_concurrent?: number;
  schedule_max_runs_per_task?: number;

  // Context monitoring
  context_monitor_enabled?: boolean;

  // Cost comparison
  cloud_input_price_per_million?: number;
  cloud_output_price_per_million?: number;


  // Telemetry
  telemetry_enabled?: boolean;


  // Experimental
  multi_tool_enabled?: boolean;
  tool_call_validation?: boolean;
  tool_request_detection?: boolean;

  // Extension toggles
  ext_memory_enabled?: boolean;
  ext_schedule_enabled?: boolean;
  ext_weather_enabled?: boolean;
  ext_knowledge_enabled?: boolean;
  ext_system_enabled?: boolean;
  ext_device_enabled?: boolean;
  ext_sensor_enabled?: boolean;
  /** Delegation to saved agent roles. Ships OFF: read as `=== true` so absent means off. */
  ext_orchestrator_enabled?: boolean;

  // Speaking and acting unprompted. Both ship OFF: read as `=== true` so absent means off.
  /** Unasked review and proposals; needs `ext_orchestrator_enabled` (runs as a delegated child). */
  proactive_review_enabled?: boolean;
  /** May the pond speak without having been spoken to? */
  unprompted_speech_enabled?: boolean;
  /** Quiet-hours start, local `"HH:MM"`; overrides every other speech setting. Wraps midnight
   *  when start > end; equal bounds or an unparseable value mean always silent. */
  quiet_hours_start?: string;
  /** End of the quiet-hours window, local `"HH:MM"`. See `quiet_hours_start`. */
  quiet_hours_end?: string;
  /** Comma-separated categories spoken unprompted (default "alert"); unknown ones match nothing. */
  unprompted_speech_categories?: string;

  // API keys are not here: see listSecretKeys / setSecret / deleteSecret (values are write-only).
  searxng_url?: string | null;

  // Data retention
  retention_event_log_days?: number;
  retention_sensor_days?: number;
  retention_session_messages_keep?: number;

  // Privacy / sensor access
  mic_enabled?: boolean;
  cameras_enabled?: boolean;
  cloud_fallback_enabled?: boolean;

  // Identity — home name
  home_name?: string;

  // Vision / cameras (on-device event detection)
  vision_enabled?: boolean;
  vision_camera_url?: string;
  vision_camera_id?: string;
  vision_fps?: number;
  vision_motion_threshold?: number;

  // Matter (smart-home fabric). No on/off toggle; a stored `matter_enabled` is ignored.
  matter_ws_url?: string;
  /** Whether the controller pairs over Bluetooth as well as over the network. */
  matter_ble_enabled?: boolean;

  // Inference stats display
  show_turn_stats?: boolean;

  // ── Network, security and other server settings ───────────────────────────
  // `catalogue.test.ts` fails if this type and the settings catalogue disagree.

  /** How hard outbound HTTP is gated. Server rejects anything else with 422. */
  network_mode?: "open" | "allowlist" | "offline";
  /** How hard the security policy bites. */
  security_policy_mode?: "off" | "audit" | "enforce";
  /** Ask the model whether the request was actually met before ending a turn. */
  goal_check_enabled?: boolean;
  /** Start the private mesh transport (needs a `mesh`-feature server build). */
  mesh_enabled?: boolean;
  /** Turn what the pond's own sensors report into per-member context items. */
  context_ingest_enabled?: boolean;
  /** Let the model read the personal-context corpus (adds two tool schemas). */
  ext_context_enabled?: boolean;

  // Compaction — GIAP-owned history pruning
  hybrid_compaction_enabled?: boolean;
  summary_idle_secs?: number;
  compaction_verbatim_days?: number;

  // Retention — the unified events log
  retention_events_days?: number;
  retention_events_by_category?: Record<string, number>;
  retention_sensitive_days?: number;

  /** Turn cap for VOICE requests; never raised above `agent_max_turns`. */
  voice_max_turns?: number;
  /** ONNX detector that labels motion events. Needs a `vision-onnx` build. */
  vision_classifier_model?: string;
}

/** Matter runtime: `enabled` = intent, `state` = reality (differ while starting or unreachable). */
export interface MatterStatus {
  enabled: boolean;
  url: string;
  state: "disabled" | "connecting" | "connected" | "unreachable";
  /** Present only when `state` is "unreachable". */
  error?: string;
}

// ── Consolidation ────────────────────────────────────────────
export type ConsolidationEventType =
  | "started"
  | "proposer_done"
  | "adversary_done"
  | "judge_done"
  | "applied"
  | "completed"
  | "error"
  | "cancelled";

/** What renaming one named conversation did. */
export interface RetitleOneResult {
  session_id: string;
  outcome: "retitled" | "skipped" | "unusable" | "cancelled";
  /** The new name, or null when nothing was written. */
  title: string | null;
  /** Present on "skipped" — why, in a stable slug. */
  reason?: string;
}

/** What one manual re-titling pass did. */
export interface RetitleResult {
  renamed: { session_id: string; title: string }[];
  renamed_count: number;
  /** Conversations the pass looked at, excluding the pond's own background ones. */
  considered: number;
  /** True when the pass hit its own bound — pressing again picks up from there. */
  capped: boolean;
  /** The model answered with something unusable; the old name was kept. */
  unusable: number;
  failed: number;
  skipped: {
    /** Named by hand. Never overwritten. */
    user_named: number;
    /** Already has a model-written name that still fits. */
    still_current: number;
    too_short: number;
    /** Predates the provenance column and is not the six-word fallback. */
    unknown_provenance: number;
  };
}

export interface ConsolidationEvent {
  type: ConsolidationEventType;
  memory_count?: number;
  proposals?: unknown[];
  challenges?: unknown[];
  decisions?: unknown[];
  exchange?: unknown;
  result?: {
    exchanges: unknown[];
    accepted_count: number;
    rejected_count: number;
    duration_ms: number;
  };
  message?: string;
}

// ── Devices ───────────────────────────────────────────────────
export interface Device {
  id: string;
  name: string;
  device_type?: string;
  hostname?: string;
  room?: string;
  is_online: boolean;
  last_seen?: string;
  /** `set_device_state` verbs it accepts (`power`, `brightness`, …); a contact sensor has none. */
  capabilities?: string[];
  /** The address the server knows, when it knows one. */
  ip_address?: string;
  metadata?: Record<string, unknown>;
}

// ── Mesh ──────────────────────────────────────────────────────
// Mirrors 'pond_core::mesh::domain' + the /api/v1/mesh/* routes.
export interface MeshPeer {
  peer_id: string;
  trust_scope: "self_owned" | "circle";
  connected: boolean;
  credit_balance_millisats: number;
}

export interface MeshSelf {
  mesh_enabled: boolean;
  peer_id?: string;
  invite_url?: string;
}

/** Live, on-demand — not part of MeshPeer since it's queried over the mesh, not cached. */
export interface MeshPeerCapabilities {
  peer_id: string;
  inference_available: boolean;
  lightning_available: boolean;
}

/** Periodic settlement job status. Read-only: `millisats_per_token` is settings-API only. */
export interface MeshSettlementStatus {
  configured: boolean;
  millisats_per_token: number;
  peers: Array<{
    peer_id: string;
    pending_tokens: number;
    pending_millisats: number;
  }>;
}

// ── Schedules ─────────────────────────────────────────────────
export interface Schedule {
  id: string;
  name: string;
  label?: string;
  cron: string;
  /** Set when this schedule is a one-shot: fires once at this instant, then never again. */
  fire_at?: string | null;
  prompt: string;
  enabled: boolean;
  timezone?: string;
  kind?: { type: "agent_prompt"; prompt: string } | { type: "webhook"; webhook_url: string };
  last_run?: string;
  next_run?: string;
  created_at?: string;
}

export interface ScheduleRun {
  id: string;
  schedule_id: string;
  status: "running" | "completed" | "failed";
  result?: string;
  error?: string;
  started_at: string;
  finished_at?: string;
  duration_ms?: number;
}

/** Enriched schedule run for UI notification display. */
export interface ScheduleRunNotification {
  id: string;
  scheduleId: string;
  scheduleName: string;
  status: "running" | "completed" | "failed";
  result: string | null;
  error: string | null;
  startedAt: string;
  finishedAt: string | null;
  durationMs: number | null;
  read: boolean;
  /** First ~80 chars of result for preview */
  excerpt: string;
  /** Inferred recipe type from schedule name/prompt */
  recipe: string | null;
}

/** Context passed when navigating to Canvas to view a schedule debrief. */
export interface DebriefContext {
  type: "debrief";
  run: ScheduleRunNotification;
}

// ── Memory ────────────────────────────────────────────────────
export type MemorySegment =
  | "identity"
  | "preference"
  | "correction"
  | "relationship"
  | "project"
  | "knowledge"
  | "context";

export type MemoryTier = "short" | "long" | "permanent";
export type MemoryLifecycle = "active" | "archived" | "merged";

export interface MemoryFragment {
  id: string;
  content: string;
  source?: string;
  tags?: string[];
  created_at: string;
  segment?: MemorySegment;
  importance?: number;
  tier?: MemoryTier;
  lifecycle?: MemoryLifecycle;
  access_count: number;
  last_accessed_at?: string;
  superseded_by?: string;
}

// ── Semantic index coverage ───────────────────────────────────
// Mirrors `context_index_health` / `rebuild_context_index` (pond-api routes.rs). No index or
// embedder answers 200 with `indexed: false` and the counts absent, not zero.

/** The three stores the index covers, spelled as the server spells them. */
export type ContextCorpus = "memory" | "context" | "summary";

/** One store's share of the index. `coverage` is null when `rows` is 0 (0/0): render it as
 *  "nothing to index", never as a percentage. */
export interface ContextCorpusCoverage {
  corpus: ContextCorpus;
  /** Live rows in the source store that qualify for indexing. */
  rows: number;
  /** Unfiltered source-table row count; tells "empty" from "all excluded", never a denominator. */
  source_rows: number;
  /** `rows === 0` on a non-empty table: a filter excludes everything; re-embedding won't fix it. */
  structurally_excluded: boolean;
  /** Of those, the ones carrying a vector from the model currently configured. */
  indexed_rows: number;
  missing_rows: number;
  /** Rows with a vector from another embedder: they score plausibly but wrongly until rebuilt. */
  mismatched: number;
  coverage: number | null;
}

/** How much of what the pond knows retrieval can currently reach. */
export interface ContextIndexHealth {
  indexed: boolean;
  /** Why there is nothing to report. Sent only when `indexed` is false. */
  reason?: string;
  model_id: string | null;
  dims: number | null;
  /** The three totals, summed across corpora. Absent when `indexed` is false. */
  rows?: number;
  matching?: number;
  mismatched?: number;
  missing?: number;
  coverage: number | null;
  corpora: ContextCorpusCoverage[];
}

export interface ContextCorpusCleared {
  corpus: ContextCorpus;
  cleared: number;
}

/** What one sync pass did, as counts rather than a success flag. */
export interface AccountSyncSummary {
  sources: number;
  unchanged: number;
  ingested: number;
  needs_reauth: number;
  failed: number;
  paused: number;
  /** Per-account breakdown of the totals. */
  per_source?: SourceSyncOutcome[];
}

/** One account's result from a sync pass. */
export interface SourceSyncOutcome {
  source_id: string;
  provider: string;
  kind: string;
  /** `ingested` | `unchanged` | `needs_reauth` | `failed` | `paused` */
  outcome: string;
  ingested: number;
}

/** One thing the pond read from a connected source. */
export interface ContextItem {
  id: string;
  source_id: string;
  /** `calendar`, `mail`, `camera`, `sensor`. */
  source_kind: string;
  /** `event`, `message`, `document`, `location`, `task`. */
  kind: string;
  title: string;
  body: string;
  occurred_at: string;
  participants: string[];
  /** Whether retrieval can currently reach it. */
  searchable: boolean;
}

/** A connected personal-context source, as the sources API reports it. */
export interface ContextSource {
  id: string;
  /** `calendar`, `mail`, `camera`, `sensor`. */
  kind: string;
  /** `google`, `icloud`, `fastmail`, `nextcloud`, `custom`, or a device id. */
  provider: string;
  profile_id: string;
  /** `connected` | `needs_reauth` | `error` | `paused` */
  status: string;
  /** RFC3339, or null when the pond has not reached this account yet. */
  last_sync: string | null;
  /** Whether this kind signs in to an account, as opposed to being on-pond. */
  needs_credentials: boolean;
  /** Everything stored from this source. */
  items: number;
  /** Of those, the ones still waiting to become searchable by meaning. */
  awaiting_index: number;
}

/** Result of emptying the index; lists every corpus, even those at zero. */
export interface ContextIndexRebuild {
  indexed: boolean;
  reason?: string;
  /** Vectors dropped per the DELETE; includes rows under corpus names no longer in `corpora`. */
  cleared: number;
  corpora: ContextCorpusCleared[];
}

// ── User Skills ───────────────────────────────────────────────
export interface UserSkill {
  id: string;
  name: string;
  description: string;
  /** Icon key from SKILL_ICONS (Skills.tsx) — cosmetic only. */
  icon: string;
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
  /** Catalog-declared max context window (LLMs only); prefer it to guessing from `name`. */
  context_length?: number;
  /** Quantisation scheme, e.g. "Q4_K_M" — read from the model file's own header. */
  quantization?: string;
  downloaded?: boolean;
  description?: string;
  size_mb?: number;
  category?: string;
  filename?: string;
  url?: string;
  asr_language?: string;
  asr_size?: string;
  tts_engine?: string;
  config_filename?: string;
}

/** GET /api/v1/warmup — the boot/model-change prefix warm-up (see Agent::prewarm). */
export interface WarmupStatus {
  state: "warming" | "ready" | "skipped" | "failed";
  /** Present on skipped/failed. */
  reason?: string;
  /** Chat model the warm-up ran against ("" before the first run). */
  model: string;
  started_unix_ms: number;
  finished_unix_ms: number | null;
  elapsed_ms: number;
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
  tool_calling: boolean;
}

// ── Prompt Templates ──────────────────────────────────────────
export interface PromptTemplate {
  name: string;
  content: string;
  description?: string;
  is_system: boolean;
  /** User-edited: the startup factory reseed leaves this template alone. */
  is_customized?: boolean;
  /** Built-in template generation; an edited row keeps the one it was forked from. */
  factory_version?: number;
  updated_at?: string;
}

/** Mirrors `FACTORY_VERSION` in pond-core `user_data/domain/prompt_template.rs`; bump both. */
export const PROMPT_FACTORY_VERSION = 1;

/** True when the user's edit predates the current built-in; notice only, never overwrite it. */
export function promptTemplateIsOutdated(t: PromptTemplate): boolean {
  return (
    t.is_system === true &&
    t.is_customized === true &&
    (t.factory_version ?? 0) < PROMPT_FACTORY_VERSION
  );
}

// ── Agent ─────────────────────────────────────────────────────
export interface AgentTool {
  extension: string;
  name: string;
  description?: string;
}

export interface RecipeParameter {
  key: string;
  input_type?: "string" | "number" | "boolean" | "date" | "file" | "select";
  requirement?: "required" | "optional" | "user_prompt";
  description?: string;
  default?: string;
  options?: string[];
}

export interface RecipeExtensionSpec {
  type?: string;
  name: string;
  timeout?: number;
  bundled?: boolean;
}

export interface AgentRecipe {
  id?: string;
  name: string;
  description?: string;
  yaml: string;
  active?: boolean;
  created_at?: string;
  /** Parsed out of `yaml` server-side; present on list/create/update responses. */
  title?: string;
  parameters?: RecipeParameter[];
  extensions?: RecipeExtensionSpec[];
  activities?: string[];
}

// ── Chat / Streaming ──────────────────────────────────────────

/** A single image sent alongside a chat turn. `data` is raw base64 — NO `data:...;base64,` prefix. */
export interface ImageAttachment {
  data: string;
  mime_type: string;
}

/** Request body for POST /api/v1/chat/stream */
export interface ChatStreamRequest {
  message: string;
  session_id?: string;
  canvas_mode?: boolean;
  voice_mode?: boolean;
  images?: ImageAttachment[];
  /** Ask the server to keep this turn running if the connection drops. */
  resumable?: boolean;
}

/** What the server is still driving for a session, from `GET .../active-run`. */
export interface ActiveRun {
  run_id: string;
  session_id: string;
  state: "running" | "finished" | "failed" | "cancelled";
  started_at: string;
  /** Oldest frame still replayable. Anything before it is genuinely lost. */
  first_seq: number;
  last_seq: number;
  /** Identifies the server process. A different one means the run is gone. */
  epoch: string;
}

export type ChatEventType = "text" | "thinking" | "tool_call" | "tool_result" | "done" | "error" | "status" | "review_status" | "review_revision" | "tool_revision" | "turn_stats" | "turn_limit_reached" | "context_warning" | "subagent_progress" | "run_started" | "reattached" | "replay_gap" | "run_evicted" | "cancelled";

/** One delegation's progress; spellings are pond-core's `SubagentStatus::as_str`. */
export type SubagentStatus =
  | "queued"
  | "running"
  | "tool"
  | "completed"
  | "cancelled"
  | "turn_budget_exhausted"
  | "failed";

/** Sent as the next user turn to resume after the turn budget ran out (no resume endpoint). */
export const CONTINUE_TURN_MESSAGE = "Continue where you left off.";

/** Per-turn inference stats; timing fields are null when the provider doesn't report them. */
export interface TurnStats {
  type: "turn_stats";
  ttft_ms: number | null;
  prefill_ms: number | null;
  decode_tok_per_sec: number | null;
  prefill_tok_per_sec: number | null;
  // `prefill_tok_per_sec` is over `prefilled_tokens`, not `prompt_tokens` (KV-cache reuse).
  // `reused_prefix_tokens === 0` on turn 2+ means the prefix is no longer token-stable.
  prefilled_tokens: number | null;
  reused_prefix_tokens: number | null;
  prompt_tokens: number;
  completion_tokens: number;
  context_used_tokens: number | null;
  context_limit_tokens: number | null;
  context_pct: number | null;
  model_load_ms: number | null;
  inference_count: number;
}

/** Mid-stream frame when `should_compact` is true. Unlike `CompactionReport`, `turns_remaining`
 *  may be `TURNS_REMAINING_UNKNOWN` (clamp before printing) and `warning` may be null. */
export interface ContextWarning {
  type: "context_warning";
  utilization_pct: number;
  turns_remaining: number;
  avg_growth_rate: number;
  warning: string | null;
}

/** Sentinel the monitor uses for "growth rate unknown, so turns remaining is unknown". */
export const TURNS_REMAINING_UNKNOWN = 4294967295;

/** Response body of POST /api/v1/sessions/{session_id}/compact. */
export interface CompactionReport {
  session_id: string;
  /** "compacted" only when a pass actually persisted a new summary. */
  status: "compacted" | "skipped";
  /** Why it was skipped: monitor_disabled, compaction_disabled, not_under_pressure, no_summariser,
   *  already_running, cooling_down, nothing_to_summarise, preempted_by_turn, failed. */
  reason: string | null;
  outcome: string | null;
  context: {
    utilization_pct: number;
    /** `null` here where the SSE frame sends 4294967295. */
    turns_remaining: number | null;
    avg_growth_rate: number;
    should_compact: boolean;
    warning: string | null;
  };
}

export interface ChatEvent {
  type: ChatEventType;
  content?: string;         // for "text" events
  token?: string;           // legacy backend alias for content
  tool?: string;            // for "tool_call" and "tool_result" events
  id?: string;              // tool call ID — for matching tool_call to tool_result
  input?: unknown;          // for "tool_call" events — model's tool call arguments
  result?: unknown;         // for "tool_call" events
  error?: string;           // for "error" events
  /** Optional MCP-APP UI rendering hint from backend */
  ui?: {
    card_type?: string;
    data?: Record<string, unknown>;
  };
  /** Turn budget that was exhausted — present on "turn_limit_reached" events. */
  max_turns?: number;
  /** On "subagent_progress": the run; group tree nodes by it, not `role` (a role can repeat). */
  task_id?: string;
  /** On "subagent_progress": the delegated role, shown as the tree node's label. */
  role?: string;
  /** Present on "subagent_progress" events. */
  status?: SubagentStatus;
  /** A child's current tool name, or the pond's failure reason. Never the child's words or args. */
  detail?: string;
  done?: boolean;
  session_id?: string;      // present on done events
  model_role?: string;      // present on done events (chat | think | task)
  model_name?: string;      // present on done events — name of the model that responded
  usage?: {                 // token usage — present on done events when provider reports it
    prompt_tokens: number;
    completion_tokens: number;
  };
  /** Frame sequence from the SSE `id:` field (not the JSON body); a reattach resumes from it. */
  seq?: number;
  /** The turn this belongs to; on run_started, reattached, run_evicted, cancelled and done. */
  run_id?: string;
  /** On "run_started"/"reattached"; a new epoch means the remembered run died with the server. */
  epoch?: string;
  /** On "done": the turn was cancelled or timed out. */
  interrupted?: boolean;
  /** On "replay_gap": the oldest replayable frame; anything missed before it is lost. */
  first_available_seq?: number;
  /** On "replay_gap"/"run_evicted": what to do, which is always to reload the session. */
  advice?: string;
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
// Mirrors `pond_core::ports::handshake::HandshakeResponse`.
export interface HandshakeResponse {
  accepted: boolean;
  session_token: string | null;
  refresh_token?: string | null;
  /** RFC3339 expiry of the session token. */
  expires_at?: string | null;
  hostname: string;
  server_version: string;
  capabilities: string[];
  rejection_reason: string | null;
}

/** Response from POST /api/v1/handshake/init. */
export interface ChallengeResponse {
  challenge_id: string;
  /** base64-encoded challenge bytes. */
  challenge: string;
  expires_at: string;
}

/** Response from the loopback-only GET /api/v1/handshake/pairing-code. */
export interface PairingCodeResponse {
  code: string | null;
  expires_at?: string;
}

// ── Model Role Assignments ────────────────────────────────────
export interface ModelRoleAssignment {
  provider: string;
  model: string;
}

export interface ModelActiveRoles {
  chat:       ModelRoleAssignment | null;
  tool:       { model: string | null } | null;
  asr:        ModelRoleAssignment | null;
  tts:        ModelRoleAssignment | null;
  embedding:  ModelRoleAssignment | null;
}

// ── Sessions ──────────────────────────────────────────────────
export interface SessionSummary {
  id: string;
  title?: string;
  /** How the conversation opened, for a history card. Absent on older servers. */
  preview?: string;
  created_at: string;
  updated_at: string;
  message_count?: number;
  total_prompt_tokens?: number;
  total_completion_tokens?: number;
  model_name?: string;
}

export interface UsageSummary {
  total_prompt_tokens: number;
  total_completion_tokens: number;
  total_tokens: number;
  session_count: number;
  cloud_input_price_per_million?: number;
  cloud_output_price_per_million?: number;
}

export interface SessionMessageToolCall {
  id: string;
  name: string;
  arguments: string;
}

/** A persisted image on a session message; `url` is relative to the API base. */
export interface SessionMessageImage {
  id: string;
  mime_type: string;
  byte_size: number;
  url: string;
}

export interface SessionMessage {
  id: string;
  session_id: string;
  role: "user" | "assistant" | "tool";
  content: string;
  created_at: string;
  /** Present on role="assistant" messages that invoked tools. */
  tool_calls?: SessionMessageToolCall[];
  /** Present on role="tool" messages — links back to the tool_call id. */
  tool_call_id?: string;
  /** Present on messages (typically role="user") that had images attached. */
  images?: SessionMessageImage[];
  /** Reasoning passages in order, if `persist_thinking` was on; absent (not `[]`) if unrecorded. */
  thinking?: string[];
  /** Training-feedback vote: true keeps it as training data, false excludes it, null = no vote. */
  liked?: boolean | null;
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
  /** `paused` keeps the partial file for resuming; `cancelled` deletes it. */
  status: "downloading" | "paused" | "done" | "error" | "cancelled";
  error?: string;
}

// ── Disk cleanup / usage ─────────────────────────────────────
export interface RemovedBlob {
  path: string;
  category: string;
  bytes: number;
}

export interface CleanupResponse {
  reclaimed_bytes: number;
  removed: RemovedBlob[];
}

export interface DiskUsage {
  total_bytes: number;
  by_category: Record<string, number>;
  hf_cache_bytes: number;
  incomplete_bytes: number;
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
  /** Extension connection status: "connected", "error", or "loading" */
  status?: string;
  /** Last error message if status is "error" */
  last_error?: string | null;
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

export interface SecretRequirement {
  key: string;
  display_name: string;
  description: string;
  required: boolean;
  kind: 'api_key' | 'oauth_flow' | 'generic';
}

export interface MarketplaceExtension {
  id: string;
  name: string;
  description: string;
  kind: string;
  command?: string;
  args: string[];
  uri?: string;
  category: string;
  author: string;
  tools: string[];
  featured: boolean;
  required_secrets: SecretRequirement[];
}

// ── Activity / Logs ──────────────────────────────────────────
// Mirrors GET /api/v1/activity and GET /api/v1/activity/summary.

export type EventCategory =
  | "agent" | "tool" | "inference" | "sensor"
  | "camera" | "device" | "auth" | "network" | "system";

export type PrivacySensitivity = "public" | "internal" | "sensitive" | "secret";

export type AttributeValue =
  | { bool: boolean }
  | { int: number }
  | { float: number }
  | { text: string };

export interface ActivityEvent {
  // The backend sends no row id; consumers derive their own React key.
  id?: string;
  timestamp: string;
  category: EventCategory;
  action: string;
  privacy_sensitivity: PrivacySensitivity;
  session_id?: string | null;
  trace_id?: string | null;
  attributes: Record<string, AttributeValue>;
}

export interface ActivityResponse {
  count: number;
  events: ActivityEvent[];
}

export interface ActivitySummary {
  window: string;
  since: string;
  total: number;
  by_category: Record<string, number>;
}

export interface ActivityQueryParams {
  limit?: number;
  since?: string;
  category?: EventCategory;
  session_id?: string;
}

export interface LogEntry {
  id: number;
  timestamp: string;
  level: string;      // INFO | WARN | ERROR
  source: string;
  message: string;
  metadata?: string;  // JSON string
}

// ── Face Recognition models (auto-managed status) ──────────────
// GET /api/v1/faces/models; read-only, the server fetches the files on first boot (`face-onnx`).
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

/** A pending suggestion from the pond; mirrors `proposal_json` in pond-api routes.rs. */
export interface Proposal {
  id: string;
  summary: string;
  rationale: string;
  confidence: number;
  profile_id: string | null;
  created_at: string;
  expires_at: string;
  proposed_action: string;
  trigger: {
    kind: string;
    source_id: string;
    signal: string;
    observed_at: string;
  };
}

export interface ProposalList {
  profile_id: string | null;
  proposals: Proposal[];
}

/** Approve or reject. The server accepts no third value. */
export type ProposalDecision = "approve" | "reject";

// ── Time and place ──────────────────────────────────────────

/** A zone, the offset it is on today, and the place its name implies. */
export interface ZoneChoice {
  zone: string;
  /** e.g. "+03:00". Resolved for today — an offset is not fixed per zone. */
  offset: string;
  /** e.g. "Nairobi". Empty for zones like UTC that are not places. */
  place: string;
}

/** How the pond worked out where it is. */
export type PlaceSource = "timezone" | "geocoded" | "device" | "network";

/** The result of one detection pass. */
export interface DetectedPlace {
  name: string;
  latitude: number;
  longitude: number;
  timezone: string;
  source: PlaceSource;
  /** Whether this is a fact rather than a good guess. */
  certain: boolean;
  has_coordinates: boolean;
  /** Why there are no coordinates, when there are none. Shown as-is. */
  note: string | null;
}
