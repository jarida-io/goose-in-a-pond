# Settings — categories, catalogue, and the client contract

**Verified 2026-08-14** against `feat/extensions-redesign` @ `525b8d9b`. Every current-state claim
below was checked by grep against the symbol, not by reading a previous version of this document.
Line numbers rot; symbols do not. Re-verify before trusting.

This document is the shared definition of GIAP's settings surface for **every** client — the
desktop app, Goose On The Go (GOTG), the embedded web UI, and anything else that speaks
`/api/v1/settings`. It exists because the settings surface currently has no shared vocabulary: the
desktop app groups fields one way, the domain struct groups them another, and the API exposes a flat
bag of 122 keys with no grouping at all. A mobile client building a settings screen today has to
re-derive the categories by reading Rust.

---

## 1. The three axes

A setting is not one thing. It is three, and they fail independently:

| Axis | Question | Where it lives |
|---|---|---|
| **Store** | Is the value persisted and readable back? | `sqlite_settings.rs` — `upsert!` + `apply_key` arm |
| **Consumer** | Does anything *act* on the value? | any reader outside the persistence layer |
| **Surface** | Can a person change it, and where? | desktop control, API, or both |

The existing guard test `every_settings_field_is_dispositioned`
([settings.rs:2216](../../crates/pond-core/src/user_data/domain/settings.rs)) covers **Store** and
**Surface**. It classifies all 122 fields into `UI_WIRED` and `HEADLESS_BY_DESIGN` and fails the
build on an unclassified field.

**Nothing covers the Consumer axis.** That is the gap this document names, and it is not
theoretical: ten settings are persisted, classified `UI_WIRED`, rendered as an operable control, and
read by nothing. The test's own comment records that this class of defect has bitten before — *"this
list asserts whether a control EXISTS, and claiming one that does not is how twenty-two switches came
to render without being operable"*. The fix then was to check the surface. The same failure mode is
now live on the other axis.

A setting is **whole** only when all three axes agree. The catalogue in §4 states all three per
field.

---

## 2. Categories

Twelve categories, derived from the desktop hub's existing view structure
(`pond-desktop/src/hub/views/settings/`) and extended to cover the fields that have no hub home yet.
These are the names every client should use. They are a *presentation* taxonomy, deliberately not
the domain struct's section comments — the struct groups by when a field was added, which is not how
a household thinks about its house.

| # | Category | Hub view | Status | What it answers |
|---|---|---|---|---|
| 1 | **Account & Home** | `Account.tsx` | exists | Who lives here, what the home is called, where it is, what time it is |
| 2 | **Prompts & Personality** | `Prompts.tsx` | exists | How the assistant talks |
| 3 | **Models** | `Models.tsx` | exists, no settings bound | Which brains run, and how hard |
| 4 | **Voice** | `Voice.tsx` | exists | Wake word, listening, speaking |
| 5 | **Memory** | `Memory.tsx` | exists | What it remembers and for how long |
| 6 | **Privacy & Security** | `Privacy.tsx` | exists | Mic, cameras, network reach, policy, telemetry |
| 7 | **Extensions & Tools** | `Extensions.tsx` | exists | Which capabilities the model can reach |
| 8 | **Reasoning** | — | **proposed** | Whether and how long it thinks; answer review |
| 9 | **Performance & Context** | — | **proposed** | Compaction, turn budgets, timeouts, prompt caching |
| 10 | **Vision & Cameras** | `Cameras.tsx` | exists, no settings bound | The camera pipeline itself |
| 11 | **Automation & Proactivity** | — | **proposed** | Schedules, quiet hours, speaking unprompted |
| 12 | **Data & Retention** | — | **proposed** | How long anything is kept; cost accounting |

Four hub views hold **no settings fields at all** today and are pure device/local state:
`Appearance.tsx`, `Notifications.tsx`, `Rooms.tsx`, `Logs.tsx`. A client implementing this taxonomy
should not expect settings keys for them.

### 2.1 Category is presentation, not storage

