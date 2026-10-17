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
  /** The household's chosen service. With Apple Music chosen, Spotify is not asked at all. */
  service?: "apple" | "spotify";
  /** The chosen player: the player page, or the service's own app. */
  player?: "page" | "app";
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
  /** The item's page on Spotify, which the design guidelines ask to link back to. */
  link?: string | null;
  /** What Spotify allows right now (its `actions.disallows`, inverted). */
  can?: { pause: boolean; resume: boolean; next: boolean; previous: boolean };
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
  /** Suggestor ids never offered on Home; per kind, as a suggestion has no durable id. */
  suggestions_muted?: string[];
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
  voice_kws_energy_threshold?: number;
  voice_kws_post_trigger_silence_ms?: number;
  voice_kws_cooldown_ms?: number;

  // Context
  context_window_override?: number;

  // Agent behaviour
  agent_backend?: string;
  // Commented out while llama.cpp lacks speculative decoding; restore with the catalogue entry.
  // /** Guess ahead with a helper model (speculative decoding). Only Gemma 4 E2B
  //  *  and E4B have one; defaults true. */
  // speculative_decoding_enabled?: boolean;
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
  /** Turn what the pond remembers into questions on Home. */
  suggestion_generation_enabled?: boolean;
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
  /** Directions and ride-app links (adds two tool schemas). */
  ext_travel_enabled?: boolean;

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
  /** Let a phone on the home network ask the pond to read a pairing code aloud. Ships ON. */
  spoken_pairing_codes?: boolean;
  /** Advertise over Bluetooth LE so a phone can set the pond up without a network. Ships ON. */
  ble_setup_enabled?: boolean;

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

