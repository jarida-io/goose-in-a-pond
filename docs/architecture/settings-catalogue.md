# Settings: catalogue and client contract

Checked against the code on 2026-10-23 (branch `docs/current-state-rework`, cut from main
`c451bfa5`). The field tables in section 4 were derived mechanically from the `Settings` struct
and `catalogue.ts` at that commit; when fields change, re-derive them from those two sources
rather than editing rows by hand.

This is the shared definition of GIAP's settings surface for every client: the desktop app, Goose
On The Go (GOTG), the embedded web UI, and anything else that speaks `/api/v1/settings`.

## 1. Where settings live

| What | Where |
|---|---|
| Fields, types, serde defaults | `Settings` and `impl Default for Settings` in `crates/pond-core/src/user_data/domain/settings.rs` |
| Storage | `pond_system.db` table `settings`, one row per field ([database_schema.md](database_schema.md)) |
| Persistence | `SqliteSettingsRepository` in `crates/pond-infra/src/sqlite_settings.rs`: one `upsert!` per field in `update_fields`, one arm per field in `apply_key` |
| Server-side validation | `FIELD_RULES` in `crates/pond-core/src/user_data/domain/settings_validation.rs` |
| Shipped-default changes | `DEFAULT_ADOPTIONS` in `settings.rs`, each paired with a system migration |
| REST | `get_settings` / `update_settings` in `crates/pond-api/src/routes.rs` |
| Per-field presentation (category, label, control, consumer mark, client-side validation) | `CATALOGUE` in `pond-desktop/src/settings/catalogue.ts`, rendered as the Settings page by `SettingsCatalogueView` (`pond-desktop/src/settings/SettingsCatalogue.tsx`) |
| TypeScript mirror | `Settings` interface in `pond-desktop/src/api/types.ts` |

The hub views under `pond-desktop/src/hub/views/settings/` (Account, Prompts, Voice, Memory,
Privacy, Extensions, Connections and others) render a subset of the same keys.

Categories are presentation only. The store stays a flat key/value table and a category is
metadata about a key, so re-categorising is never a migration.

## 2. The three axes

A setting is whole only when all three agree, and they fail independently:

| Axis | Question | Guarded by |
|---|---|---|
| **Store** | Is the value persisted and read back? | `roundtrip_persists_every_field` (`sqlite_settings.rs`) perturbs every serialised field, writes, reads back and compares |
| **Surface** | Can a person change it, and where? | `every_settings_field_is_dispositioned` (`settings.rs`) requires every field in exactly one of `UI_WIRED` or `HEADLESS_BY_DESIGN`; it checks the classification is complete, not that a control exists |
| **Consumer** | Does anything act on the value? | Only the desktop side: each `catalogue.ts` entry carries a hand-checked `consumer` mark (`live`, `app`, `none`), and `catalogue.test.ts` pins the counts. No Rust test covers it |

## 3. Counts

| | Count |
|---|---|
| Fields on `Settings` | **147** |
| `upsert!` calls in `update_fields` | 147 |
| `apply_key` arms | 147 fields, plus five legacy keys ignored on read (`think_provider`, `think_model`, `task_provider`, `task_model`, `matter_enabled`); any other unknown stored key is ignored too |
| `UI_WIRED` / `HEADLESS_BY_DESIGN` | 122 / 25 |
| Entries in the desktop catalogue | 135: all 122 `UI_WIRED` fields plus 13 that Rust classifies headless |
| No desktop control (API only) | 12, all `HEADLESS_BY_DESIGN` |
| Fields with a server-side rule | 23 |
| Catalogue `consumer: "none"` (rendered disabled) | 7 |
| Catalogue `consumer: "app"` (only the desktop reads it) | 3 |
| Fields with no Rust reader outside persistence | 14: the 7 inert and 3 app-only fields, plus `llm_provider`, `searxng_url`, `mesh_settlement_millisats_per_token` and `memory_extraction_first_pass_at` |

"No Rust reader" was established by grepping each key across `crates/` outside `sqlite_settings.rs`,
`domain/settings.rs`, `settings_validation.rs`, the settings mock and test files.

## 4. Catalogue

Categories and order follow `catalogue.ts`; the tier is the rail group the desktop shows them
under (Household, then Workshop). Defaults are `Settings::default()`. Notes flag what is not
plain: a non-`live` consumer, a field Rust classifies `HEADLESS_BY_DESIGN` although the catalogue
gives it a control, and the server-side rule if any.

### Account & Home (`account`, Household, 8)