Categories must **not** become a storage concern. The store stays a flat key-value table; the
category is metadata *about* a key. This matters because a field can honestly belong to two
categories (`vision_enabled` is both a camera setting and a privacy setting) and because
re-categorising must never be a migration.

---

## 3. The client contract

### 3.1 Is it editable via the API? — Yes. All 122.

**Every field is writable via `PUT /api/v1/settings`, including all 20 that have no desktop
control.** This is not incidental; it is structural, and it is guaranteed by two properties that
already have guard tests:

1. `every_declared_settings_field_is_serialized` proves every declared field appears in the
   serialized JSON object — no `skip_serializing`, no hidden fields.
2. `update_settings` ([routes.rs:3990](../../crates/pond-api/src/routes.rs)) does not have a
   per-field allowlist. It serialises current settings to a `Value`, blind-inserts every key from the
   request body, and deserialises the result back into `Settings`. Any key that round-trips through
   serde is therefore writable.

**GOTG has strictly more reach than the desktop app.** The 20 headless settings — including
`network_mode` and `security_policy_mode`, the two strongest privacy levers in the system — are
reachable from a phone and from `curl`, and from nowhere in the desktop UI.

### 3.2 Endpoints

| Method | Path | Auth | Semantics |
|---|---|---|---|
| `GET` | `/api/v1/settings` | **Protected** — Bearer token, always | Returns the full 122-key object |
| `PUT` | `/api/v1/settings` | **Public until onboarded**, protected after | Partial patch; returns the full merged object |

`PUT` is `Exposure::UntilOnboarded` ([middleware/mod.rs:267](../../crates/pond-api/src/middleware/mod.rs))
so the onboarding wizard can persist before a token exists. `GET` is **never** public — it was the
route that leaked API keys before PAI-2 P0 moved credentials to `SecretRepository`.

### 3.3 Patch semantics

- **Partial.** Send only the keys you are changing. Omitted keys keep their stored value.
- **Type-strict.** A type-invalid value returns `422`, not a silent no-op. This was a real defect:
  returning `200` with the old settings made the UI flash "Saved" while discarding every edit.
- **Unknown keys are silently ignored.** There is no `deny_unknown_fields`. A typo'd key returns
  `200 OK` and changes nothing. **This is the single biggest hazard for a mobile client** — see
  Fix 3 in §5.
- **Secrets are not here.** `api_key_*` fields were removed from `Settings` (PAI-2 P2) and live
  behind `/api/v1/secrets`, which returns names and existence only. `no_settings_field_is_secret_shaped`
  fails the build if one comes back.

### 3.4 Server-side validation — only four fields

Everything else accepts any type-valid value:

| Field | Rule | Status |
|---|---|---|
| `network_mode` | must be `open` \| `allowlist` \| `offline` | `422` |
| `reasoning_effort` | must be `brief` \| `balanced` \| `thorough` | `422` |
| `agent_backend` | `"pond"` refused — backend quarantined (Q2-05) | `422` |
| `matter_ws_url` | must start `ws://` or `wss://`, only when the patch touches Matter | `422` |

The asymmetry is deliberate for the first two: both parsers fall back to a *safe* value on a bad
string (`open`, `brief`), so a typo reaching the store would silently be no gate / the smallest
think. Refusing at the edge is the only place the user finds out. **Every other enum-valued
setting lacks this and should gain it** — see Fix 4.

### 3.5 Side effects a client must expect

`PUT /settings` is not a pure write. It can:

- **Geocode.** Setting `weather_location_name` without editing coordinates triggers a lookup that
  overwrites `weather_latitude`/`weather_longitude`. The response carries the resolved values.
- **Rebuild the LLM provider.** Changing `chat_provider`/`chat_model`/`llm_*` calls
  `rebuild_llm_provider`.
- **Self-heal.** `agent_backend: "pond"` on an unbuilt binary is rewritten to `"goose"` and persisted.

Clients must therefore **use the response body** as the new state rather than assuming the patch
they sent is what landed.

---

## 4. The catalogue

