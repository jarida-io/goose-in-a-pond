// ────────────────────────────────────────────────────────────
// GIAP Desktop — API Type Definitions
// Mirror of pond-server REST API shapes
// ────────────────────────────────────────────────────────────

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
  /**
   * Set when Spotify answered but refused the request — `"unauthorized"`,
   * `"forbidden"`, `"rate_limited"` or `"unavailable"`. Absent on a healthy
   * response, including the genuine "connected but nothing playing" case.
   */
  error?: string;
  /**
   * The literal HTTP status Spotify answered with, when it answered at all.
   * Absent on a healthy response and on transport failures — which is the
   * point: only a real 4XX counts toward the stop rule below.
   */
  upstream_status?: number;
  /** Human-readable explanation for `error`, safe to show as-is. */
  message?: string;
}

export type MusicControlAction = "play" | "pause" | "next" | "previous";

// ── OAuth ────────────────────────────────────────────────────
/**
 * Outcome of a single OAuth flow, keyed server-side by its `state` nonce.
 *
 * `unknown` is not a failure on its own — a flow whose nonce the server never
 * issued (it restarted mid-flow) reports it too, so callers should keep waiting
 * until their own timeout rather than treating it as terminal.
 */
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
  /**
   * Speaking pace as a multiplier, 0.5–2.0. 1.0 is the voice as trained.
   * Stored as a multiplier rather than a percentage because that is exactly
   * what the engine's `speed` tensor takes — no conversion, nothing to get
   * backwards between the slider and the model.
   */
  voice_tts_speed?: number;
  /**
   * Quality tier — a Kokoro quantization (`q8` | `q8f16` | `q4f16` | `fp16` |
   * `fp32`). Picking a tier picks an `.onnx` file; there is nothing else to it.
   */
  voice_tts_quality?: string;
  voice_thinking_tone_enabled?: boolean;

  // Model roles
  chat_provider?: string;
  chat_model?: string;
  tool_model?: string | null;
  thinking_mode?: string;
  reasoning_effort?: string;
  show_thinking?: boolean;
  /** PAI-5 P6. Whether reasoning text survives the stream that produced it.
   *  Orthogonal to `show_thinking`, which only decides whether it is shown
   *  live — showing something once and keeping it are different consents. */
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
  ext_news_enabled?: boolean;
  ext_finance_enabled?: boolean;
  ext_discovery_enabled?: boolean;
  ext_audit_enabled?: boolean;
  ext_vision_enabled?: boolean;
  ext_sensor_enabled?: boolean;
  /**
   * Delegation to saved agent roles. The one extension toggle that ships OFF —
   * turning it on lets the assistant run a second agent autonomously on this
   * device. Read it as `=== true`, never `!== false`: absent must mean off.
   */
  ext_orchestrator_enabled?: boolean;

  // Speaking and acting unprompted (PAI-7 P4 and P6).
  //
  // Both booleans ship OFF and must be read as `=== true`, never `!== false`:
  // a key the server has not sent yet, or a settings read that failed, has to
  // mean "does not speak" and "does not review". These are the only settings in
  // this type that decide whether the assistant addresses somebody who did not
  // address it, so the widening direction is the one that matters.
  /**
   * May the pond reason about the household unasked, and propose things?
   * Needs `ext_orchestrator_enabled` as well — the reviewer runs its work as a
   * delegated child, so with delegation off there is nothing to run it in.
   */
  proactive_review_enabled?: boolean;
  /** May the pond speak without having been spoken to? */
  unprompted_speech_enabled?: boolean;
  /**
   * Start of the nightly window in which the pond never speaks unprompted,
   * local `"HH:MM"`. ABSOLUTE: the server checks this window before consent,
   * presence and category, so no combination of the others produces speech
   * inside it. Wraps midnight when start > end; equal bounds mean silent all
   * day; a value the server cannot parse also means silence.
   */
  quiet_hours_start?: string;
  /** End of the quiet-hours window, local `"HH:MM"`. See `quiet_hours_start`. */
  quiet_hours_end?: string;
  /**
   * Comma-separated notification categories that may be SPOKEN unprompted.
   * Defaults to `"alert"` alone. An unrecognised or blank entry matches
   * nothing, so a typo silences that category rather than opening the rest.
   */
  unprompted_speech_categories?: string;

  // API keys are NOT on Settings (PAI-2 P2). They live in the secret store and
  // are managed through listSecretKeys / setSecret / deleteSecret; the server
  // never returns a secret VALUE, only whether the key is set.
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

  // Matter (smart-home fabric). `matter_enabled` is gone, not just absent from
  // this type: the integration runs by default and installs its own controller,
  // so there was nothing left for the field to mean. A stored row from before
  // the removal is ignored on read rather than honoured, because an install that
  // had it off would otherwise have no way back once the toggle went.
  matter_ws_url?: string;
  /** Whether the controller pairs over Bluetooth as well as over the network. */
  matter_ble_enabled?: boolean;

  // Inference stats display
  show_turn_stats?: boolean;

  // ── Settings the server has always accepted but this type could not name ──
  //
  // Every one of these is a real `Settings` field on the Rust side, persisted
  // and writable through `PUT /api/v1/settings` — `update_settings` merges the
  // request body into the stored object with no field allowlist, so anything
  // that round-trips through serde is settable. They were simply absent here,
  // which meant the desktop app could not type a write to them even though a
  // phone or a curl could send one. `catalogue.test.ts` fails if this type and
  // the settings catalogue ever disagree again.
  //
  // Two of them — `network_mode` and `security_policy_mode` — decide whether
  // the pond may reach the internet at all, so "reachable only from curl" was
  // the wrong place for them to live.

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
  resume_compaction_idle_secs?: number;
  compaction_verbatim_days?: number;

  // Retention — the unified events log (#117)
  retention_events_days?: number;
  retention_events_by_category?: Record<string, number>;
  retention_sensitive_days?: number;

  /** Turn cap for VOICE requests; never raised above `agent_max_turns`. */
  voice_max_turns?: number;
  /** ONNX detector that labels motion events. Needs a `vision-onnx` build. */
  vision_classifier_model?: string;
}