| Key | Default | Notes |
|---|---|---|
| `user_name` | `"Friend"` |  |
| `primary_profile_id` | `null` | rule: a member's id or empty (picker of members, never free text) |
| `home_name` | `""` | app only: the server never reads it |
| `timezone` | `"UTC"` | rule: IANA zone, canonicalised |
| `weather_location_name` | `""` |  |
| `weather_enabled` | `false` |  |
| `weather_latitude` | `0.0` | rule: -90 to 90 |
| `weather_longitude` | `0.0` | rule: -180 to 180 |

### Prompts & Personality (`prompts`, Household, 5)

| Key | Default | Notes |
|---|---|---|
| `assistant_name` | `"Goose"` |  |
| `assistant_personality` | `"friendly and concise"` |  |
| `prompt_style` | `"balanced"` | rule: one of `balanced`, `concise`, `technical`, `warm` |
| `prompt_addendum` | `""` |  |
| `custom_system_prompt` | `null` |  |

### Voice (`voice`, Household, 18)

| Key | Default | Notes |
|---|---|---|
| `audio_input_device` | `""` |  |
| `audio_output_device` | `""` |  |
| `audio_sound_test` | `true` |  |
| `audio_manage_system` | `true` |  |
| `voice_wake_word` | `"goose"` | rule: 1 to 4 words of letters, at most 40 letters |
| `voice_wake_word_transcriptions` | `[]` |  |
| `voice_kws_energy_threshold` | `0.003` |  |
| `voice_kws_post_trigger_silence_ms` | `400` |  |
| `voice_kws_cooldown_ms` | `2000` |  |
| `voice_recording_duration_secs` | `3` | app only: the server never reads it |
| `voice_whisper_url` | `"http://127.0.0.1:9000"` | rule: `http(s)://` URL with a host, blank allowed |
| `active_whisper_model` | `""` | mirror of `model_role_assignments`; the catalogue links to Models |
| `voice_tts_voice` | `""` |  |
| `voice_tts_speed` | `1.0` |  |
| `voice_tts_quality` | `"q8f16"` | rule: one of `q4`, `q4f16`, `q8`, `q8f16`, `fp16`, `fp32` |
| `voice_thinking_tone_enabled` | `true` |  |
| `active_tts_model` | `""` | mirror of `model_role_assignments`; the catalogue links to Models |
| `voice_max_turns` | `0` | Rust classifies it headless; read through `Settings::effective_max_turns` |

### Models (`models`, Household, 11)

| Key | Default | Notes |
|---|---|---|
| `chat_provider` | `""` |  |
| `chat_model` | `""` |  |
| `llm_provider` | `""` | no Rust reader found; the catalogue marks it live |
| `active_llm_model` | `""` | mirror of `model_role_assignments`; the catalogue links to Models; read only by `pond-server status` |
| `llm_max_tokens` | `4096` | rule: whole number 1 to 1,000,000 |
| `llm_temperature` | `0.7` |  |
| `context_window_override` | `0` |  |
| `agent_backend` | `"goose"` | rule: only `goose`; `pond` is refused as quarantined |
| `picture_support_download_enabled` | `false` |  |
| `embedding_provider` | `"fastembed"` | rule: one of `fastembed`, `gguf`, `none` |
| `active_embedding_model` | `""` | mirror of `model_role_assignments`; the catalogue links to Models |

### Memory (`memory`, Household, 17)

| Key | Default | Notes |
|---|---|---|
| `agent_memory_inject` | `true` |  |
| `agent_memory_limit` | `5` |  |
| `memory_extraction_enabled` | `true` |  |
| `memory_extraction_max_facts` | `3` |  |
| `memory_extraction_interval_secs` | `60` |  |
| `suggestion_generation_enabled` | `true` |  |
| `memory_cleanup_enabled` | `true` |  |
| `memory_cleanup_interval_hours` | `6` |  |
| `memory_prune_threshold` | `0.05` |  |
| `memory_archive_threshold` | `0.15` |  |
| `memory_decay_base_half_life_days` | `11.25` |  |
| `memory_decay_beta` | `0.8` |  |
| `memory_consolidation_enabled` | `true` |  |
| `memory_consolidation_mode` | `"single"` | rule: one of `single`, `adversarial` |
| `memory_consolidation_interval_hours` | `24` |  |
| `memory_consolidation_batch_size` | `50` |  |
| `memory_graph_enabled` | `false` | **inert**: nothing reads it |

### Privacy & Security (`privacy`, Household, 8)