**Legend — Consumer:** `LIVE` = a non-persistence reader acts on it · `UI-ONLY` = only a desktop
client reads it, the server never does · `NONE` = nothing reads it anywhere.
**Legend — Surface:** `S` = legacy `sections/Settings.tsx` · `H:X` = hub view · `O` = onboarding ·
`D` = Devices · `M` = sections/Memory · `API` = API only.

### 1. Account & Home

| Key | Default | Consumer | Surface |
|---|---|---|---|
| `primary_profile_id` | `null` | LIVE | O (indirect) |
| `user_name` | `"Friend"` | LIVE | S, H:Account, H:Prompts, O |
| `home_name` | `""` | **UI-ONLY** | H:Account |
| `timezone` | `"UTC"` | LIVE | S, H:Account, O |
| `weather_enabled` | `false` | LIVE | S, O |
| `weather_location_name` | `""` | LIVE | S, H:Account, O |
| `weather_latitude` | `0.0` | LIVE | S |
| `weather_longitude` | `0.0` | LIVE | S |

### 2. Prompts & Personality

| Key | Default | Consumer | Surface |
|---|---|---|---|
| `assistant_name` | `"Goose"` | LIVE | S, H:Prompts, O |
| `assistant_personality` | `"friendly and concise"` | LIVE | S, O |
| `prompt_style` | `"balanced"` | LIVE | S, H:Prompts, O |
| `custom_system_prompt` | `null` | LIVE | S |
| `prompt_addendum` | `""` | LIVE | S |

### 3. Models

| Key | Default | Consumer | Surface |
|---|---|---|---|
| `chat_provider` | `""` | LIVE | S |
| `chat_model` | `""` | LIVE | S |
| `llm_provider` | `""` | LIVE | **API** |
| `active_llm_model` | `""` | **write-only mirror** | **API** |
| `llm_max_tokens` | `4096` | LIVE | S |
| `llm_temperature` | `0.7` | LIVE | S |
| `context_window_override` | `0` | LIVE | S |
| `agent_backend` | `"goose"` | LIVE | S |
| `embedding_provider` | `"fastembed"` | LIVE | S, H:Memory |
| `active_embedding_model` | `""` | LIVE | S |

### 4. Voice

| Key | Default | Consumer | Surface |
|---|---|---|---|
| `voice_wake_word` | `"goose"` | LIVE | S, H:Voice, O |
| `voice_wake_word_transcriptions` | `[]` | LIVE | S, H:Voice |
| `voice_kws_energy_threshold` | `0.003` | LIVE | S |
| `voice_kws_post_trigger_silence_ms` | `400` | LIVE | S |
| `voice_kws_cooldown_ms` | `2000` | LIVE | S |
| `voice_whisper_url` | `"http://127.0.0.1:9000"` | LIVE | S |
| `active_whisper_model` | `""` | LIVE | S, H:Voice |
| `voice_recording_duration_secs` | `3` | **UI-ONLY** | S |
| `voice_tts_voice` | `""` | LIVE | S, H:Voice, O |
| `active_tts_model` | `""` | LIVE | S |
| `voice_max_turns` | `8` | LIVE | **API** |

`voice_kws_whisper_url` (was `null`, **NONE**) was removed on 2026-10-15: nothing read it once
wake-word detection moved in process. `voice_recording_duration_secs` is read only by the browser
voice path (`useVoicePipeline`); the desktop app and the pond's own voice loop end on silence.

### 5. Memory

| Key | Default | Consumer | Surface |
|---|---|---|---|
| `agent_memory_inject` | `true` | LIVE | S, M, O |
| `agent_memory_limit` | `5` | LIVE | S |
| `memory_extraction_enabled` | `true` | LIVE | S, M |
| `memory_extraction_max_facts` | `3` | LIVE | S |
| `memory_extraction_interval_secs` | `10` | LIVE | S |
| `memory_cleanup_enabled` | `true` | LIVE | S, M |
| `memory_cleanup_interval_hours` | `6` | LIVE | S |
| `memory_consolidation_enabled` | `true` | LIVE | S, H:Memory, M |
| `memory_consolidation_mode` | `"single"` | LIVE | S |
| `memory_consolidation_interval_hours` | `24` | LIVE | S |
| `memory_consolidation_batch_size` | `50` | LIVE | S |
| `memory_prune_threshold` | `0.05` | LIVE | S |
| `memory_archive_threshold` | `0.15` | LIVE | S |
| `memory_decay_base_half_life_days` | `11.25` | LIVE | **API** |
| `memory_decay_beta` | `0.8` | LIVE | **API** |
| `memory_graph_enabled` | `false` | **NONE** | S |