/** What the Matter integration is actually doing, as opposed to what was
 *  asked for. `enabled` is the saved setting; `state` is reality — the two
 *  differ while the controller is starting up or cannot be reached. */
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
  /**
   * What the device can be told to do, in `set_device_state`'s verbs — `power`,
   * `brightness`, `fan_speed`, and the rest.
   *
   * `GET /api/v1/devices` has always sent this and this type dropped it, so the
   * Devices card had nothing to gate on and offered every device a power button.
   * A contact sensor's list is empty, which is the fact that stops it being asked
   * to turn on.
   */
  capabilities?: string[];
  /** The address the server knows, when it knows one. */
  ip_address?: string;
  metadata?: Record<string, unknown>;
}

// ── Mesh (#132) ───────────────────────────────────────────────
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

/** Read-only status for the periodic settlement job. No write counterpart —
 * the exchange rate (millisats_per_token) is a settings-API-only knob until
 * that rate is actually decided, deliberately not editable from this UI. */
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
//
// Mirrors `context_index_health` and `rebuild_context_index` in
// `crates/pond-api/src/routes.rs`. Both answer **200 with `indexed: false`**
// when the pond has no vector index or no embedder, because embeddings switched
// off is a working configuration — retrieval falls back to recency and the pond
// answers fine — and an error there would make a healthy pond indistinguishable
// from a broken one at exactly the moment somebody is trying to tell them apart.
//
// So `indexed` is the discriminator and nothing else is: the counts are absent,
// not zero, in that answer, and reading a missing count as 0 would report an
// empty index for a pond that simply never had one.

/** The three stores the index covers, spelled as the server spells them. */
export type ContextCorpus = "memory" | "context" | "summary";