/** What asking for a re-titling pass answered. */
export interface RetitleResult {
  /** True when the titling job was asked to run its next pass now. */
  started: boolean;
  /** Why not, when false; a pond with no titling loop to wake is not a failure. */
  reason?: string;
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
  // Written only by batch extraction, the one path that sees more than a single turn.
  | "routine"
  | "knowledge"
  | "context";

/** One inference-spending background job in `GET /api/v1/lane`, as the lane sees it now. */
export interface LaneJobStatus {
  /** Stable wire name, and the path segment `runLaneJob` takes. */
  job: string;
  title: string;
  /** A loop for this job exists in this process. */
  present: boolean;
  /** It has asked the lane for the slot at least once since this pond started. */
  registered: boolean;
  enabled: boolean;
  /** Seconds since it last ran in this process; null means never. */
  since_last_run_secs: number | null;
  interval_floor_secs: number;
  idle_threshold_secs: number;
  /** Why it would not run right now, or null if it would. */
  blocked_by: string | null;
  /** Counters since startup. Only `lost_to_total` tells losing the tie-break from about to run. */
  granted?: number;
  /** Times the lane rang this job's bell because it should have been running. */
  nudged?: number;
  slot_busy?: number;
  refused_disabled?: number;
  refused_no_activity?: number;
  refused_still_active?: number;
  refused_interval_floor?: number;
  lost_to_total?: number;
  lost_to_most?: { job: string; times: number } | null;
  /** It is the one that would take the slot on the next tick. */
  would_run_next: boolean;
}

export interface LaneStatus {
  /** False when this process has no lane at all (CLI paths); not the same as an empty job list. */
  lane: boolean;
  jobs: LaneJobStatus[];
  would_run?: string | null;
  idle_reason?: string | null;
  idle_for_secs?: number;
  saw_activity_since_start?: boolean;
  slot_busy?: boolean;
  /** The job holding the inference slot right now, and for how long. */
  running?: string | null;
  running_title?: string | null;
  running_for_secs?: number | null;
}

/** What asking for a job to run now did. */
export interface LaneRunResult {
  lane: boolean;
  job: string;
  /** The job's loop was asked to take its next tick at once. */
  woken: boolean;
  /** Why not, when not. */
  reason?: string;
}

/** Memory extraction. `running` false: no engine here (CLI, no embedder), not "nothing left". */
export interface ExtractionStatus {
  sessions_total: number;
  sessions_pending: number;
  mode: string | null;
  last_pass_at: string | null;
  last_pass_windows: number | null;
  last_pass_written: number | null;
  /** Notes the last pass refused for carrying a date; the date survives only as a reminder row. */
  last_pass_dated: number | null;
  /**
   * Refused notes no stored reminder matched, i.e. dates lost; over-counts by design. With
   * `last_pass_reminders_lost` at 0 the model filed no reminder; above 0 the store refused it.
   */
  last_pass_dates_lost: number | null;
  /** Reminder rows the last pass wrote; a row a re-walk recognised is neither written nor lost. */
  last_pass_reminders_written: number | null;
  /** Reminder candidates that reached no store at all. */
  last_pass_reminders_lost: number | null;
  /** Store-wide count of sessions with no known owner, so never remembered (mostly voice). */
  unattributed_sessions: number | null;
  blocked_on: string | null;
  running: boolean;
}

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

/** What runs a conversation model; absent on speech, voice and embedding rows. */
export interface ModelEngine {
  /** `llama_cpp`, `litert_lm`, `ollama` or `llamafile`; any other id is grouped as "other". */
  id: string;
  label: string;
  /** The extension it loads, e.g. `.gguf`; null for Ollama, which keeps its own store. */
  file_format: string | null;
  /** Its weights count against the pond's own memory budget. */
  in_process: boolean;
}

export type ModelProvenance = "catalogue" | "added" | "on_disk" | "ollama";

/** A helper (tool-call model, speech, embeddings) is never offered as conversation. */
export type ModelKind = "conversation" | "helper";

/** A download the pond runs, another program's store, or no source at all. */
export type ModelAcquire = "download" | "external" | "unavailable";

/** A separate download that extends a model. Picture support is the only kind. */
export interface ModelCompanion {
  kind: "pictures";
  label: string;
  size_bytes: number;
  /** `verifying`: the file is on disk and its hash is not yet checked. */
  state: "installed" | "available" | "downloading" | "verifying" | "not_on_this_device";
}

/** What a pick did on this class of machine; the server sends it only where it was measured. */
export interface ModelMeasured {
  device: "orin" | "desktop";
  summary: string;
  first_reply_s?: number;
  tokens_per_second_min?: number;
  tokens_per_second_max?: number;
  window_tokens?: number;
  /** `YYYY-MM-DD`. */
  measured_on: string;
}

/** Why GIAP suggests a model: shown, never imposed. */
export interface ModelRecommendation {
  rank: "primary" | "lighter" | "alternative";
  reason: string;
  measured?: ModelMeasured;
}

/** Why a pond cannot run a model's engine. */
export type ModelUnavailableReason = "litert_library_missing";

export interface ModelEntry {
  /** `"{category}/{name}"`, the key of the row and of its downloads (`DownloadEntry.model_id`). */
  id: string;
  provider: string;
  name: string;
  display_name?: string;
  /** What the household reads as the name; never a placeholder. */
  title?: string;
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
  engine?: ModelEngine;
  provenance?: ModelProvenance;
  kind?: ModelKind;
  acquire?: ModelAcquire;
  recommended?: ModelRecommendation;
  /** Separate downloads that extend the model; none means text only. */
  companions?: ModelCompanion[];
  /** False when this pond cannot run the model's engine; the pond refuses to fetch or use it. */
  available?: boolean;
  /** Why `available` is false. */
  unavailable_reason?: ModelUnavailableReason;
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

/** Mirrors `vision_encoder.rs::EncoderState`; closed so an unknown `kind` fails the shape check. */
export type EncoderState =
  | { kind: "unknown" | "not_declared" | "not_on_this_device" | "absent" | "verifying" }
  | { kind: "downloading"; done: number; total: number }
  | { kind: "ready"; bytes: number | null }
  | { kind: "failed"; reason: string; retry_at_unix_ms: number }
  | { kind: "blocked"; mode: string; host: string };

/** GET /api/v1/models/vision-status — picture support for the active chat model. */
export interface VisionStatus {
  model: string;
  state: EncoderState;
  size_bytes: number | null;
  /** Server-formatted copy for this state (sizes in MB); null for ready/unknown/not_declared. */
  message: string | null;
}

export interface ModelMemoryStatus {
  total_mb: number;
  available_for_llm_mb: number;
  loaded_model: string | null;
  /** What switching away from the model in use frees; absent from an older server. */
  reclaimable_mb?: number;
  /** The most this pond lets the models take: a desktop's memory less what it keeps for itself, or
   *  a budgeted device's own figure. `available_for_llm_mb` is what is left of it now. */
  budget_mb?: number;
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
  /** The wake phrase as heard in this take; `null` when the take did not contain it. */
  heard:         string | null;
  /** Whether `heard` was new and became a trigger. */
  new_variant:   boolean;
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

/** A phone asking to replace its remote identity, awaiting approval on this Pond. */
export interface RecoveryRequest {
  id: string;
  device: string;
  /** The name the household gave the phone; null when it could not be read. */
  deviceName: string | null;
  /** The start of the replacement's machine key, which the phone also shows. */
  keyPreview: string;
  approved: boolean;
  /**
   * Set once the household approved this recovery and the coordinator still refused it:
   * the coordinator's reason, e.g. `retired_capacity`, each with its own `remote.*` message.
   */
  refused?: string;
}

/** Response from the loopback-only GET /api/v1/handshake/pairing-code. */
export interface PairingCodeResponse {
  code: string | null;
  expires_at?: string;
  /** Where a phone connects and which key it pins; sent only on this host-only route. */
  pairing: PairingMaterial;
}

export interface PairingMaterial {
  hostname: string;
  /** LAN IPv4 for phones that cannot resolve `<hostname>.local`; null without a LAN route. */
  lan_address: string | null;
  /** Tailscale IPv4, reachable from outside the house. Null unless this Pond is on a tailnet. */
  tailnet_address: string | null;
  https_port: number | null;
  tls_spki_sha256: string | null;
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
  /** The picture add-on a download of this file brings. Null where the pairing is not known, and on
   *  a device that carries no add-ons. */
  pictures?: { size_bytes: number; label: string } | null;
}

/** Which file of a model a download is. */
export type DownloadPart = "model" | "pictures";

export interface DownloadEntry {
  /** The tracker key, which pause, resume and stop name. */
  filename: string;
  category: string;
  downloaded_bytes: number;
  total_bytes: number | null;
  /** `paused` keeps the partial file where `resumable`; `cancelled` deletes it. */
  status: "downloading" | "paused" | "done" | "error" | "cancelled";
  /** The row this file belongs to, `"{category}/{name}"`. */
  model_id?: string;
  part?: DownloadPart;
  /** Why it stopped, on an `error` entry: a sentence for the household, shown as given. */
  error?: string;
  /** Whether a pause keeps the partial file to resume from: true for a Hugging Face transfer, false for
   *  any other host, whose pause discards it and whose resume starts again from the beginning. */
  resumable?: boolean;
}

/** The server's answer to pausing, resuming or stopping a model's download: each part it moved. */
export interface DownloadControlResult {
  status: string;
  files?: { filename: string; status?: string; error?: string }[];
}

/** The server's answer to a download request: what it will fetch, before anything starts. */
export interface DownloadStarted {
  /** `download_started`, or `already_downloading` when every file asked for was already coming down. */
  status: string;
  model_id?: string;
  parts?: { part: DownloadPart; filename: string; size_bytes: number | null }[];
  /** `text_only`, `not_on_this_device`, `installed`, `included` or `left_out`. */
  pictures?: string;
  /** The household's sentence, e.g. "Downloading Gemma 4 E4B (4.2 GB) and picture support (945 MB)". */
  message?: string;
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

/** One answer a `choice` takes. */
export interface SecretOption {
  value: string;
  label: string;
  description?: string;
}

export interface SecretRequirement {
  key: string;
  display_name: string;
  description: string;
  required: boolean;
  /** `choice`: one of `options`; not a secret, so its value is read back and shown. */
  kind: 'api_key' | 'oauth_flow' | 'generic' | 'choice';
  /** For a `choice`: its answers, the first being what applies until one is saved. */
  options?: SecretOption[];
  /** Kept under "Developer settings": for bringing your own credentials or overriding a default. */
  advanced?: boolean;
  /** Used by the host only, never put in the extension's environment. */
  host_only?: boolean;
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

// ── Web search ────────────────────────────────────────────────
export type WebSearchStatus = "ok" | "not_configured" | "rejected" | "quota" | "unverified";

/** `GET /api/v1/web-search`. The key itself is never returned. */
export interface WebSearchState {
  provider: "brave";
  configured: boolean;
  status: WebSearchStatus;
}

/** `PUT /api/v1/web-search/key` when the key was stored. A key Brave refused is a 422 instead. */
export interface WebSearchKeyStored {
  configured: true;
  status: "ok" | "quota" | "unverified";
}

/** The 422 body of `PUT /api/v1/web-search/key`; nothing was stored. */
export interface WebSearchKeyRejected {
  error: string;
}

/** One result of a `search` card. */
export interface WebSearchHit {
  title: string;
  url: string;
  domain: string;
  snippet: string;
}

// ── API Error ─────────────────────────────────────────────────
export class ApiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
    /** Machine-readable `code` of a structured `{error, code}` body, e.g. "vision_not_ready". */
    public readonly code?: string,
    /** The full parsed error body, for a caller that needs more than `code`. */
    public readonly body?: unknown,
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
  /** Mirrors the Rust `TaskKind`, serde-tagged on "type". */
  proposed_action:
    | { type: "agent_prompt"; prompt: string }
    | { type: "webhook"; url: string }
    | { type: "sensor_trigger"; device_id: string; signal: string };
  trigger: {
    kind: string;
    source_id: string | null;
    signal: string | null;
    observed_at: string;
  };
}

export interface ProposalList {
  profile_id: string | null;
  proposals: Proposal[];
}

/**
 * A question the household might ask, from `GET /api/v1/suggestions`. Addressed to nobody (no
 * expiry, no `profile_id`): the tap is the consent, so its route needs no session or member.
 */
export interface Suggestion {
  /** Stable per KIND, so muting one mutes the same one tomorrow. */
  id: string;
  /** The sentence shown AND the prompt sent. One string, deliberately. */
  prompt: string;
  /** The measured fact behind it. Never blank. */
  because: string;
  /** The tool group that can answer `prompt`. */
  answered_by: string;

  /** Composed from one of your memories (a row, so settled on tap) rather than from a template. */
  composed: boolean;
}

/** Why one suggestor produced nothing, so quiet can be told from broken. */
export interface SuggestionConsidered {
  id: string;
  silent_because: string | null;
}

export interface SuggestionList {
  suggestions: Suggestion[];
  considered: SuggestionConsidered[];
  audience: "personal" | "shared";
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