### 6. Privacy & Security

| Key | Default | Consumer | Surface |
|---|---|---|---|
| `mic_enabled` | `true` | LIVE | H:Privacy |
| `cameras_enabled` | `true` | **NONE** | H:Privacy |
| `network_mode` | `"open"` | LIVE | **API** |
| `security_policy_mode` | `"audit"` | LIVE | **API** |
| `telemetry_enabled` | `true` | LIVE | S, H:Privacy |
| `cloud_fallback_enabled` | `false` | **NONE** | H:Privacy |
| `mesh_enabled` | `false` | LIVE | **API** (Mesh.tsx displays, cannot set) |
| `context_ingest_enabled` | `false` | LIVE | **API** |
| `spoken_pairing_codes` | **`true`** | LIVE | H:Privacy, catalogue |
| `ble_setup_enabled` | **`true`** | LIVE | H:Privacy, catalogue |

### 7. Extensions & Tools

| Key | Default | Consumer | Surface |
|---|---|---|---|
| `ext_memory_enabled` | `true` | LIVE | S |
| `ext_schedule_enabled` | `true` | LIVE | S |
| `ext_weather_enabled` | `true` | LIVE | S |
| `ext_knowledge_enabled` | `true` | LIVE | S |
| `ext_system_enabled` | `true` | LIVE | S |
| `ext_device_enabled` | `true` | LIVE | S |
| `ext_news_enabled` | `true` | LIVE | S |
| `ext_finance_enabled` | `true` | LIVE | S |
| `ext_discovery_enabled` | `true` | LIVE | S |
| `ext_audit_enabled` | `true` | LIVE | S |
| `ext_vision_enabled` | `true` | LIVE | S |
| `ext_sensor_enabled` | `true` | LIVE | S |
| `ext_orchestrator_enabled` | **`false`** | LIVE | S |
| `ext_travel_enabled` | **`false`** | LIVE | S |
| `ext_context_enabled` | **`false`** | LIVE | **API** |
| `tool_selection_mode` | `"all"` | LIVE | S, H:Extensions |
| `tool_model` | `null` | LIVE | S |
| `searxng_url` | `null` | LIVE | **API** |
| `tool_output_compaction` | `true` | **NONE** | S |
| `tool_call_validation` | `true` | **NONE** | S |
| `tool_request_detection` | `true` | **NONE** | S |
| `multi_tool_enabled` | `false` | **NONE** | S |

### 8. Reasoning *(proposed view)*

| Key | Default | Consumer | Surface |
|---|---|---|---|
| `thinking_mode` | `"auto"` | LIVE | S |
| `reasoning_effort` | `"brief"` | LIVE | S |
| `show_thinking` | `false` | LIVE | S |
| `persist_thinking` | `false` | LIVE | S |
| `review_mode` | `"off"` | LIVE | S |
| `review_max_rounds` | `1` | LIVE | S |
| `review_pass_threshold` | `3` | LIVE | S |
| `goal_check_enabled` | `true` | LIVE | **API** |

### 9. Performance & Context *(proposed view)*

| Key | Default | Consumer | Surface |
|---|---|---|---|
| `agent_max_turns` | `50` | LIVE | S |
| `agent_timeout_secs` | `300` | LIVE | S |
| `prefix_cache_prompt` | `true` | LIVE | S |
| `context_monitor_enabled` | `true` | LIVE | S |
| `show_turn_stats` | `false` | **UI-ONLY** | S |
| `agent_goose_mode` | `"auto"` | **NONE** | S |
| `hybrid_compaction_enabled` | `true` | LIVE | **API** |
| `summary_idle_secs` | `120` | LIVE | **API** |
| `resume_compaction_idle_secs` | `1800` | LIVE | **API** |
| `compaction_verbatim_days` | `3` | LIVE | **API** |