| Key | Default | Notes |
|---|---|---|
| `mic_enabled` | `true` | enforced through `mic_gate`; a save applies it at once |
| `cameras_enabled` | `true` | consent: off stops capture within a few seconds and refuses camera images, streams and enrolment (403 `cameras_disabled`) |
| `network_mode` | `"open"` | Rust classifies it headless; rule: one of `open`, `allowlist`, `offline` |
| `cloud_fallback_enabled` | `false` | **inert**: nothing reads it; only a `TODO(cloud-fallback)` comment in `pond-server` mentions it |
| `mesh_enabled` | `false` | needs a `mesh` build |
| `security_policy_mode` | `"audit"` | Rust classifies it headless; rule: one of `off`, `audit`, `enforce` |
| `telemetry_enabled` | `true` |  |
| `context_ingest_enabled` | `false` | Rust classifies it headless |

### Connections (`connections`, Household, 2)

| Key | Default | Notes |
|---|---|---|
| `ble_setup_enabled` | `true` |  |
| `spoken_pairing_codes` | `true` |  |

### Extensions & Tools (`extensions`, Household, 17)

| Key | Default | Notes |
|---|---|---|
| `ext_memory_enabled` | `true` |  |
| `ext_schedule_enabled` | `true` |  |
| `ext_weather_enabled` | `true` |  |
| `ext_knowledge_enabled` | `true` |  |
| `ext_system_enabled` | `true` |  |
| `ext_device_enabled` | `true` |  |
| `ext_sensor_enabled` | `true` |  |
| `ext_travel_enabled` | `false` |  |
| `ext_orchestrator_enabled` | `false` |  |
| `ext_context_enabled` | `false` | Rust classifies it headless |
| `tool_selection_mode` | `"all"` | rule: one of `all`, `relevant`, `minimal` |
| `tool_model` | `null` |  |
| `searxng_url` | `null` | no Rust reader found (`search_web` does not read it); the catalogue marks it live; rule: `http(s)://` URL with a host, blank allowed |
| `tool_output_compaction` | `true` | **inert**: nothing reads it |
| `tool_call_validation` | `true` | **inert**: nothing reads it |
| `tool_request_detection` | `true` | **inert**: nothing reads it |
| `multi_tool_enabled` | `false` | **inert**: nothing reads it |

### Automations (`automation`, Household, 11)

| Key | Default | Notes |
|---|---|---|
| `schedule_max_concurrent` | `2` |  |
| `schedule_max_runs_per_task` | `50` |  |
| `schedule_result_notify` | `true` | read for each finished run; off, no run reaches the phones (2026-10-27) |
| `system_notices_enabled` | `true` | read for each system notice; off silences all but security notices ([notifications.md](notifications.md)) |
| `unprompted_speech_enabled` | `false` |  |
| `unprompted_speech_categories` | `"alert"` |  |
| `quiet_hours_start` | `"22:00"` | rule: 24-hour `HH:MM`, blank allowed |
| `quiet_hours_end` | `"07:00"` | rule: 24-hour `HH:MM`, blank allowed |
| `proactive_review_enabled` | `false` |  |
| `suggestions_muted` | `[]` |  |
| `session_titling_enabled` | `true` |  |

### Reasoning (`reasoning`, Workshop, 8)

| Key | Default | Notes |
|---|---|---|
| `thinking_mode` | `"auto"` | rule: one of `auto`, `on`, `off` |
| `reasoning_effort` | `"brief"` | rule: one of `brief`, `balanced`, `thorough` |
| `show_thinking` | `false` |  |
| `persist_thinking` | `false` |  |
| `review_mode` | `"off"` | rule: one of `off`, `on`, `auto` |
| `review_max_rounds` | `1` |  |
| `review_pass_threshold` | `3` |  |
| `goal_check_enabled` | `true` | Rust classifies it headless |

### Performance & Context (`performance`, Workshop, 9)

| Key | Default | Notes |
|---|---|---|
| `agent_max_turns` | `50` | rule: whole number 1 to 500 |
| `agent_timeout_secs` | `300` |  |
| `agent_goose_mode` | `"auto"` | **inert**: nothing reads it |
| `prefix_cache_prompt` | `true` |  |
| `hybrid_compaction_enabled` | `true` | Rust classifies it headless |
| `summary_idle_secs` | `120` | Rust classifies it headless |
| `compaction_verbatim_days` | `3` | Rust classifies it headless |
| `context_monitor_enabled` | `true` |  |
| `show_turn_stats` | `false` | app only: the server never reads it |

