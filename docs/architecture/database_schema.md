# Database schema

GIAP keeps its state in three SQLite files under the data directory. `Database::init`
(`crates/pond-infra/src/db.rs`) opens and migrates all three with `sqlx::migrate!`, in this order:

| Pool | File | Migrations | Versions | Tables | Holds |
|---|---|---|---|---|---|
| `db.system` | `pond_system.db` | `crates/pond-infra/migrations/system/` | 0001–0060 (0024 unused, 59 files) | 38 | Authoritative household data: conversations, members, devices, memory, settings, pairing |
| `db.logs` | `pond_logs.db` | `crates/pond-infra/migrations/logs/` | 0001–0010 | 6 | High-volume, prunable data: operational log, audit events, sensors, camera events, turn metrics |
| `db.vectors` | `pond_vectors.db` | `crates/pond-infra/migrations/vectors/` | 0001–0002 | 1 | Derived embedding index, rebuildable from `pond_system.db` |

Every connection runs with `foreign_keys` ON, WAL journaling, `synchronous=NORMAL`, an 8 MB page
cache and a 32 MB mmap, four connections per pool (`Database::connect`). Vector connections also
`ATTACH` `pond_system.db` as `sys` so retrieval can join live source rows
(`SqliteVectorIndex::connect`), which is why the vector migrations run after the system ones.

Queries are runtime `sqlx::query`/`query_as` calls; there are no compile-time `query!` macros and no
`.sqlx/` directory. Migration versions must be unique per database
(`migration_versions_are_unique_within_each_database` in `db.rs`).

Not in these files:

- **Goose's own state.** Goose keeps its conversation store (`sessions.db`) under
  `<data_dir>/engine/`, which `pin_goose_state_under` (`crates/pond-server/src/main.rs`) sets as
  `GOOSE_PATH_ROOT`. The GIAP REST API never reads it; chat history is `sessions` /
  `session_messages` below, and `engine_session_map` links the two.
- **Credentials.** `FileSecretRepository` keeps them encrypted in `<data_dir>/secrets.json`
  (`crates/pond-infra/src/file_secret_repository.rs`). `secret_migration.rs` moves any legacy
  `api_key_*` rows out of `settings`.

Cross-database references (`sensor_readings.device_id`, `events.session_id`, `vectors.row_id`)
are logical only: SQLite cannot enforce a foreign key across files.

---

## `pond_system.db`

FK = enforced foreign key. Key columns, not full column lists; read the migration or
`sqlite3 pond_system.db .schema <table>` for the rest.

### Conversations