### 10. Vision & Cameras

| Key | Default | Consumer | Surface |
|---|---|---|---|
| `vision_enabled` | `false` | LIVE | S |
| `vision_camera_url` | `""` | LIVE | S |
| `vision_camera_id` | `"camera-1"` | LIVE | S |
| `vision_fps` | `2` | LIVE | S |
| `vision_motion_threshold` | `0.05` | LIVE | S |
| `vision_classifier_model` | `""` | LIVE | **API** |
| `matter_enabled` | `false` | LIVE | D |
| `matter_ws_url` | `"ws://127.0.0.1:5580/ws"` | LIVE | D |

### 11. Automation & Proactivity *(proposed view)*

| Key | Default | Consumer | Surface |
|---|---|---|---|
| `schedule_max_concurrent` | `2` | LIVE | S |
| `schedule_max_runs_per_task` | `50` | LIVE | S |
| `schedule_result_notify` | `true` | **NONE** | S |
| `unprompted_speech_enabled` | **`false`** | LIVE | S |
| `unprompted_speech_categories` | `"alert"` | LIVE | S |
| `quiet_hours_start` | `"22:00"` | LIVE | S |
| `quiet_hours_end` | `"07:00"` | LIVE | S |
| `proactive_review_enabled` | **`false`** | LIVE | S |

### 12. Data & Retention *(proposed view)*

| Key | Default | Consumer | Surface |
|---|---|---|---|
| `retention_event_log_days` | `30` | LIVE | S |
| `retention_sensor_days` | `7` | LIVE | S |
| `retention_session_messages_keep` | `500` | LIVE | S |
| `retention_events_days` | `30` | LIVE | **API** |
| `retention_events_by_category` | `{}` | LIVE | **API** |
| `retention_sensitive_days` | `7` | LIVE | **API** |
| `cloud_input_price_per_million` | `2.50` | LIVE | S |
| `cloud_output_price_per_million` | `10.00` | LIVE | S |

---

## 5. Findings and proposed fixes

Ordered by severity. Each states the defect, the evidence, and the fix.

### Fix 1 — `cameras_enabled` is a consent toggle that enforces nothing (**severity: high**)

`mic_enabled` has a real enforcement path: `models/domain/mic_gate.rs`, consulted by
`pond-audio/src/owner.rs`, `pond-audio/src/cpal_device.rs` and the whisper adapter — 20 reader sites.
`cameras_enabled` has **zero** readers outside persistence. It renders in `H:Privacy` directly beside
the mic switch, with the same visual weight, and turning it off stops nothing. `vision_enabled` is
the only thing that actually halts capture.

This is worse than a dead toggle. A privacy control that appears to enforce and does not is a
false assurance about a camera in someone's home.

**Fix:** add `camera_gate` mirroring `mic_gate`, consulted at the vision pipeline start
(`main.rs:3249`) and at every frame-capture entry point. Until that lands, remove the control
from `H:Privacy` — an absent switch is honest, an inert one is not.

### Fix 2 — nine more toggles render without being operable (**severity: medium**)

`cloud_fallback_enabled` (its only mention is a `TODO(cloud-fallback)` comment at `main.rs:1705`),
`voice_kws_whisper_url` (removed 2026-10-15), `agent_goose_mode`, `tool_output_compaction`,
`tool_call_validation`, `tool_request_detection`, `multi_tool_enabled`, `memory_graph_enabled`,
`schedule_result_notify` (the result broadcast is unconditional).

Two of these have doc comments describing behaviour that does not exist —
`voice_kws_whisper_url` claims `WhisperKeywordDetector` sends sliding windows to it, and
`tool_output_compaction` claims it "saves 50-80% of tokens".

**Fix:** for each, either implement the consumer or remove the control and mark the field
`RESERVED` (see Fix 5). Delete the misleading doc comments in the same change. The precedent is
already in the tree: `searxng_url` had its row removed from `Settings.tsx` when `search_web` was
deregistered, with the value left persisted so restoring the tool restores the setting.