### Vision & Cameras (`vision`, Workshop, 13)

| Key | Default | Notes |
|---|---|---|
| `vision_enabled` | `false` |  |
| `vision_camera_url` | `""` |  |
| `vision_camera_id` | `"camera-1"` |  |
| `vision_fps` | `2` | clamped 1..30 on save and on read |
| `vision_motion_threshold` | `0.05` | clamped 0.005..0.5 |
| `vision_classifier_model` | `""` | Rust classifies it headless |
| `vision_event_cooldown_secs` | `10` | clamped 1..3600 |
| `vision_classifier_confidence` | `0.5` | clamped 0.05..0.99 |
| `vision_face_recognition_enabled` | `true` |  |
| `vision_face_on_motion` | `true` |  |
| `vision_event_types` | all eight types | stored as a JSON list; empty records every type |
| `matter_ws_url` | `"ws://127.0.0.1:5580/giap"` | rule: `ws://` or `wss://` URL with a host, blank allowed |
| `matter_ble_enabled` | `false` |  |

### Data & Retention (`retention`, Workshop, 8)

| Key | Default | Notes |
|---|---|---|
| `retention_event_log_days` | `30` |  |
| `retention_sensor_days` | `7` |  |
| `retention_session_messages_keep` | `500` |  |
| `retention_events_days` | `30` | Rust classifies it headless |
| `retention_events_by_category` | `{}` | Rust classifies it headless |
| `retention_sensitive_days` | `7` | Rust classifies it headless |
| `cloud_input_price_per_million` | `2.50` |  |
| `cloud_output_price_per_million` | `10.00` |  |

### Not in the desktop catalogue: API only (12)

| Key | Default | Notes |
|---|---|---|
| `vad_backend` | `"silero"` | rule: one of `rms`, `silero` |
| `lightning_enabled` | `false` | needs a `lightning` build |
| `mesh_settlement_millisats_per_token` | `0` | vestigial: nothing reads it; the live rate is the constant `MESH_SETTLEMENT_MILLISATS_PER_TOKEN` |
| `mesh_lend_token_ceiling` | `0` | `0` = no ceiling |
| `memory_extraction_sessions_per_pass` | `3` |  |
| `memory_extraction_window_messages` | `20` |  |
| `memory_extraction_idle_secs` | `900` |  |
| `memory_reinforce_threshold` | `0.94` | unmeasured proposal |
| `memory_relate_threshold` | `0.78` | unmeasured proposal |
| `memory_date_proposals_enabled` | `true` |  |
| `memory_extraction_mode` | `"write"` | `shadow`, `write` or `reinforce`; an unrecognised value reads as `shadow` |
| `memory_extraction_first_pass_at` | `""` | nothing reads or writes it outside persistence |

## 5. The client contract

### 5.1 Endpoints

| Method | Path | Auth | Semantics |
|---|---|---|---|
| `GET` | `/api/v1/settings` | Always authenticated (`Exposure::Authenticated`) | The full `Settings` object, every field |
| `PUT` | `/api/v1/settings` | `Exposure::UntilOnboarded`: tokenless only from loopback while the pond is neither claimed nor onboarded, the claimant's token while claimed, a bearer token once onboarded | Partial patch; returns the full merged object |

The exposure table is `PUBLIC_ROUTES` in `crates/pond-api/src/middleware/mod.rs`. `GET` is never
tokenless because it serialises the whole struct. There is no settings schema endpoint; clients
learn categories and allowed values from this document or `catalogue.ts`.

### 5.2 Patch semantics

- **Partial.** Send only the keys being changed. The handler merges the patch onto the stored
  settings but writes only the patch's keys (plus geocoded coordinates, below) through
  `update_fields(.., Some(keys))`, so a concurrent writer's change to another key is not reverted.
- **Validated, all failures at once.** `validate_patch` checks every patch key that has a rule and
  answers `422` with `{"error": <one sentence>, "fields": [{"field", "message"}, ...]}` naming each
  failing field. Valid values are canonicalised before storing: enum values are lower-cased, a time
  zone takes its catalogue spelling, `9:05` becomes `09:05`, a wake phrase gets single spaces.
- **Type-strict.** A value that does not deserialise into the field's type returns `422`
  (`Invalid settings value: ...`). A body that is not JSON returns `400`.