/**
 * One store's share of the index.
 *
 * `coverage` is `null` rather than a number when `rows` is 0, because 0/0 is
 * neither 0% nor 100% and both readings actively mislead — zero paints a
 * permanent red figure on a pond that has simply never stored a memory, and one
 * paints a green 100% on a corpus that is structurally unable to answer
 * anything. Render the `null` as "nothing to index", never as a percentage.
 */
export interface ContextCorpusCoverage {
  corpus: ContextCorpus;
  /** Live rows in the source store that qualify for indexing. */
  rows: number;
  /**
   * Every row in the source table, ignoring the qualifying filter.
   *
   * Never the denominator of coverage. It answers the one question `rows`
   * cannot: is this corpus EMPTY, or is everything in it being excluded? Both
   * read as zero qualifying rows and they need opposite fixes.
   */
  source_rows: number;
  /**
   * `rows === 0` while the table is not empty — a filter is excluding
   * everything, and no amount of embedding repairs it.
   *
   * Measured on a live pond: 27 sessions carried a rolling summary, none
   * qualified, and the index reported itself 100% covered because zero of zero
   * cannot pull an average down.
   */
  structurally_excluded: boolean;
  /** Of those, the ones carrying a vector from the model currently configured. */
  indexed_rows: number;
  missing_rows: number;
  /**
   * Rows holding a vector from some OTHER embedder. They score plausibly and
   * are wrong, so they are unreachable in practice until re-embedded — which is
   * what the rebuild route exists for.
   */
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

/**
 * The result of emptying the index. Every corpus is listed, including the ones
 * at zero, so a corpus that was never populated is still visible afterwards.
 */
export interface ContextIndexRebuild {
  indexed: boolean;
  reason?: string;
  /**
   * Vectors dropped. Taken from the DELETE rather than summed from `corpora`,
   * so rows written under a corpus name a later build stopped using are still
   * counted here.
   */
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
  /**
   * Declared maximum context window, straight from the catalog row the backend
   * persisted. LLM entries only; absent when the catalog provider could not
   * answer. Prefer this over inferring the window from `name` — the name
   * heuristic is a copy of a backend rule that has already moved on.
   */
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
  /**
   * Which generation of the built-in templates this row came from. The reseed
   * stamps the current one on rows it owns; an edited row keeps the generation
   * it was forked from, so `is_customized && factory_version < FACTORY_VERSION`
   * means "there is a newer built-in you have not seen".
   */
  factory_version?: number;
  updated_at?: string;
}

/**
 * Mirrors `FACTORY_VERSION` in
 * `crates/pond-core/src/user_data/domain/prompt_template.rs`. Bump both together
 * — `promptTemplateIsOutdated` compares against this, and a stale copy here
 * means the update notice never appears.
 */
export const PROMPT_FACTORY_VERSION = 1;

/**
 * True when the user's edit predates the current built-in template.
 *
 * Only ever a NOTICE. The row is never adopted on the user's behalf:
 * `is_customized` is the one honest record that somebody chose this text, and
 * the settings adoption it would otherwise imitate (migration 0035) is explicit
 * that a value moves only when the user never set it.
 */
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
}

export type ChatEventType = "text" | "thinking" | "tool_call" | "tool_result" | "done" | "error" | "status" | "review_status" | "review_revision" | "tool_revision" | "turn_stats" | "turn_limit_reached" | "context_warning" | "subagent_progress";

/**
 * PAI-6 P6. Where one delegation has got to.
 *
 * The spellings are `SubagentStatus::as_str` in `pond-core`, pinned against
 * that enum's serde on the Rust side by `the_wire_spelling_is_the_serialized_spelling`.
 * `tool` is the state a delegating turn spends most of its wall clock in, and
 * the only one that says anything is still happening.
 */
export type SubagentStatus =
  | "queued"
  | "running"
  | "tool"
  | "completed"
  | "cancelled"
  | "turn_budget_exhausted"
  | "failed";

// Sent as a fresh user turn when the agent stopped on its turn budget. The
// backend has no dedicated resume endpoint — a continuation IS just the next
// message — so this lives in one place to keep both chat surfaces identical.
export const CONTINUE_TURN_MESSAGE = "Continue where you left off.";