### Fix 3 — `PUT /settings` silently accepts unknown keys (**severity: medium, GOTG-facing**)

`Settings` has no `#[serde(deny_unknown_fields)]`, and `update_settings` blind-inserts every key from
the request body before deserialising. A mobile client that sends `camera_enabled` (singular) gets
`200 OK` and a full settings object back, with nothing changed and no indication of that.

This is the defect class that `update_settings` already fixed once for *type*-invalid values — the
comment at [routes.rs:4068](../../crates/pond-api/src/routes.rs) records that returning `200` with
stale settings made the UI "flash Saved while every edit in the form was discarded". Unknown *keys*
are the same failure with the same consequence, still open.

**Fix:** reject unknown keys with `422` and name them. Do it at the merge loop in `update_settings`
rather than via `deny_unknown_fields` on the struct, so the error message can list the offending
keys and so the internal `SettingsRepository` deserialisation path stays lenient for
forward-compatibility with older stored rows.

### Fix 4 — enum validation covers 4 of 11 enum-valued settings (**severity: medium**)

Validated: `network_mode`, `reasoning_effort`, `agent_backend`, `matter_ws_url`.

Unvalidated, each with a documented accepted-value set and a silent fallback:
`thinking_mode` (`auto`/`on`/`off`), `prompt_style` (`balanced`/`concise`/`technical`/`warm`),
`review_mode` (`off`/`on`/`auto`), `memory_consolidation_mode` (`single`/`adversarial`),
`tool_selection_mode` (`all`/`relevant`/`minimal`), `security_policy_mode` (`off`/`audit`/`enforce`),
`embedding_provider` (`fastembed`/`gguf`/`none`).

`security_policy_mode` is the sharp one: it is API-only, so the *only* way to set it is the path with
no validation, and an unrecognised value silently becomes a fallback rather than an error.

**Fix:** promote the existing `NETWORK_MODES`/`REASONING_EFFORTS` pattern to a table-driven check —
one `&[(key, &[allowed])]` const walked by `update_settings` — so adding an enum setting without
validation becomes impossible rather than merely discouraged.

### Fix 5 — the disposition guard cannot see the Consumer axis (**severity: medium, root cause**)

`every_settings_field_is_dispositioned` passes today with all ten dead settings classified
`UI_WIRED`. It is an honour-system list: it asserts the *classification* is complete, never that a
control exists or that a reader exists. It also currently carries **five stale entries** — fields
listed `UI_WIRED` with no control anywhere in `pond-desktop/src`:

| Field | Note |
|---|---|
| `searxng_url` | the test's own comment says the row was removed from `Settings.tsx` |
| `llm_provider` | no control |
| `active_llm_model` | no control |
| `memory_decay_base_half_life_days` | no control |
| `memory_decay_beta` | no control |

The lists reconcile against measurement exactly once those five move:

| | Test claims | Measured | Delta |
|---|---|---|---|
| `UI_WIRED` | 107 | 102 | −5 |
| `HEADLESS_BY_DESIGN` | 15 | 20 | +5 |

`HEADLESS_BY_DESIGN`'s 15 entries are all correct — including `retention_events_by_category`, whose
only appearance in `Settings.tsx` is a comment explaining that map-valued settings must be compared
by value, not a control.

**Fix:** replace the two-list classification with a three-axis one — a single const table of
`(key, category, surface, consumer)` where `surface ∈ {UiWired, ApiOnly}` and
`consumer ∈ {Live, UiOnly, Reserved}`. Then:

- `Reserved` becomes a **deliberate, reviewable** state rather than an accident, and requires the
  control to be absent.
- A cheap grep-based test can assert `UiWired` fields actually appear in `pond-desktop/src` outside
  `types.ts` — which would have caught all five stale entries above.
- The table is the natural source for Fix 6.

### Implemented 2026-08-14 — the catalogue is now a page in the desktop app

Fixes 1, 2, 4 and 7 are addressed in `pond-desktop`:

- **`pond-desktop/src/settings/catalogue.ts`** carries all 122 entries with the
  three axes, and `catalogue.test.ts` fails the build if it drifts from the
  TypeScript `Settings` type.
- **The 20 API-only settings gained controls**, including `network_mode` and
  `security_policy_mode` as radio groups on Privacy (Fix 7). Fifteen of them had
  to be added to the TS `Settings` type first — it named only 107 of the 122.
- **The 10 inert settings render disabled**, each with a sentence saying what
  does not happen when you change it (Fix 2). `cameras_enabled` says the camera
  is stopped by Vision & Cameras, not by it — the honest interim answer until
  the `camera_gate` in Fix 1 exists.
- **Client-side validation** covers the fields the server does not (Fix 4),
  including the four it 422s. See `validation.ts` for why the client has to
  check what the server ignores.

Fixes 1, 3, 5 and 6 remain open, and 5 is the one that keeps the rest honest.

### Fix 6 — expose the catalogue over the API (**severity: low, enables everything else**)

The category taxonomy in §2 is currently prose in this file. A mobile client cannot read prose.

**Fix:** add `GET /api/v1/settings/schema` returning, per key: `category`, `type`, `default`,
`allowed_values` (when enum-valued), `surface`, and `consumer`. Generated from the Fix 5 table so it
cannot drift. GOTG then renders a settings screen that groups correctly, validates client-side
against the same allowed-values the server enforces, and can grey out or hide `Reserved` fields
without hardcoding a list that goes stale.

### Fix 7 — give `network_mode` and `security_policy_mode` a UI (**severity: low, already owed**)

Both are `HEADLESS_BY_DESIGN`, and the test's comment on `network_mode` states the condition for
shipping a control has **already been met**: *"THAT CONDITION IS NOW MET. PAI-2 P6b gated the last
file; `UNGATED_SENDERS` is empty and `MAX_UNGATED` is 0."* It stays headless only because the
`Settings.tsx` + `types.ts` change belongs to whoever owns those files.

These are the two settings that decide whether the pond can talk to the internet. Leaving them
reachable only from `curl` and GOTG, while `cameras_enabled` gets a prominent switch that does
nothing, is the surface allocation exactly backwards.

**Fix:** three-value control (`open` / `allowlist` / `offline`) on the Privacy section, next to a
`security_policy_mode` control. This is the last thing PAI-2 P6 owes.

---

## 6. Invariants

Any change to the settings surface must preserve all of these:

1. **Every field is persisted.** `upsert!` + an `apply_key` arm in `sqlite_settings.rs`.
   Guarded by `roundtrip_persists_every_field`.
2. **Every field is serialized.** No `skip_serializing_if`, no hidden fields.
   Guarded by `every_declared_settings_field_is_serialized`.
3. **No field is credential-shaped.** Credentials live in `SecretRepository`.
   Guarded by `no_settings_field_is_secret_shaped`.
4. **Every field is classified.** Guarded by `every_settings_field_is_dispositioned`.
5. **A control implies a consumer.** *Not currently guarded — this is Fix 5.*
6. **A default change reaching existing installs needs a `DEFAULT_ADOPTIONS` entry plus a
   migration.** A flat key-value store only applies a default when the key is absent, so any install
   that ever saved a snapshot has every key pinned. Guarded by
   `default_adoptions_are_structurally_sound` and
   `every_adoption_entry_states_the_literal_the_adapter_writes`.
7. **Off is the safe direction for any new consent or autonomy toggle,** with its own named
   `default_*` fn rather than a reuse of `default_ext_enabled`. A failed settings read falls back to
   `Settings::default()`, so the default is also the read-failure behaviour.

---

## 7. Counts

| | Count |
|---|---|
| Total settings | 122 |
| Editable via `PUT /api/v1/settings` | **122** |
| Editable in the desktop UI | 102 |
| API-only (no desktop control) | 20 |
| Backend-live | 109 |
| UI-only (no server reader) | 3 |
| **No reader anywhere** | **10** |
| **Dead but rendered as an operable control** | **10** |