- **Unknown keys are accepted and ignored.** `Settings` has no `deny_unknown_fields`, and
  `update_fields` writes only known keys, so a misspelt key returns `200` with nothing changed.
  This is pinned as intended by `settings_response_never_carries_a_secret_shaped_key`
  (`crates/pond-api/tests/settings_integration_test.rs`): an older client sending a removed field
  must not fail its whole save.
- **User intent is recorded.** Every key in the patch is marked `is_user_set`
  (`mark_user_set`), which exempts it from future `DEFAULT_ADOPTIONS`.
- **Use the response body.** It is the merged state after side effects. `f32` fields echo widened
  (`0.7` reads back as `0.699999988079071`), so diff against what was sent, not for equality.
- **No secrets.** API keys and tokens live in `SecretRepository` behind `/api/v1/secrets`, which
  returns names and existence only. `no_settings_field_is_secret_shaped` fails the build if a
  credential-shaped field is added.

### 5.3 Server-side rules

`FIELD_RULES` holds 23 rules; a field without one accepts any type-valid value.

| Kind | Fields |
|---|---|
| Closed vocabulary (`Check::OneOf`) | `network_mode`, `reasoning_effort`, `security_policy_mode`, `tool_selection_mode`, `agent_backend`, `prompt_style`, `thinking_mode`, `review_mode`, `memory_consolidation_mode`, `embedding_provider`, `voice_tts_quality`, `vad_backend` |
| IANA time zone | `timezone` |
| 24-hour clock time | `quiet_hours_start`, `quiet_hours_end` |
| URL with scheme and host | `matter_ws_url` (`ws`/`wss`), `searxng_url`, `voice_whisper_url` (`http`/`https`) |
| Number in range | `weather_latitude`, `weather_longitude`, `agent_max_turns` (1 to 500), `llm_max_tokens` (1 to 1,000,000) |
| Wake phrase | `voice_wake_word` |

Beyond the table, `update_settings` refuses `agent_backend: "pond"` by name, and refuses an empty or
non-WebSocket `matter_ws_url` whenever the patch touches `matter_ws_url` or `matter_ble_enabled`.

Only `PUT /api/v1/settings` applies these rules. Other writers go straight to the repository:
whole-struct `SettingsRepository::update` (the wake-word calibration routes, the `chat`,
`calibrate` and `onboard` subcommands, the startup `agent_backend` repair) and `set_key` callers
(`pond-connect`'s BLE advertising switch, the device MCP tool's audio pins, `pond-server`'s TTS
voice repair). `check_raw_key` exists for key/value writers but nothing calls it.

### 5.4 Side effects of a save

| Trigger | Effect |
|---|---|
| Every save | Vision supervisor woken: it re-derives the vision bus from the stored settings, and restarts or stops capture if a capture setting or consent changed (it also polls every 2 s, for writers in other processes); microphone gate set from `mic_enabled` (`mic_gate::set_mic_enabled`); vision download allow-list set or cleared from `picture_support_download_enabled`; `network_mode` re-installed into the egress gate |
| `audio_input_device`, `audio_output_device` or `mic_enabled` changed | Speaker and microphone reselected in the background |
| `weather_location_name` (changed, or coordinates unset) without hand-edited coordinates | Name geocoded; `weather_latitude`/`weather_longitude` overwritten and saved (best effort) |
| `matter_ws_url`, `matter_ble_enabled` | Matter controller reconfigured |
| `mesh_enabled` set true | Mesh transport rebuilt, then the LLM provider |
| `chat_provider`, `chat_model`, `tool_model`, `active_whisper_model`, `active_tts_model` | LLM provider rebuilt; `chat`, `asr` and `tts` roles mirrored into `model_role_assignments` |
| `chat_provider` or `chat_model` (changed) | Prompt-prefix prewarm restarted in the background |

At `serve` startup a stored `agent_backend` of `pond` is rewritten to `goose` and persisted.

## 6. Findings and their status

Status against the code at the date above.