| Table | Purpose | Key columns |
|---|---|---|
| `sessions` | One row per conversation | `title`, `title_source`, `title_through_message_id`; `profile_id` (FK `profiles`), `identification_source`, `identification_confidence`; `surface` (where it began: `phone`, `desktop`, `cli`, `pond`, `room`, `schedule`, `goose`; first write wins), `surface_device_id`; `input_mode`; `total_prompt_tokens`, `total_completion_tokens`, `model_name`; `rolling_summary`, `rolling_summary_through_id`, `rolling_summary_updated_at`; batch-extraction cursor `extracted_through_id`, `extracted_at`, `extraction_attempts` |
| `session_messages` | Messages in a session | `session_id` (FK, cascade), `role` (`user`/`assistant`/`system`/`tool`), `content`, `tool_call_id`, `tool_calls_json`, `prompt_tokens`, `completion_tokens`, `reasoning_tokens`, `liked`, `embedding`, `note_json` (a `MessageNote`: what produced the message, the turn's receipts, an offer) |
| `session_thinking` | Reasoning text kept beside its message when `persist_thinking` is on | `message_id`, `session_id` (both FK, cascade), `block_index`, `content` |
| `message_attachments` | Metadata for chat image attachments; the bytes live under `<data_dir>/attachments/<session_id>/` | `message_id`, `session_id` (FK, cascade), `ordinal`, `mime_type`, `byte_size`, `file_path` |
| `engine_session_map` | GIAP session id to the Goose engine's own session id, so a restart resumes the same engine conversation | `session_id` (PK), `engine_session_id` |
| `session_tool_groups` | Tool groups (MCP extension names) a session holds when `tool_selection_mode` narrows; only ever grows (a later message or `enable_tool_group` adds) | `session_id` (PK), `groups` |

### Household, devices and pairing

| Table | Purpose | Key columns |
|---|---|---|
| `onboarding_state` | Singleton (`CHECK(id=1)`) holding the wizard's current `OnboardingStep` | `current_step` |
| `profiles` | Household members | `display_name`, `avatar_emoji`, `preferences` (JSON). Deleting a profile releases its sessions and expires its drafts and reminders (triggers `trg_profiles_delete_*`) |
| `devices` | Paired clients and registered devices | `name`, `device_type`, `hostname`, `ip_address`, `capabilities` (JSON), `last_seen`, `room`, `profile_id` (FK, set null), `is_test` (a harness's device: no household notices, left out of the reliability report). `is_online` is stored but never read: `SqliteDeviceRegistry` derives it from `last_seen` within 300 s |
| `pairing_codes` | Single-use pairing codes; only the SHA-256 hash is stored | `code_hash` (PK), `expires_at`, `consumed_at`, `profile_id` (FK, set null), `approved` (its phone passes the pairing lock), `is_test` |
| `pairing_lock` | Singleton (`CHECK(id=1)`): whether pairing is locked, the main member, and the recovery PIN as a PBKDF2 verifier with its failed tries | `locked`, `owner_profile_id`, `pin_verifier`, `pin_failures`, `pin_locked_until` |
| `pairing_requests` | Phones the lock held, waiting ten minutes for a yes; the claim secret is stored hashed | `client_id`, `client_type`, `device_name`, `profile_id`, `claim_hash`, `status` (`pending`, `approved`, `declined`, `claimed`), `expires_at` |
| `handshake_challenges` | Short-lived per-handshake challenges | `client_id`, `challenge`, `client_type`, `expires_at`, `consumed_at` |
| `session_tokens` | Issued bearer and refresh tokens, hashes only | `token_hash` (PK), `refresh_hash`, `device_id`, `client_id`, `client_type`, `expires_at`, `refresh_expires_at`, `last_seen_at`, `revoked_at` |
| `push_tokens` | One FCM/APNs token per device | `device_id` (PK, FK cascade), `token`, `platform` |
| `notifications` | Offline queue for targeted notifications, and the record of what became of each; broadcasts are not queued | `device_id` (FK cascade), `category`, `title`, `body`, `data`, `delivered_at`; `held` (waiting for quiet hours to end); `wakes`, `woken_at`, `fetched_at`, `shown_at`, `opened_at`, `spoken_at` |
| `face_embeddings` | Per-member face vectors | `profile_id` (FK cascade), `embedding`, `model_dims`, `model_name` |
| `face_profile_thresholds` | Per-member face match threshold | `profile_id` (PK, FK cascade), `match_threshold` |

### Memory, proposals and personal context

| Table | Purpose | Key columns |
|---|---|---|
| `memory_fragments` | Curated long-term memory | `profile_id` (FK cascade; null = household-wide), `session_id` (FK set null), `content`, `source`, `tags` (JSON), `segment`, `importance`, `tier`, `decay_rate`, `access_count`, `last_accessed_at`, `lifecycle`, `superseded_by`, `corrects`, `about` (what it says of its member, from the closed list), `valid_until` (a passing fact's end), `resort_attempts`, `embedding` |
| `memory_events` | Audit trail of memory lifecycle events | `event_kind`, `memory_id`, `session_id`, `data` |
| `consolidation_runs` | One row per memory consolidation pass | `mode`, `memory_count`, `accepted`, `rejected`, `duration_ms`, `details` |
| `memory_edges` | Edges between memories (`from_id`, `to_id`, `relation`). No code reads or writes it, and its foreign keys name a `memories` table that does not exist | |
| `suggestion_queue` | Suggestions the pond composed for Home | `profile_id`, `prompt`, `reason`, `source_memory_id`, `answered_by`, `state` (`queued`/`taken`/`dismissed`) |
| `reminders` | Dated utterances held for proposal instead of becoming memories | `about`, `when_said`, `about_key`, `session_id`, `window_id`, `subject`, `profile_id`, `said_at`, `disposition` (`pending`/`proposed`/`dismissed`/`expired`), `decided_at` |
| `drafts` | Staged actions awaiting approval, and proactive proposals (`origin = 'proactive'`) | `session_id`, `kind`, `summary`, `payload` (JSON), `status`, `profile_id`, `identification_source`, `origin`, `expires_at`, `rationale`. Triggers require a rationale and an expiry on proposals and refuse approving an expired row |
| `context_sources` | Per-member sources of personal context (accounts, sensors, devices) | `kind`, `provider`, `profile_id` (FK cascade), `scopes`, `cursor`, `last_sync`, `status`, `secret_ref`. Owner and kind are immutable (triggers) |
| `context_items` | The personal-context corpus, one row per item a source produced | `source_id` (FK cascade), `external_id`, `profile_id` (must match the source's, trigger), `source_kind`, `item_kind`, `occurred_at`, `title`, `body`, `participants`, `sensitivity`, `embedding` |

### Configuration, models and prompts

| Table | Purpose | Key columns |
|---|---|---|
| `settings` | Flat key/value store behind `Settings`; see [settings-catalogue.md](settings-catalogue.md) | `key` (PK), `value`, `updated_at`, `is_user_set` (set by a `PUT /api/v1/settings` patch; exempts the row from `DEFAULT_ADOPTIONS`) |
| `models` | Model catalogue | `category`, `name`, `filename`, `url`, `hf_id`, `size_mb`, `ram_estimate_mb`, `recommended_role`, `context_length`, `quantization`, ASR and TTS fields, `downloaded`, `is_custom` |
| `model_role_assignments` | Which model fills each role (`chat`, `asr`, `tts`, ...) | `role` (PK), `model_id` |
| `prompt_templates` | System-prompt templates; built-ins are reseeded at boot unless customised | `name` (PK), `content`, `description`, `is_system`, `is_customized`, `factory_version` |
| `prompt_extras` | Keyed extra instructions added to the system prompt | `key` (PK), `instruction`, `active`, `sort_order` |
| `user_skills` | Household-written skills | `name` (unique), `content`, `description`, `icon`, `active` |
| `agent_recipes` | Goose recipe YAML | `name` (unique), `description`, `yaml`, `active` |
| `mcp_servers` | User-added MCP servers | `name`, `kind` (`stdio`/`streamable_http`), `command`, `args`, `env`, `uri`, `enabled` |
| `lane_job_runs` | When each inference-lane job last ran, so a restart does not reset the cadence | `job` (PK), `last_run_at` |
| `rides` | Ride offers and bookings per member, kept across restarts | `profile_id` (FK cascade), `saved` (JSON), `updated_at` |

### Private mesh

| Table | Purpose | Key columns |
|---|---|---|
| `mesh_trusted_peers` | Peers this pond trusts | `peer_id` (PK), `trust_scope`, `last_known_address` |
| `mesh_credit_balances` | Credit per peer, in millisats | `peer_id` (PK), `balance_millisats` |
| `mesh_usage_tally` | Tokens lent to and borrowed from each peer | `peer_id` (PK), `tokens_borrowed`, `tokens_lent` |

The `settings` table also carries raw keys that are not `Settings` fields, written through
`SettingsRepository::set_key`: `mesh_identity_secret` (mesh identity keypair) and
`lightning_wallet_mnemonic` (`lightning` builds only). `apply_key` ignores them on read, so
`GET /api/v1/settings` never returns them.

---

## `pond_logs.db`

Never write sensor, camera or telemetry rows to `db.system`; they live here.

| Table | Purpose | Key columns |
|---|---|---|
| `event_log` | Operational log written by `pond-server`'s tracing drain; read by the log viewer. `serve --debug` tails it to stdout (`tail_event_log`) | `timestamp`, `level`, `source`, `message`, `metadata` |
| `events` | Unified activity and audit events (`EventLog`) | `timestamp`, `category`, `action`, `session_id`, `trace_id`, `attributes`, `privacy_sensitivity` |
| `sensor_readings` | Sensor time series | `device_id` (logical ref to `devices.id`), `sensor_type`, `value`, `unit`, `created_at` |
| `camera_events` | Vision detections | `camera_id`, `event_type`, `confidence`, `snapshot_path`, `metadata`, `acknowledged` |
| `turn_metrics` | Per-turn performance telemetry, recorded when `telemetry_enabled` | `session_id`, `turn_number`, token counts, `ttft_ms`, `prefill_ms`, `total_latency_ms`, `decode_tok_per_sec`, `prefill_tok_per_sec`, `prefilled_tokens`, `reused_prefix_tokens`, `context_utilization_pct`, `context_limit_tokens`, tool name and latency, `model_name` |
| `system_info` | Key/value snapshots. Created by migration 0001; no code reads or writes it | `key`, `value` |

---

## `pond_vectors.db`

| Table | Purpose | Key columns |
|---|---|---|
| `vectors` | Embeddings for three corpora: `memory` (`memory_fragments`), `context` (`context_items`) and `summary` (`sessions.rolling_summary`) | PK `(corpus, row_id, chunk_ix)`; `chunk_start`, `chunk_len` (span into the source text, null = whole text), `model_id`, `dims`, `vector` (little-endian f32), `source_rev`, `embedded_at` |

The table deliberately has no text column and no foreign keys. A source row and its vector cannot
be deleted atomically across files under WAL, so orphans are expected; an orphan holds only an
offset, resolves to nothing on the join, and `VectorIndex::prune_orphans` (called by the embedding
slot, `crates/pond-core/src/context/embedding_slot.rs`, once its backlog drains) removes it.
Retrieval filters on `model_id`, so vectors from a different embedder are never scored together.
Deleting the file is safe: the slot re-embeds every row from its source.

---

## Ports and adapters

Ports are traits in `crates/pond-core/src/`; adapters are in `crates/pond-infra/src/`. Apart from
`serve --debug`'s `event_log` tail and the context-index rebuild route (`rebuild_context_index` in
`crates/pond-api/src/routes.rs`, which clears `vectors`), no SQL runs outside `pond-infra`.

| Port (path under `pond-core/src/`) | Adapter | Tables |
|---|---|---|
| `OnboardingRepository` (`user_data/ports/onboarding.rs`) | `SqlxOnboardingRepository` (`onboarding.rs`) | `onboarding_state` |
| `SessionStorage` (`user_data/ports/session_storage.rs`) | `SqliteSessionStorage` | `sessions`, `session_messages`, `session_thinking`, `message_attachments`, `engine_session_map`, `session_tool_groups` |
| `SettingsRepository` (`user_data/ports/settings.rs`) | `SqliteSettingsRepository` | `settings` |
| `ProfileRepository` (`user_data/ports/profile.rs`) | `SqliteProfileRepository` | `profiles` |
| `DeviceRegistry` (`user_data/ports/device_registry.rs`) | `SqliteDeviceRegistry` | `devices` |
| `DeviceAttribution` (`user_data/ports/device_attribution.rs`) | `SqliteDeviceAttribution` | `devices`, `push_tokens` |
| `Handshake` (`security/ports/handshake.rs`) | `SqliteHandshakeAdapter` | `pairing_codes`, `handshake_challenges`, `session_tokens`, `devices`, `profiles` |
| `PushTokenRepository` (`user_data/ports/push_token.rs`) | `SqlitePushTokenRepository` | `push_tokens` |
| `NotificationQueueRepository` (`mcp/ports/notification_queue.rs`) | `SqliteNotificationQueue` | `notifications` |
| `FaceRecognition` (`user_data/ports/face_recognition.rs`) | `SqliteFaceRecognition` | `face_embeddings`, `face_profile_thresholds` |
| `MemoryRepository` (`user_data/ports/memory_repository.rs`) | `SqliteMemoryRepository` | `memory_fragments`, `memory_events`, `consolidation_runs` |
| `SuggestionQueueRepository` (`user_data/ports/suggestion_queue.rs`) | `SqliteSuggestionQueue` | `suggestion_queue` (reads `memory_fragments`) |
| `ReminderRepository` (`user_data/ports/reminder_repository.rs`) | `SqliteReminderRepository` | `reminders` |
| `DraftRepository` (`user_data/ports/draft.rs`) | `SqliteDraftRepository` | `drafts` |
| `ProposalRepository` (`user_data/ports/proposal.rs`) | `SqliteProposalRepository` | `drafts` (rows with `origin = 'proactive'`) |
| `ContextRepository` (`context/ports.rs`) | `SqliteContextRepository` | `context_sources`, `context_items` |
| `VectorIndex` (`context/vector_index.rs`) | `SqliteVectorIndex` | `vectors` (joins `sys.*` sources) |
| `ModelRepository` (`models/ports/model_repository.rs`) | `SqliteModelRepository` | `models`, `model_role_assignments` |
| `PromptTemplateRepository` (`user_data/ports/prompt_template.rs`) | `SqlitePromptTemplateRepository` | `prompt_templates` |
| `PromptExtraRepository` (`user_data/ports/prompt_extra.rs`) | `SqlitePromptExtraRepository` | `prompt_extras` |
| `UserSkillRepository` (`user_data/ports/skill.rs`) | `SqliteSkillRepository` | `user_skills` |
| `AgentRecipeRepository` (`user_data/ports/recipe.rs`) | `SqliteRecipeRepository` | `agent_recipes` |
| `McpServerRepository` (`mcp/ports/mcp_server.rs`) | `SqliteMcpServerRepository` | `mcp_servers` |
| `LaneRunLog` (`user_data/ports/lane_run_log.rs`) | `SqliteLaneRunLog` | `lane_job_runs` |
| `RideStore` (`rides/ports.rs`) | `SqliteRideStore` | `rides` |
| `PeerDirectory` (`mesh/ports/peer_directory.rs`) | `SqlitePeerDirectory` | `mesh_trusted_peers` |
| `CreditLedger` (`mesh/ports/credit_ledger.rs`) | `SqliteCreditLedger` | `mesh_credit_balances` |
| `UsageTally` (`mesh/ports/usage_tally.rs`) | `SqliteUsageTally` | `mesh_usage_tally` |
| `OperationalLogRepository` (`security/ports/event_log.rs`) | `SqliteOperationalLog` (`sqlite_event_log.rs`) | `event_log` |
| `EventLog` (`security/ports/event_log.rs`) | `SqliteEventLog` | `events` |
| `SecurityPolicy` (`security/ports/policy.rs`) | `SqliteSecurityPolicy` | none of its own; audits through `EventLog` into `events` |
| `SensorStorage` / `CameraStorage` (`user_data/ports/sensor_storage.rs`, `camera_storage.rs`) | `SqliteSensorStorage` / `SqliteCameraStorage` (`sqlite_sensor.rs`) | `sensor_readings` / `camera_events` |
| `TelemetryPort` (`security/ports/telemetry.rs`) | `SqliteTelemetry` | `turn_metrics` |

---

## Retention

`pond_infra::pruning::run_pruning`, spawned once by `pond-server`, runs `prune_once` every
6 hours. The first pass is 6 hours after start (it never prunes at startup), and each pass re-reads
`Settings`, so a retention change applies on the next pass.

| Table | Rule | Setting (default) |
|---|---|---|
| `event_log` | Delete rows older than N days | `retention_event_log_days` (30) |
| `sensor_readings` | Delete rows older than N days | `retention_sensor_days` (7) |
| `camera_events` | Delete **acknowledged** rows older than 14 days; unacknowledged rows are kept | none; fixed in `PruningConfig` |
| `events` | Delete rows at `Sensitive` or above older than the cap, then each category past its retention; `0` keeps that category forever | `retention_sensitive_days` (7, `0` = no cap), `retention_events_days` (30), `retention_events_by_category` (per-category override) |
| `session_messages` | Keep the newest N messages per session, delete the rest | `retention_session_messages_keep` (500) |
| `face_embeddings` | Delete rows whose profile no longer exists | none |

For `event_log`, `sensor_readings` and `session_messages`, `0` is not special-cased: it deletes
every row (for the first two, every row older than the pass). Only the `events` rules treat `0` as
"forever".

Nothing in `pruning.rs` or any adapter deletes old rows from `turn_metrics`, `memory_events`,
`consolidation_runs`, `notifications`, `handshake_challenges` or `session_tokens`. Memory decay
and archiving (`memory_cleanup_enabled`) and vector orphan pruning run outside this module.

---

## Conventions

- **Two plus one files.** Logs and sensor data churn on a days-long TTL without touching
  household data, and high-frequency writes do not contend with system queries. Vectors are a
  third file so derived data never travels with the authoritative database.
- **TEXT timestamps.** Most columns default to `datetime('now')` (`YYYY-MM-DD HH:MM:SS`, UTC).
  Some adapter-written columns are RFC 3339, for example `drafts.expires_at`, whose format the
  triggers compare. Where rows can share a second, sort by `rowid` as a tiebreak.
- **JSON in TEXT** for sparse, unqueried shapes (`devices.capabilities`, `profiles.preferences`,
  `memory_fragments.tags`, `drafts.payload`). Normalise a field once it is queried with
  `json_extract()`.
- **Settings need no migration.** A new `Settings` field is a new row; it needs an `upsert!` and an
  `apply_key` arm in `sqlite_settings.rs`, not a schema change. Changing a shipped default does
  need a migration, paired with a `DEFAULT_ADOPTIONS` entry (see the settings catalogue).