// Per-turn inference performance stats emitted by the backend after each assistant turn.
// All timing/throughput fields may be null when the provider does not report them.
export interface TurnStats {
  type: "turn_stats";
  ttft_ms: number | null;
  prefill_ms: number | null;
  decode_tok_per_sec: number | null;
  prefill_tok_per_sec: number | null;
  prompt_tokens: number;
  completion_tokens: number;
  context_used_tokens: number | null;
  context_limit_tokens: number | null;
  context_pct: number | null;
  model_load_ms: number | null;
  inference_count: number;
}

// PAI-4 P7b. The server pushes this mid-stream when `ContextHealth.should_compact`
// is true for the session that just took a turn (routes.rs, guarded by the
// default-true `context_monitor_enabled`). It is NOT the same shape as the
// `POST /sessions/{id}/compact` response body below, and the two differences are
// the ones a client gets wrong:
//
//  - `turns_remaining` is a raw u32 here, and the monitor uses `u32::MAX`
//    (4294967295) to mean "growth is unknown". The endpoint sends `null` for the
//    same state. Never print this number without clamping it.
//  - `warning` is only populated above 60% utilisation, but `should_compact`
//    also fires through the `estimated_turns_remaining < 3` limb, which can be
//    true below that. So `warning: null` on a `context_warning` frame is a
//    producible state, not a defensive `?`.
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
  /**
   * Why it was skipped. The server's vocabulary, verbatim: monitor_disabled,
   * compaction_disabled, not_under_pressure, no_summariser, already_running,
   * cooling_down, nothing_to_summarise, preempted_by_turn, failed.
   */
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
  /** PAI-6 P6 — present on "subagent_progress" events. The run this frame is
   *  about; one turn may delegate the same role twice, so the id and not the
   *  role is what groups a tree's nodes. */
  task_id?: string;
  /** The delegated role — the label on a tree node. Present on
   *  "subagent_progress" events only; a chat event has no other notion of a
   *  role. */
  role?: string;
  /** Present on "subagent_progress" events. */
  status?: SubagentStatus;
  /** A tool NAME while a child is calling one, or the pond's own reason when a
   *  run failed. Never the child's words, its reasoning, or a tool call's
   *  arguments — the server cannot put those here (PAI-2 minimisation). */
  detail?: string;
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

/** Attachment metadata for a persisted image on a session message. `url` is
 *  relative to the API base, e.g. `/api/v1/sessions/<sid>/attachments/<aid>`. */
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
  /** PAI-5 P6. The reasoning passages this reply was produced by, in emission
   *  order — present only on role="assistant" messages recorded while
   *  `persist_thinking` was on. Absent (not `[]`) when nothing was kept, so a
   *  turn that was never recorded is distinguishable from one that thought
   *  nothing. */
  thinking?: string[];
  /** Training-feedback vote from the chat UI's like/dislike controls.
   *  `true` = liked (kept as training data), `false` = disliked (excluded),
   *  `null`/absent = no vote. */
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
  /**
   * `paused` keeps the partial file and can be resumed; `cancelled` threw it
   * away. Both arrive from the same stop — the transfer checks a flag between
   * chunks, the only moment it is not blocked inside a read.
   */
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

// ── Logs / Telemetry ─────────────────────────────────────────
// ── Activity / Observability ──────────────────────────────────
//
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
  // The backend `Event` domain type has no stable id (GET /api/v1/activity
  // serializes category/action/timestamp/trace_id/etc., not a row id). Optional
  // so consumers derive a stable React key instead of keying on `undefined`.
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

/**
 * A suggestion the pond has made and is waiting on an answer for.
 *
 * Shape mirrors `proposal_json` in `crates/pond-api/src/routes.rs`. Every field
 * the server sends is here; `trigger` is what the pond noticed, which is what
 * makes a suggestion explicable rather than uncanny.
 */
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
