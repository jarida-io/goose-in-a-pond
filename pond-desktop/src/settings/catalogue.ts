// ─── The settings catalogue ────────────────────────────────────────────────
//
// One entry per `Settings` field, grouped the way a household thinks about the
// house rather than the way the Rust struct grew. `docs/architecture/settings-catalogue.md`
// is the prose version of this file and the two are meant to move together.
//
// THE THREE AXES. A setting is not one thing; it is three, and they fail
// independently:
//
//   Store     — is the value persisted?        (guarded in pond-infra)
//   Consumer  — does anything ACT on it?       (this file's `consumer` field)
//   Surface   — can a person change it, where? (this file's existence)
//
// pond-core's `every_settings_field_is_dispositioned` covers Store and Surface.
// Nothing covered Consumer, which is how ten settings came to render as
// operable controls that no code reads. `consumer` is that missing axis, and
// `catalogue.test.ts` fails the build if an entry drifts from the `Settings`
// type.
//
// `consumer` is maintained BY HAND against the Rust tree, so it can go stale in
// one direction the test cannot catch: a setting that gets wired up stays
// marked "none" until somebody edits this file. That is the safe direction —
// it under-promises. The durable fix is `GET /api/v1/settings/schema` serving
// this from the server (Fix 6 in the design doc); until then, re-check with:
//
//   grep -rn --include='*.rs' -w '<key>' crates/ | grep -v sqlite_settings.rs
//
// A key with no hit outside `sqlite_settings.rs` and `domain/settings.rs` has
// no consumer.

import type { Settings } from "../api/types";
import {
  all, atLeast, hhmm, ianaTimezone, integer, latitude, longitude, oneOf,
  optional, range, retentionMap, speechCategories, url, type Validator,
} from "./validation";

/** Does anything read this setting once it is saved? */
export type Consumer =
  /** The pond reads it and acts on it. */
  | "live"
  /** Only this app reads it. The server never does. */
  | "app"
  /** Nothing reads it anywhere. The control is inert. */
  | "none";

/**
 * Where a picker's options come from when they are not a fixed list.
 *
 * Filters mirror `sections/Models.tsx`, which is the app's existing authority
 * on which `provider` values belong to which role.
 */
export type OptionSource = "llm-models" | "whisper-models" | "tts-voices" | "embedding-models" | "llm-providers" | "time-zones";

export type Control =
  | { kind: "toggle" }
  | { kind: "text"; placeholder?: string }
  | { kind: "number"; step?: number; min?: number; max?: number; unit?: string }
  /** A short, consequential choice — all options visible without opening anything. */
  | { kind: "radio"; options: readonly { value: string; label: string; hint?: string }[] }
  | { kind: "select"; options: readonly string[] }
  /** A picker filled from the model registry. Falls back to free text offline. */
  | { kind: "lookup"; source: OptionSource; placeholder?: string; allowCustom?: boolean };

export interface Entry {
  /** The `Settings` field. Typed, so a typo is a build error. */
  key: keyof Settings;
  /** What the person controls, in their words — never the field name. */
  label: string;
  /**
   * What it does, in one line, always shown under the label. Lifted from the
   * doc comment on the `Settings` field so the explanation has exactly one
   * home — if the behaviour changes, the Rust comment is what gets edited.
   */
  description: string;
  control: Control;
  consumer: Consumer;
  /**
   * Why the mark is not "live", in plain language. Required for `app` and
   * `none` — a mark the interface will not explain is worse than no mark.
   */
  note?: string;
  /**
   * No control existed in the desktop app before this catalogue; the setting
   * was reachable only through the API. Rendered as a "New" badge so the
   * design reads as a proposal rather than a claim about what shipped.
   */
  proposed?: boolean;
  /**
   * This field is a mirror, and something else owns the decision. `pond-server`
   * syncs the `active_*` keys FROM `model_role_assignments` — "the join table is
   * the source of truth" — so a control here would lose to the next sync.
   *
   * The entry stays in the catalogue because `catalogue.test.ts` proves every
   * `Settings` field has a home, and deleting it would only make the field
   * unaccounted for rather than unrendered. Settings shows a pointer to the
   * owning surface instead of a control.
   */
  ownedBy?: "models";
  /**
   * Checked on every edit; a message blocks Save and renders under the control.
   * See `validation.ts` for why the client checks what the server does not.
   */
  validate?: Validator;
}

export interface Subcategory {
  name: string;
  entries: Entry[];
}

export type Tier = "Household" | "Workshop";

export interface Category {
  id: string;
  name: string;
  tier: Tier;
  /** One line under the category title. Says what the category answers. */
  blurb: string;
}

export interface CatalogueCategory extends Category {
  groups: Subcategory[];
}

/** Shown once per tier in the rail, above its categories. */
export const TIER_NOTE: Record<Tier, string> = {
  Household: "What the people living here set.",
  Workshop: "Tuning for how the pond thinks and runs. Safe to leave alone.",
};

/** The tiers `pond_adapters_kokoro::model_filename` recognises, smallest first. */
const TTS_QUALITY = ["q4", "q4f16", "q8", "q8f16", "fp16", "fp32"] as const;