| # | Finding | Status | Evidence |
|---|---|---|---|
| 1 | `cameras_enabled` is a consent switch that enforces nothing; add a `camera_gate` like `mic_gate` | **Done** | The vision supervisor (`pond-adapters-vision/src/supervisor.rs`) captures nothing while it is off and stops a running pipeline within a poll; camera snapshot, stream, event-snapshot, SSE and enrol-from-camera routes answer 403 `cameras_disabled`; the `giap-vision` camera tools refuse. Enforced from the stored setting rather than a process gate, because the voice child writes settings from another process |
| 2 | Inert toggles render as operable controls | **Partial** | The catalogue renders all 7 `none` fields disabled, each with a note (`SettingsCatalogue.tsx`, `inert`). `voice_kws_whisper_url` is gone from `Settings`. Still open: the fields persist with no reader; `Privacy.tsx` keeps an operable `cloud_fallback_enabled` switch; Rust doc comments still describe behaviour that does not exist (`tool_output_compaction` "50-80% fewer tokens", `multi_tool_enabled` names a `ToolAgent` that is not in the tree, `tool_request_detection`, `tool_call_validation`). The catalogue also marks `llm_provider` and `searxng_url` `live`, though no Rust code reads either |
| 3 | `PUT /settings` silently accepts unknown keys | **Not started** | Behaviour unchanged (section 5.2) and now pinned as intended by `settings_response_never_carries_a_secret_shaped_key` |
| 4 | Enum validation covered 4 of 11 enum-valued settings | **Done** | `FIELD_RULES` is the table-driven check, walked by `validate_patch`; it covers all 12 closed vocabularies and 11 more fields. `every_ruled_key_is_a_real_settings_field` and `the_shipped_defaults_pass_every_rule` guard it. Nothing forces a new enum field into the table |
| 5 | The disposition guard cannot see the Consumer axis; replace the two lists with a `(key, category, surface, consumer)` table | **Partial** | The table exists on the desktop side only (`catalogue.ts` with `consumer`, `catalogue.test.ts` pinning 8 `none` and 3 `app`). Rust still has the two lists, and they have drifted: 13 `HEADLESS_BY_DESIGN` fields now have catalogue controls (`network_mode`, `security_policy_mode`, `goal_check_enabled`, the compaction and `retention_events*` knobs, `voice_max_turns`, `vision_classifier_model`, `context_ingest_enabled`, `ext_context_enabled`), and the list's comments still say controls are owed. Nothing checks that a Rust field reaches `types.ts` |
| 6 | Expose the catalogue as `GET /api/v1/settings/schema` | **Not started** | `routes.rs` registers only `/settings` (GET, PUT) |
| 7 | Give `network_mode` and `security_policy_mode` a UI | **Done** | Both are radio controls in the catalogue's Privacy & Security category (`catalogue.ts`, `proposed: true`). Rust still classifies them headless (finding 5) |

## 7. Invariants

Any change to the settings surface must keep these:

1. **Every field is persisted:** an `upsert!` in `update_fields` and an arm in `apply_key`.
   Guarded by `roundtrip_persists_every_field`.
2. **Every field is serialised:** no `skip_serializing`, `skip_serializing_if` or `rename`.
   Guarded by `every_declared_settings_field_is_serialized`, which parses the struct source.
3. **No field is credential-shaped.** Guarded by `no_settings_field_is_secret_shaped`; the three
   exemptions are listed in `NOT_ACTUALLY_SECRET`.
4. **Every field is classified** `UI_WIRED` or `HEADLESS_BY_DESIGN`. Guarded by
   `every_settings_field_is_dispositioned`. On the desktop, every `types.ts` field needs a
   catalogue entry (`catalogue.test.ts`).
5. **A control implies a consumer.** Guarded only on the desktop: a `consumer` mark per entry and
   the counts in `catalogue.test.ts`. Not guarded in Rust (finding 5).
6. **A changed default that must reach existing installs needs a `DEFAULT_ADOPTIONS` entry and a
   migration.** The store applies a default only when a key is absent, and an install that ever
   saved has every key pinned; the migration rewrites a row only where it still holds the old
   default and `is_user_set = 0`. Guarded by `default_adoptions_are_structurally_sound`,
   `every_adoption_entry_has_matching_migration_sql` and
   `every_adoption_entry_states_the_literal_the_adapter_writes`. Current entries:

   | Key | Old | New | Migration |
   |---|---|---|---|
   | `agent_max_turns` | `20` | `50` | 0035 |
   | `hybrid_compaction_enabled` | `false` | `true` | 0035 |
   | `vad_backend` | `rms` | `silero` | 0052 |
   | `voice_max_turns` | `8` | `0` | 0053 |

7. **Off is the safe default for any consent or autonomy toggle,** with its own named `default_*`
   fn rather than a shared one. A failed settings read falls back to `Settings::default()`, so the
   default is also the failure behaviour. Guarded by
   `only_the_deliberate_extension_toggles_ship_switched_off` (only `ext_context_enabled`,
   `ext_orchestrator_enabled` and `ext_travel_enabled` ship off) and the
   `*_toggle_defaults_off_by_its_own_route` tests.