export const CATALOGUE: CatalogueCategory[] = [
  // ── Household ───────────────────────────────────────────────────────────
  {
    id: "account",
    name: "Account & Home",
    tier: "Household",
    blurb: "Who lives here, what the home is called, and where in the world it is.",
    groups: [
      {
        name: "Who lives here",
        entries: [
          { key: "user_name", label: "Your name", description: "What the pond should call you.", control: { kind: "text", placeholder: "Friend" }, consumer: "live" },
          { key: "primary_profile_id", label: "Household profile", description: "The main person this pond belongs to.", control: { kind: "text", placeholder: "Not set" }, consumer: "live" },
        ],
      },
      {
        name: "Where and when",
        entries: [
          {
            key: "home_name", label: "Home name", description: "What to call this home. Used in greetings.",
            control: { kind: "text", placeholder: "Not set" }, consumer: "app",
            note: "Shown in this app only. The assistant never sees it — it greets you using your name and location instead.",
          },
          {
            key: "timezone", label: "Time zone", description: "The time zone this home is in, so schedules land at the right hour.",
            // A lookup, not a hand list. There were three lists in this app
            // — 16, 18 and 13 zones, no two alike — so a household in Kampala
            // could not pick its own zone anywhere. This one comes from the
            // server's IANA catalogue, which is also what saving validates
            // against, so the picker cannot offer a zone the save would reject.
            control: { kind: "lookup", source: "time-zones", placeholder: "Choose a time zone" },
            consumer: "live",
            validate: ianaTimezone,
          },
          { key: "weather_location_name", label: "Location", description: "The place name used when the pond talks about the weather.", control: { kind: "text", placeholder: "Nairobi" }, consumer: "live" },
          { key: "weather_enabled", label: "Use local weather", description: "Let the pond know the local weather.", control: { kind: "toggle" }, consumer: "live" },
          // Filled in for you: saving a location name the coordinates do not
          // match makes the server geocode it and answer with the result.
          // Editing either number by hand suppresses that, so a deliberate
          // coordinate is never overwritten by a name lookup.
          { key: "weather_latitude", label: "Latitude", description: "Latitude of the place used for weather.", control: { kind: "number", step: 0.0001, min: -90, max: 90, unit: "°" }, consumer: "live", validate: optional(latitude) },
          { key: "weather_longitude", label: "Longitude", description: "Longitude of the place used for weather.", control: { kind: "number", step: 0.0001, min: -180, max: 180, unit: "°" }, consumer: "live", validate: optional(longitude) },
        ],
      },
    ],
  },
  {
    id: "prompts",
    name: "Prompts & Personality",
    tier: "Household",
    blurb: "What the assistant calls itself, and how it talks to you.",
    groups: [
      {
        name: "Voice and manner",
        entries: [
          { key: "assistant_name", label: "Assistant name", description: "What the pond calls itself.", control: { kind: "text", placeholder: "Goose" }, consumer: "live" },
          { key: "assistant_personality", label: "Personality", description: "A short note on the manner it should answer in.", control: { kind: "text", placeholder: "friendly and concise" }, consumer: "live" },
          {
            key: "prompt_style", label: "Prompt style", description: "The overall tone it writes in.", consumer: "live",
            control: { kind: "radio", options: [
              { value: "balanced",  label: "Balanced",  hint: "Warm, and gets to the point" },
              { value: "concise",   label: "Concise",   hint: "As few words as the answer allows" },
              { value: "technical", label: "Technical", hint: "Precise, assumes you know the terms" },
              { value: "warm",      label: "Warm",      hint: "Conversational and unhurried" },
            ] },
            validate: oneOf(["balanced", "concise", "technical", "warm"]),
          },
        ],
      },
      {
        name: "Advanced",
        entries: [
          { key: "prompt_addendum", label: "Extra instructions", description: "An extra standing instruction, added to everything it already knows.", control: { kind: "text", placeholder: "Always answer in Swahili." }, consumer: "live" },
          { key: "custom_system_prompt", label: "Replace the system prompt", description: "Replace its standing instructions entirely. Leave empty to keep the built-in ones.", control: { kind: "text", placeholder: "Uses the prompt style above" }, consumer: "live" },
        ],
      },
    ],
  },
  {
    id: "voice",
    name: "Voice",
    tier: "Household",
    blurb: "What wakes the assistant, how it listens, and the voice it answers in.",
    groups: [
      {
        name: "Wake word",
        entries: [
          { key: "voice_wake_word", label: "Wake word", description: "The phrase that wakes the pond. Capitalisation does not matter.", control: { kind: "text", placeholder: "goose" }, consumer: "live" },
          { key: "voice_wake_word_transcriptions", label: "Learned pronunciations", description: "Alternative spellings learned while training the wake word, so it still answers.", control: { kind: "text", placeholder: "Not calibrated" }, consumer: "live" },
        ],
      },
      {
        name: "Listening",
        entries: [
          { key: "voice_kws_energy_threshold", label: "Ignore sound quieter than", description: "How loud a sound must be before the pond bothers listening for its name.", control: { kind: "number", step: 0.001, min: 0, max: 1 }, consumer: "live", validate: range(0, 1) },
          { key: "voice_kws_post_trigger_silence_ms", label: "Stop after silence", description: "How long a pause means you have finished speaking.", control: { kind: "number", min: 0, unit: "ms" }, consumer: "live", validate: all(integer, atLeast(0, "ms")) },
          { key: "voice_kws_cooldown_ms", label: "Wait before listening again", description: "How long to wait after answering before listening for its name again.", control: { kind: "number", min: 0, unit: "ms" }, consumer: "live", validate: all(integer, atLeast(0, "ms")) },
          {
            key: "voice_recording_duration_secs", label: "Recording length", description: "How long it records after you speak to it.",
            control: { kind: "number", min: 1, unit: "seconds" }, consumer: "app",
            note: "This app uses it. The pond's own microphone loop does not — it ends a recording on silence instead.",
            validate: all(integer, range(1, 300, "seconds")),
          },
          {
            key: "voice_kws_whisper_url", label: "Wake-word server", description: "A separate listening service used only for hearing the wake word.",
            control: { kind: "text", placeholder: "Uses the transcription server" }, consumer: "none",
            note: "Nothing reads this. Wake-word detection uses the transcription server below, whatever you type here.",
            validate: optional(url(["http://", "https://"], "http://127.0.0.1:9000")),
          },
        ],
      },
      {
        name: "Transcription",
        entries: [
          {
            key: "voice_whisper_url", label: "Transcription server", description: "Address of the service that turns speech into text.",
            control: { kind: "text", placeholder: "http://127.0.0.1:9000" }, consumer: "live",
            validate: optional(url(["http://", "https://"], "http://127.0.0.1:9000")),
          },
          { key: "active_whisper_model", label: "Whisper model", ownedBy: "models", description: "What turns your speech into text. Chosen on the Models page.", control: { kind: "lookup", source: "whisper-models", placeholder: "Choose a downloaded model" }, consumer: "live" },
        ],
      },
      {
        name: "Speaking",
        entries: [
          { key: "voice_tts_voice", label: "Voice", description: "The voice the pond speaks in.", control: { kind: "lookup", source: "tts-voices", placeholder: "Choose a downloaded voice" }, consumer: "live" },
          // Pace is the engine's `speed` multiplier, not a percentage — stored
          // exactly as the model takes it so there is no conversion to get
          // backwards between here and the synthesiser.
          { key: "voice_tts_speed", label: "Speaking pace", description: "How fast it speaks. 1.0 is its natural pace.", control: { kind: "number", step: 0.05, min: 0.5, max: 2 }, consumer: "live", validate: range(0.5, 2) },
          { key: "voice_tts_quality", label: "Voice quality", description: "How much detail the voice is made with. Higher sounds smoother and takes longer.", control: { kind: "select", options: TTS_QUALITY }, consumer: "live" },
          // No `note`: notes are reserved for controls that are not connected,
          // and this one is. What the tone is for is said in the Voice view's
          // row subtitle, where an explanation does not double as a warning.
          { key: "voice_thinking_tone_enabled", label: "Sound while it thinks", description: "Play a soft tone while it is thinking, so silence does not read as broken.", control: { kind: "toggle" }, consumer: "live" },
          { key: "active_tts_model", label: "Speech model", ownedBy: "models", description: "The voice used for speaking. Chosen on the Models page.", control: { kind: "text", placeholder: "af_heart" }, consumer: "live" },
          { key: "voice_max_turns", label: "Steps before answering aloud", description: "How much work it may do before answering aloud. Fewer steps means a faster reply.", control: { kind: "number", min: 0, max: 50 }, consumer: "live", proposed: true, validate: all(integer, range(0, 50)) },
        ],
      },
    ],
  },
  {
    id: "models",
    name: "Models",
    tier: "Household",
    blurb: "Which model answers you, and how much room it has to work in.",
    groups: [
      {
        name: "Chat model",
        entries: [
          { key: "chat_provider", label: "Provider", description: "Where conversation runs. Leave unset to use the same place as everything else.", control: { kind: "lookup", source: "llm-providers", placeholder: "Choose a provider" }, consumer: "live" },
          { key: "chat_model", label: "Model", description: "Which model handles conversation. Leave unset to use the one on the Models page.", control: { kind: "lookup", source: "llm-models", placeholder: "Choose a downloaded model" }, consumer: "live" },
          { key: "llm_provider", label: "Startup provider", description: "Where the pond runs its thinking.", control: { kind: "lookup", source: "llm-providers", placeholder: "Same as above" }, consumer: "live", proposed: true },
          { key: "active_llm_model", label: "Last selected model", ownedBy: "models", description: "The model that does the thinking. Chosen on the Models page.", control: { kind: "lookup", source: "llm-models", placeholder: "Not set" }, consumer: "live", proposed: true },
        ],
      },
      {
        name: "Generation",
        entries: [
          { key: "llm_max_tokens", label: "Longest reply", description: "How long a single answer may get.", control: { kind: "number", min: 1, max: 131072, unit: "tokens" }, consumer: "live", validate: all(integer, range(1, 131072, "tokens")) },
          { key: "llm_temperature", label: "Creativity", description: "How predictable its answers are. Lower is steadier, higher is more inventive.", control: { kind: "number", step: 0.05, min: 0, max: 2 }, consumer: "live", validate: range(0, 2) },
          // 0 means "use the model's own window", so the floor is 0 and not 1.
          { key: "context_window_override", label: "Context window cap", description: "Limit how much it may hold in mind at once. Zero uses the model's own limit.", control: { kind: "number", min: 0, max: 1048576, unit: "tokens" }, consumer: "live", validate: all(integer, atLeast(0, "tokens")) },
        ],
      },
      {
        name: "Engine",
        entries: [
          // "pond" is refused by the server with a 422 — the backend is
          // quarantined (Q2-05), so offering it would be offering a failure.
          {
            key: "agent_backend", label: "Agent engine", description: "Which engine runs the assistant. Only one is available.", consumer: "live",
            control: { kind: "radio", options: [
              { value: "goose", label: "Goose", hint: "The only engine that ships today" },
            ] },
            validate: oneOf(["goose"]),
          },
        ],
      },
      {
        name: "Embeddings",
        entries: [
          {
            key: "embedding_provider", label: "Embedding provider", description: "What makes your memories searchable.", consumer: "live",
            control: { kind: "radio", options: [
              { value: "gguf",      label: "On-device (GGUF)", hint: "Runs through llama.cpp. The only one that starts on a Jetson" },
              { value: "fastembed", label: "FastEmbed",        hint: "ONNX Runtime. Does not initialise on the Orin" },
              { value: "none",      label: "Off",              hint: "No semantic memory or context search" },
            ] },
            validate: oneOf(["fastembed", "gguf", "none"]),
          },
          { key: "active_embedding_model", label: "Embedding model", ownedBy: "models", description: "The model used to make memories searchable. Chosen on the Models page.", control: { kind: "lookup", source: "embedding-models", placeholder: "Not set" }, consumer: "live" },
        ],
      },
    ],
  },
  {
    id: "memory",
    name: "Memory",
    tier: "Household",
    blurb: "What the assistant keeps about your household, and what it lets go of.",
    groups: [
      {
        name: "What it remembers",
        entries: [
          { key: "agent_memory_inject", label: "Use memories in replies", description: "Let it recall what it knows about you when answering.", control: { kind: "toggle" }, consumer: "live" },
          { key: "agent_memory_limit", label: "Memories per reply", description: "How many remembered things it may bring to a single answer.", control: { kind: "number", min: 0, max: 100 }, consumer: "live", validate: all(integer, range(0, 100)) },
          { key: "memory_extraction_enabled", label: "Learn from conversations", description: "Read your conversations back in quiet moments and remember what lasts. Nothing is remembered while you are talking, so this takes a while to show up \u2014 and a long history takes a few nights.", control: { kind: "toggle" }, consumer: "live" },
          { key: "memory_extraction_max_facts", label: "Most things kept at once", description: "How many things it may remember from one stretch of conversation. Fewer is better: a store full of near-misses crowds out what matters.", control: { kind: "number", min: 0, max: 50 }, consumer: "live", validate: all(integer, range(0, 50)) },
          { key: "memory_extraction_interval_secs", label: "Wait between readings", description: "The shortest gap between two readings. It only ever makes them rarer.", control: { kind: "number", min: 0, unit: "seconds" }, consumer: "live", validate: all(integer, atLeast(0, "seconds")) },
          { key: "suggestion_generation_enabled", label: "Suggest things to ask", description: "Turn what it remembers about you into questions on the Home screen. Off, Home still suggests \u2014 but only the same general questions every day.", control: { kind: "toggle" }, consumer: "live" },
        ],
      },
      {
        name: "Forgetting",
        entries: [
          { key: "memory_cleanup_enabled", label: "Let memories fade", description: "Let old memories fade and be tidied away on their own.", control: { kind: "toggle" }, consumer: "live" },
          { key: "memory_cleanup_interval_hours", label: "Check for faded memories every", description: "How often to tidy old memories.", control: { kind: "number", min: 1, unit: "hours" }, consumer: "live", validate: all(integer, atLeast(1, "hours")) },
          { key: "memory_prune_threshold", label: "Delete below", description: "How faded a memory must be before it is deleted.", control: { kind: "number", step: 0.01, min: 0, max: 1 }, consumer: "live", validate: range(0, 1) },
          { key: "memory_archive_threshold", label: "Archive below", description: "How faded a memory must be before it is hidden but kept.", control: { kind: "number", step: 0.01, min: 0, max: 1 }, consumer: "live", validate: range(0, 1) },
          { key: "memory_decay_base_half_life_days", label: "Half-life", description: "How long an ordinary memory takes to fade by half. Important ones last longer.", control: { kind: "number", step: 0.25, min: 0, unit: "days" }, consumer: "live", proposed: true, validate: atLeast(0, "days") },
          { key: "memory_decay_beta", label: "Fade curve", description: "How sharply memories fade. Lower is gentler.", control: { kind: "number", step: 0.05, min: 0, max: 10 }, consumer: "live", proposed: true, validate: range(0, 10) },
        ],
      },
      {
        name: "Tidying up",
        entries: [
          { key: "memory_consolidation_enabled", label: "Merge duplicate memories", description: "Merge memories that duplicate or contradict each other.", control: { kind: "toggle" }, consumer: "live" },
          {
            key: "memory_consolidation_mode", label: "How it merges", description: "How carefully memories are merged. The thorough option takes longer.", consumer: "live",
            control: { kind: "radio", options: [
              { value: "single",      label: "One pass",    hint: "A single model call. Cheapest on-device" },
              { value: "adversarial", label: "Three passes", hint: "Proposer, adversary, judge. Slower, catches more" },
            ] },
            validate: oneOf(["single", "adversarial"]),
          },
          { key: "memory_consolidation_interval_hours", label: "Merge every", description: "How often to merge duplicate memories.", control: { kind: "number", min: 1, unit: "hours" }, consumer: "live", validate: all(integer, atLeast(1, "hours")) },
          { key: "memory_consolidation_batch_size", label: "Memories per pass", description: "How many memories to work through at a time.", control: { kind: "number", min: 1, max: 1000 }, consumer: "live", validate: all(integer, range(1, 1000)) },
        ],
      },
      {
        name: "Experimental",
        entries: [
          {
            key: "memory_graph_enabled", label: "Follow links between memories", description: "Follow links between memories to find related ones. Experimental.",
            control: { kind: "toggle" }, consumer: "none",
            note: "Nothing reads this. The links are stored, but no reply has ever followed one.",
          },
        ],
      },
    ],
  },
  {
    id: "privacy",
    name: "Privacy & Security",
    tier: "Household",
    blurb: "What the pond may sense, where it may reach, and what it records about itself.",
    groups: [
      {
        name: "Sensors",
        entries: [
          { key: "mic_enabled", label: "Microphone", description: "Let the pond use the microphone. Off means it cannot hear anything.", control: { kind: "toggle" }, consumer: "live" },
          {
            key: "cameras_enabled", label: "Cameras", description: "Let the pond use the camera. Off means it cannot see anything.",
            control: { kind: "toggle" }, consumer: "none",
            note: "Nothing reads this. Turning it off does not stop the cameras — only Watch the camera, under Vision & Cameras, does that.",
          },
        ],
      },
      {
        name: "Reach",
        entries: [
          {
            key: "network_mode", label: "Network reach", description: "How much of the internet it may reach: everything, only what you allow, or nothing.", consumer: "live", proposed: true,
            // A radio, not a dropdown: this is the setting that decides whether
            // the pond can talk to the internet, and all three answers should
            // be readable without opening anything.
            control: { kind: "radio", options: [
              { value: "open",      label: "Open",       hint: "Every outbound call is recorded, none refused" },
              { value: "allowlist", label: "Allowed only", hint: "Refuses hosts that are not loopback or on the curated list" },
              { value: "offline",   label: "Offline",    hint: "Refuses everything except this machine" },
            ] },
            validate: oneOf(["open", "allowlist", "offline"]),
          },
          {
            key: "cloud_fallback_enabled", label: "Fall back to a cloud model", description: "Let it ask a cloud service when the local model cannot answer.",
            control: { kind: "toggle" }, consumer: "none",
            note: "Nothing reads this. There is no cloud fallback to switch on yet — the pond stays local either way.",
          },
          { key: "mesh_enabled", label: "Talk to your other ponds", description: "Let this pond talk to your other ponds.", control: { kind: "toggle" }, consumer: "live", proposed: true },
        ],
      },
      {
        name: "Policy",
        entries: [
          {
            key: "security_policy_mode", label: "Permission checks", description: "Whether permission rules are ignored, recorded, or enforced.", consumer: "live", proposed: true,
            control: { kind: "radio", options: [
              { value: "audit",   label: "Watch",   hint: "Records every decision, blocks nothing. The shipped default" },
              { value: "enforce", label: "Enforce", hint: "Denials bite. Read the activity log first" },
              { value: "off",     label: "Off",     hint: "No checks and no record. Debugging only" },
            ] },
            validate: oneOf(["off", "audit", "enforce"]),
          },
          { key: "telemetry_enabled", label: "Record how it performs", description: "Keep a record of how fast it responds, so you can see how it is performing.", control: { kind: "toggle" }, consumer: "live" },
          { key: "context_ingest_enabled", label: "Build personal context from sensors", description: "Let what the sensors and cameras notice become part of what it knows.", control: { kind: "toggle" }, consumer: "live", proposed: true },
        ],
      },
    ],
  },
  {
    id: "extensions",
    name: "Extensions & Tools",
    tier: "Household",
    blurb: "Which capabilities the assistant can reach for, and how they are offered to it.",
    groups: [
      {
        name: "Capabilities",
        entries: [
          { key: "ext_memory_enabled", label: "Memories", description: "Let it remember, recall and forget things on request.", control: { kind: "toggle" }, consumer: "live" },
          { key: "ext_schedule_enabled", label: "Schedules", description: "Let it create, pause and run things on a schedule.", control: { kind: "toggle" }, consumer: "live" },
          { key: "ext_weather_enabled", label: "Weather", description: "Let it look up the weather.", control: { kind: "toggle" }, consumer: "live" },
          { key: "ext_knowledge_enabled", label: "Knowledge", description: "Let it look things up in an encyclopedia.", control: { kind: "toggle" }, consumer: "live" },
          { key: "ext_system_enabled", label: "System", description: "Let it read files and check on the device it runs on.", control: { kind: "toggle" }, consumer: "live" },
          { key: "ext_device_enabled", label: "Devices", description: "Let it see and change the devices in this home.", control: { kind: "toggle" }, consumer: "live" },
          { key: "ext_sensor_enabled", label: "Sensors", description: "Let it read what the sensors in this home have recorded.", control: { kind: "toggle" }, consumer: "live" },
          { key: "ext_orchestrator_enabled", label: "Delegation", description: "Let it hand part of a job to a helper working on its own.", control: { kind: "toggle" }, consumer: "live" },
          { key: "ext_context_enabled", label: "Personal context", description: "Let it read the notes and documents this home has given it.", control: { kind: "toggle" }, consumer: "live", proposed: true },
        ],
      },
      {
        name: "How tools are offered",
        entries: [
          {
            key: "tool_selection_mode", label: "Tools sent each turn", description: "Whether it is offered everything it can do, or only what suits the question.", consumer: "live",
            control: { kind: "radio", options: [
              { value: "all",      label: "All of them", hint: "Every enabled tool, every turn. About 3.3K tokens, 41% of the prompt budget" },
              { value: "relevant", label: "The relevant ones", hint: "A small core plus what this conversation seems to need" },
              { value: "minimal",  label: "None until asked for", hint: "Only the two tools that load a group. 222 tokens, 2.7%" },
            ] },
            validate: oneOf(["all", "relevant", "minimal"]),
          },
          { key: "tool_model", label: "Tool-call helper model", description: "A small helper model that tidies up requests the main one gets wrong.", control: { kind: "lookup", source: "llm-models", placeholder: "Not set" }, consumer: "live" },
          {
            key: "searxng_url", label: "Search server", description: "Address of your own search server, if you run one.",
            control: { kind: "text", placeholder: "Not set" }, consumer: "live", proposed: true,
            validate: optional(url(["http://", "https://"], "http://127.0.0.1:8888")),
          },
        ],
      },
      {
        name: "Tool handling",
        entries: [
          {
            key: "tool_output_compaction", label: "Shorten tool results", description: "Shorten what a lookup returns before it is read, to leave more room.",
            control: { kind: "toggle" }, consumer: "none",
            note: "Nothing reads this. Tool results reach the model in full, whatever this says.",
          },
          {
            key: "tool_call_validation", label: "Repair malformed tool calls", description: "Check and repair its requests before running them.",
            control: { kind: "toggle" }, consumer: "none",
            note: "Nothing reads this. No repair step exists in the pond yet.",
          },
          {
            key: "tool_request_detection", label: "Catch tool requests in replies", description: "Notice when it says it will look something up, and actually do it.",
            control: { kind: "toggle" }, consumer: "none",
            note: "Nothing reads this. Replies are not scanned for tool requests.",
          },
          {
            key: "multi_tool_enabled", label: "Run tools at the same time", description: "Let it do several things at once. Experimental.",
            control: { kind: "toggle" }, consumer: "none",
            note: "Nothing reads this. Tools run one after another.",
          },
        ],
      },
    ],
  },
  {
    id: "automation",
    name: "Automations",
    tier: "Household",
    blurb: "What runs on its own, what it is doing right now, and whether the assistant may speak before you do.",
    groups: [
      {
        name: "Schedules",
        entries: [
          { key: "schedule_max_concurrent", label: "Tasks at once", description: "How many scheduled jobs may run at the same time.", control: { kind: "number", min: 1, max: 32 }, consumer: "live", validate: all(integer, range(1, 32)) },
          { key: "schedule_max_runs_per_task", label: "History per task", description: "How much history to keep for each scheduled job.", control: { kind: "number", min: 0, max: 10000 }, consumer: "live", validate: all(integer, range(0, 10000)) },
          {
            key: "schedule_result_notify", label: "Tell me when a task finishes", description: "Tell you when a scheduled job finishes.",
            control: { kind: "toggle" }, consumer: "none",
            note: "Nothing reads this. Finished tasks are announced either way.",
          },
        ],
      },
      {
        name: "Speaking unprompted",
        entries: [
          { key: "unprompted_speech_enabled", label: "Speak without being asked", description: "Let it speak first, without being spoken to.", control: { kind: "toggle" }, consumer: "live" },
          { key: "unprompted_speech_categories", label: "Only for", description: "Which kinds of news it may say out loud unprompted.", control: { kind: "text", placeholder: "alert" }, consumer: "live", validate: speechCategories },
          // Free text, not a time picker: a value the server cannot parse means
          // silence, and a picker renders such a value as blank — which reads
          // as "not set" when it actually means "quiet all day".
          // Validated hard, because the server's failure mode here is silence:
          // a time it cannot parse makes `quiet_hours_cover` fail closed and the
          // pond never speaks, with nothing anywhere explaining why.
          { key: "quiet_hours_start", label: "Quiet from", description: "When it should stop speaking unprompted for the night.", control: { kind: "text", placeholder: "22:00" }, consumer: "live", validate: hhmm },
          { key: "quiet_hours_end", label: "Quiet until", description: "When it may start speaking unprompted again.", control: { kind: "text", placeholder: "07:00" }, consumer: "live", validate: hhmm },
        ],
      },
      {
        name: "Thinking unprompted",
        entries: [
          { key: "proactive_review_enabled", label: "Review the day on its own", description: "Let it think over the day without being asked.", control: { kind: "toggle" }, consumer: "live" },
          // Written from the Home card ("Don't suggest this"), and undoable
          // here -- a mute with no way back is worse than no mute. The control
          // is text because the value is a list of suggestor ids; the card is
          // the place you actually use it, and this is the place you take it
          // back.
          { key: "suggestions_muted", label: "Suggestions you have hidden", description: "Kinds of suggestion Home will not offer. Clear this to see them again.", control: { kind: "text", placeholder: "Nothing hidden" }, consumer: "live" },
          // Only ever touches names the pond wrote itself. A title typed by
          // hand is left alone whatever this is set to, so the control does not
          // need to warn about losing one.
          { key: "session_titling_enabled", label: "Give conversations better names", description: "Let it name your conversations while it is idle.", control: { kind: "toggle" }, consumer: "live" },
        ],
      },
    ],
  },

  // ── Workshop ────────────────────────────────────────────────────────────
  {
    id: "reasoning",
    name: "Reasoning",
    tier: "Workshop",
    blurb: "Whether the model thinks before answering, how long, and who checks its work.",
    groups: [
      {
        name: "Thinking",
        entries: [
          {
            key: "thinking_mode", label: "Thinking", description: "Whether it reasons before answering. Automatic decides per model.", consumer: "live",
            control: { kind: "radio", options: [
              { value: "auto", label: "When the model supports it", hint: "The default" },
              { value: "on",   label: "Always try",  hint: "Even on models that ignore it" },
              { value: "off",  label: "Never",       hint: "Removes the section from the prompt entirely" },
            ] },
            validate: oneOf(["auto", "on", "off"]),
          },
          {
            key: "reasoning_effort", label: "How long it may think", description: "How much thinking to encourage before it answers.", consumer: "live",
            control: { kind: "radio", options: [
              { value: "brief",    label: "Brief",    hint: "On a Jetson every thinking token is silence before the answer" },
              { value: "balanced", label: "Balanced", hint: "" },
              { value: "thorough", label: "Thorough", hint: "Right on a hosted provider, slow on-device" },
            ] },
            validate: oneOf(["brief", "balanced", "thorough"]),
          },
          { key: "show_thinking", label: "Show thinking as it happens", description: "Watch it think while it works.", control: { kind: "toggle" }, consumer: "live" },
          { key: "persist_thinking", label: "Keep thinking after the reply", description: "Keep its reasoning after it answers, so you can reopen it later.", control: { kind: "toggle" }, consumer: "live" },
        ],
      },
      {
        name: "Answer review",
        entries: [
          {
            key: "review_mode", label: "Review answers", description: "Whether answers are checked before you see them. Checking is slower but catches more.", consumer: "live",
            control: { kind: "radio", options: [
              { value: "off",  label: "Never",             hint: "Answers stream straight to you" },
              { value: "auto", label: "Factual questions", hint: "Only where a wrong answer would be quotable" },
              { value: "on",   label: "Every answer",      hint: "Doubles the model calls per turn" },
            ] },
            validate: oneOf(["off", "on", "auto"]),
          },
          { key: "review_max_rounds", label: "Revision rounds", description: "How many times an answer may be sent back for another try.", control: { kind: "number", min: 0, max: 5 }, consumer: "live", validate: all(integer, range(0, 5)) },
          { key: "review_pass_threshold", label: "Pass mark", description: "How good an answer must be to pass the check.", control: { kind: "number", min: 1, max: 5 }, consumer: "live", validate: all(integer, range(1, 5)) },
          { key: "goal_check_enabled", label: "Check the question was answered", description: "Ask whether it actually answered you, and let it keep going if not.", control: { kind: "toggle" }, consumer: "live", proposed: true },
        ],
      },
    ],
  },
  {
    id: "performance",
    name: "Performance & Context",
    tier: "Workshop",
    blurb: "How long a request may run, and how history is trimmed to fit the device.",
    groups: [
      {
        name: "Turn budget",
        entries: [
          // 0 is meaningful here (uncapped), so the floor is 0 rather than 1.
          { key: "agent_max_turns", label: "Steps per request", description: "How many steps it may take on one request before it must answer.", control: { kind: "number", min: 0, max: 500 }, consumer: "live", validate: all(integer, range(0, 500)) },
          { key: "agent_timeout_secs", label: "Give up after silence", description: "How long to wait in silence before giving up on a reply.", control: { kind: "number", min: 0, unit: "seconds" }, consumer: "live", validate: all(integer, atLeast(0, "seconds")) },
          {
            key: "agent_goose_mode", label: "Agent mode", description: "How much freedom it has to act on its own.",
            control: { kind: "select", options: ["auto", "chat", "smart"] }, consumer: "none",
            note: "Nothing reads this. The mode comes from the request, not from here.",
          },
        ],
      },
      {
        name: "Prompt cache",
        entries: [
          { key: "prefix_cache_prompt", label: "Reuse the prompt prefix", description: "Reuse the unchanging part of its instructions so replies start sooner.", control: { kind: "toggle" }, consumer: "live" },
        ],
      },
      {
        name: "Compaction",
        entries: [
          { key: "hybrid_compaction_enabled", label: "Trim history as you go", description: "Trim long conversations as you go, so they keep fitting.", control: { kind: "toggle" }, consumer: "live", proposed: true },
          { key: "summary_idle_secs", label: "Summarise after idle", description: "How long you must be idle before it summarises the conversation so far.", control: { kind: "number", min: 0, unit: "seconds" }, consumer: "live", proposed: true, validate: all(integer, atLeast(0, "seconds")) },
          // The server floors this at MIN_RESUME_IDLE_SECS; too SMALL is the
          // damaging direction, so the client refuses the values that would be
          // silently corrected rather than letting them look accepted.
          { key: "compaction_verbatim_days", label: "Keep in full for", description: "How many days of conversation to keep word for word before shortening it.", control: { kind: "number", min: 0, unit: "days" }, consumer: "live", proposed: true, validate: all(integer, atLeast(0, "days")) },
        ],
      },
      {
        name: "Monitoring",
        entries: [
          { key: "context_monitor_enabled", label: "Warn before context fills", description: "Warn you before a conversation gets too long to hold.", control: { kind: "toggle" }, consumer: "live" },
          {
            key: "show_turn_stats", label: "Show speed under each reply", description: "Show how fast each reply was, under the message.",
            control: { kind: "toggle" }, consumer: "app",
            note: "This app draws it. The pond does not read it — it measures every turn regardless.",
          },
        ],
      },
    ],
  },
  {
    id: "vision",
    name: "Vision & Cameras",
    tier: "Workshop",
    blurb: "The camera pipeline itself, and the Matter controller that drives your devices.",
    groups: [
      {
        name: "Camera pipeline",
        entries: [
          { key: "vision_enabled", label: "Watch the camera", description: "Watch the camera for movement. Needs a camera attached.", control: { kind: "toggle" }, consumer: "live" },
          {
            key: "vision_camera_url", label: "Camera address", description: "Where the camera is: a network address or a socket on this device.",
            control: { kind: "text", placeholder: "rtsp://… or /dev/video0" }, consumer: "live",
            // A device path is not a URL, so this accepts either shape rather
            // than insisting on a scheme the local-camera case does not have.
            validate: optional((v) => {
              const t = String(v).trim();
              if (t.startsWith("/dev/")) return null;
              return url(["rtsp://", "http://", "https://"], "rtsp://camera.local/stream")(t);
            }),
          },
          { key: "vision_camera_id", label: "Camera name", description: "A name for this camera, used in the activity feed and in rules.", control: { kind: "text", placeholder: "camera-1" }, consumer: "live" },
          { key: "vision_fps", label: "Frames per second", description: "How often to look at the picture. Low on purpose, to leave room for thinking.", control: { kind: "number", min: 1, max: 30, unit: "fps" }, consumer: "live", validate: all(integer, range(1, 30, "fps")) },
          { key: "vision_motion_threshold", label: "Motion sensitivity", description: "How much of the picture must change to count as movement.", control: { kind: "number", step: 0.01, min: 0, max: 1 }, consumer: "live", validate: range(0, 1) },
          { key: "vision_classifier_model", label: "Detector model", description: "What names what the camera saw. Empty uses the built-in one.", control: { kind: "text", placeholder: "Bundled YOLOX-Nano" }, consumer: "live", proposed: true },
        ],
      },
      {
        name: "Matter",
        entries: [
          // No enable toggle: Matter runs by default and installs its own
          // controller, so the only question left is where that controller is —
          // and that only matters to someone running their own.
          {
            key: "matter_ws_url", label: "Controller address", description: "Address of your smart-home controller.",
            control: { kind: "text", placeholder: "ws://127.0.0.1:5580/giap" }, consumer: "live",
            // Mirrors the server's own 422 so the message arrives before the
            // round-trip rather than instead of it.
            validate: optional(url(["ws://", "wss://"], "ws://127.0.0.1:5580/giap")),
          },
          {
            key: "matter_ble_enabled", label: "Pair over Bluetooth",
            description: "Needed for a brand-new device, which announces itself over Bluetooth before it is on your network.",
            control: { kind: "toggle" }, consumer: "live",
          },
        ],
      },
    ],
  },
  {
    id: "retention",
    name: "Data & Retention",
    tier: "Workshop",
    blurb: "How long anything is kept before the pond deletes it, and what cloud would have cost.",
    groups: [
      {
        name: "How long things are kept",
        entries: [
          // 0 means "keep forever" for every one of these, which is why none of
          // them has a floor of 1.
          { key: "retention_event_log_days", label: "Activity log", description: "How many days of activity to keep. Zero keeps everything.", control: { kind: "number", min: 0, unit: "days" }, consumer: "live", validate: all(integer, atLeast(0, "days")) },
          { key: "retention_sensor_days", label: "Sensor readings", description: "How many days of sensor readings to keep.", control: { kind: "number", min: 0, unit: "days" }, consumer: "live", validate: all(integer, atLeast(0, "days")) },
          { key: "retention_session_messages_keep", label: "Messages per conversation", description: "How many messages to keep in each conversation.", control: { kind: "number", min: 0, unit: "messages" }, consumer: "live", validate: all(integer, atLeast(0, "messages")) },
          { key: "retention_events_days", label: "Events", description: "How many days of activity to keep, unless a kind below says otherwise.", control: { kind: "number", min: 0, unit: "days" }, consumer: "live", proposed: true, validate: all(integer, atLeast(0, "days")) },
          { key: "retention_events_by_category", label: "Per-category overrides", description: "Keep some kinds of activity longer or shorter than the general rule.", control: { kind: "text", placeholder: "network 14, sensor 7" }, consumer: "live", proposed: true, validate: retentionMap },
          { key: "retention_sensitive_days", label: "Anything sensitive", description: "How long anything sensitive may be kept, whatever the other rules say.", control: { kind: "number", min: 0, unit: "days" }, consumer: "live", proposed: true, validate: all(integer, atLeast(0, "days")) },
        ],
      },
      {
        name: "Cost accounting",
        entries: [
          { key: "cloud_input_price_per_million", label: "Cloud input price", description: "What a cloud service charges to read a million words, to estimate your savings.", control: { kind: "number", step: 0.01, min: 0, unit: "per million tokens" }, consumer: "live", validate: atLeast(0) },
          { key: "cloud_output_price_per_million", label: "Cloud output price", description: "What a cloud service charges to write a million words, to estimate your savings.", control: { kind: "number", step: 0.01, min: 0, unit: "per million tokens" }, consumer: "live", validate: atLeast(0) },
        ],
      },
    ],
  },
];

/** Every entry, flattened. */
export function allEntries(): Entry[] {
  return CATALOGUE.flatMap((c) => c.groups.flatMap((g) => g.entries));
}

/** How many entries in a category nothing reads. Drives the rail's flag. */
export function inertCount(category: CatalogueCategory): number {
  return category.groups
    .flatMap((g) => g.entries)
    .filter((e) => e.consumer === "none").length;
}
