# Data Pipeline — Goose In A Pond

> Version 0.1.0 | March 2026
> Target hardware: NVIDIA Jetson Orin Nano (8–16 GB unified RAM, ARM64)

---

## Overview

GIAP's data pipeline addresses the **3 Vs of data** for a constrained, local-first AI assistant:

| V | Problem | Solution |
|---|---------|----------|
| **Velocity** | Audio arrives in real-time; REST requests arrive concurrently | Stage 1 acquisition + Stage 2 `tokio::mpsc` transport |
| **Variety** | Audio → text, chat text, sensor events, camera frames, device commands | Typed `PipelineInput` enum; each variety has its own ingestion path |
| **Volume** | 7B Q4 model on 8K context; storage grows forever | Stage 3 context budget + Stage 5 TTL pruning background task |

---

## Current Broken Flow (What Exists Today)

```
REST /api/v1/chat ──────────────────────────────────> echo "Received: {msg}"  ← NO LLM!
                                                             ↓
                               ChatService.chat_once()
                                 → get_messages()    ← ALL history, UNBOUNDED
                                 → provider.complete(SYSTEM_PROMPT, ALL_messages)
                                                     ← max_tokens: 512 HARDCODED
                                                     ← temperature: 0.7 HARDCODED
                                 → add_message()
                                 → maybe_generate_title()  ← 3 extra DB reads

cpal audio (5s fixed) → spawn_blocking → WAV → POST /inference (whisper.cpp) → transcript
  (audio path works but is not connected to ChatService context)

event_log / sensor_readings → grow forever (no TTL, no pruning)
```

**Root cause**: After 20–30 exchanges on a 7B Q4 model with an 8K context window, the context silently overflows. The model drops the most recent messages — not the oldest — producing incoherent responses with no error signal.

---

## 5-Stage Pipeline Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│  Stage 1 — ACQUISITION                                               │
│                                                                      │
│  Audio   → cpal ring buffer → (future VAD) → WAV                    │
│              → POST whisper.cpp /inference → transcript              │
│  Text    → stdin / POST /api/v1/chat → message string               │
│  Sensors → (future) MQTT/HTTP event → in-memory accumulator         │
│              → flush every 5 min → sensor_readings                  │
│  Camera  → (future) V4L2 frame → vision model → event metadata only │
└─────────────────────────────┬────────────────────────────────────────┘
                              │  PipelineInput enum
┌─────────────────────────────▼────────────────────────────────────────┐
│  Stage 2 — TRANSPORT                                                 │
│                                                                      │
│  tokio::mpsc::channel::<PipelineInput>(8)                            │
│  Voice → sender.send(PipelineInput::Voice { text, session_id })      │
│  REST  → sender.send(PipelineInput::Rest  { text, session_id,        │
│                                             response_tx })           │
│  (single shared queue; priority queue deferred — measure first)      │
└─────────────────────────────┬────────────────────────────────────────┘
                              │
┌─────────────────────────────▼────────────────────────────────────────┐
│  Stage 3 — CONTEXT ASSEMBLY  ← most critical for Jetson             │
│                                                                      │
│  get_recent_messages(session_id, limit=100)  ← bounded DB read      │
│  trim_to_budget(messages, USABLE_HISTORY_CHARS)  ← pure fn          │
│                                                                      │
│  Constants:                                                          │
│    MAX_CONTEXT_CHARS       = 12_000  (~3K tokens @ 4 chars/token)   │
│    RESERVE_FOR_RESPONSE    =  2_048                                  │
│    USABLE_HISTORY_CHARS    =  9_952                                  │
└─────────────────────────────┬────────────────────────────────────────┘
                              │  Vec<ChatMessage> (budget-trimmed)
┌─────────────────────────────▼────────────────────────────────────────┐
│  Stage 4 — INFERENCE                                                 │
│                                                                      │
│  LlmProvider::complete(SYSTEM_PROMPT, budgeted_messages)             │
│    max_tokens:  1024  (was 512, configurable via builder)            │
│    temperature: 0.7   (configurable via builder)                     │
│  Fallback: primary → MockProvider (if primary unavailable)           │
└─────────────────────────────┬────────────────────────────────────────┘
                              │  response text
┌─────────────────────────────▼────────────────────────────────────────┐
│  Stage 5 — STORAGE + OUTPUT                                          │
│                                                                      │
│  add_message() → session_messages (both user + assistant)            │
│  maybe_generate_title() → sessions.title                             │
│                                                                      │
│  TTL pruning (background tokio task, 6h interval):                   │
│    event_log        → 30-day retention                               │
│    sensor_readings  → 7-day retention                                │
│    camera_events    → 14-day retention (acknowledged only)           │
│    session_messages → 500 messages max per session                   │
└──────────────────────────────────────────────────────────────────────┘
```

---

## Implementation Plan (Priority Order)

### P1 — Context Budget Manager ★ HIGHEST IMPACT

**Problem**: `chat_once()` calls `get_messages()` (unbounded) on every exchange. After 20–30 turns the 8K context overflows silently.

**New file**: `crates/pond-core/src/models/services/context_budget.rs`

```rust
pub const MAX_CONTEXT_CHARS: usize = 12_000;      // ~3K tokens @ 4 chars/token
pub const RESERVE_FOR_RESPONSE_CHARS: usize = 2_048;
pub const USABLE_HISTORY_CHARS: usize = MAX_CONTEXT_CHARS - RESERVE_FOR_RESPONSE_CHARS;

/// Walk messages newest-first, keep until budget exhausted, return oldest-first.
pub fn trim_to_budget(messages: Vec<ChatMessage>) -> Vec<ChatMessage>
```

**New port method** in `pond-core/src/user_data/ports/session_storage.rs`:

```rust
/// Fetch the most recent `limit` messages in chronological order.
async fn get_recent_messages(
    &self, session_id: &str, limit: usize,
) -> Result<Vec<SessionMessage>, SessionStorageError>;
```

SQL implementation (DESC LIMIT, then reverse):
```sql
SELECT id, session_id, role, content, created_at
FROM session_messages WHERE session_id = ?
ORDER BY created_at DESC, rowid DESC LIMIT ?
```

**Modified call site** in `chat_once()`:
```rust
// BEFORE (unbounded)
let stored = self.session_storage.get_messages(&self.session_id).await?;

// AFTER (budget-aware)
let stored = self.session_storage.get_recent_messages(&self.session_id, 100).await?;
let messages = context_budget::trim_to_budget(stored.into_iter().map(|sm| sm.message).collect());
```

**Files to create/modify**:
- Create `crates/pond-core/src/models/services/context_budget.rs`
- Modify `crates/pond-core/src/user_data/ports/session_storage.rs` — add method
- Modify `crates/pond-core/src/user_data/mocks/mock_session.rs` — implement method
- Modify `crates/pond-infra/src/sqlite_session_storage.rs` — implement method
- Modify `crates/pond-core/src/shared/services/chat.rs` — replace call site
- Modify `crates/pond-core/src/<quadrant>/services/mod.rs` — expose `pub mod context_budget`

**Verification**:
```bash
cargo test -p pond-core -- context_budget
cargo test -p pond-infra -- get_recent_messages
```
Test: insert 200 messages × 200 chars = 40K chars → `trim_to_budget()` returns ≤50 messages within 9,952 char budget.

---

### P2 — LlamafileProvider Config

**Problem**: `max_tokens: 512` is hardcoded. 512 tokens cuts off most useful responses on a home assistant.

**Modify** `crates/pond-adapters-llamafile/src/lib.rs`:
```rust
pub struct LlamafileProvider {
    client: Client,
    endpoint: String,
    model: String,
    max_tokens: u32,   // default: 1024
    temperature: f32,  // default: 0.7
}

impl LlamafileProvider {
    pub fn with_max_tokens(mut self, n: u32) -> Self { self.max_tokens = n; self }
    pub fn with_temperature(mut self, t: f32) -> Self { self.temperature = t; self }
}
```

Same pattern for `crates/pond-adapters-ollama/src/lib.rs`.

**Verification**:
```bash
cargo test -p pond-adapters-llamafile
```
Assert `CompletionRequest` serializes `max_tokens: 1024` by default.

---

### P3 — TTL Migrations + Pruning Job

**Problem**: `event_log`, `sensor_readings`, `camera_events` grow forever. On a 32–64 GB eMMC Jetson, this will fill the disk within weeks of sensor data.

**New migration files**:

`crates/pond-infra/migrations/logs/0002_sensor_readings.sql`:
```sql
CREATE TABLE IF NOT EXISTS sensor_readings (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    device_id   TEXT    NOT NULL,
    sensor_type TEXT    NOT NULL,
    value       REAL    NOT NULL,
    unit        TEXT    NOT NULL,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_sensor_readings_device_type
    ON sensor_readings(device_id, sensor_type);
CREATE INDEX IF NOT EXISTS idx_sensor_readings_created_at
    ON sensor_readings(created_at);
```

`crates/pond-infra/migrations/logs/0003_camera_events.sql`:
```sql
CREATE TABLE IF NOT EXISTS camera_events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    camera_id     TEXT    NOT NULL,
    event_type    TEXT    NOT NULL,
    confidence    REAL,
    snapshot_path TEXT,
    metadata      TEXT,
    acknowledged  INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_camera_events_created_at
    ON camera_events(created_at);
```

**New file**: `crates/pond-infra/src/pruning.rs`

Background task runs every 6 hours and executes:

| Table | Retention Rule |
|-------|---------------|
| `event_log` | Delete rows older than 30 days |
| `sensor_readings` | Delete rows older than 7 days |
| `camera_events` | Delete acknowledged rows older than 14 days |
| `session_messages` | Keep only 500 most recent per session |

**Wire in** `pond-server/src/main.rs`:
```rust
let db_for_pruning = db.clone();
tokio::spawn(async move {
    pond_infra::pruning::run_pruning(
        db_for_pruning.logs.clone(),
        db_for_pruning.system.clone(),
        Default::default(),
    ).await;
});
```

**Verification**:
```bash
cargo test -p pond-infra -- pruning
```
Test: insert 5 rows with `datetime('now', '-31 days')` → run `prune_once()` → count = 0.

---

### P4 — Wire LLM into REST `/api/v1/chat` Handler

**Problem**: The REST chat endpoint currently echoes `"Received: {msg}"` instead of calling the LLM. Sessions work, persistence works, but no intelligence.

**Add to** `AppState` in `crates/pond-api/src/lib.rs`:
```rust
pub agent: Arc<dyn pond_core::models::ports::agent::Agent>,
pub llm_provider: Option<Arc<dyn pond_core::models::ports::provider::LlmProvider>>,
```

**Replace echo** in `crates/pond-api/src/routes.rs`, `chat()` handler:
```rust
let mut service = ChatService::new(
    state.agent.clone(),
    session_id.clone(),
    state.session_storage.clone(),
);
if let Some(provider) = &state.llm_provider {
    service = service.with_provider(provider.clone());
}
let response_text = service
    .chat_once(req.message.clone())
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
```

**Wire** in `pond-server/src/main.rs`:
```rust
let agent: Arc<dyn Agent> = Arc::new(MockAgent::new());
let llm_provider: Option<Arc<dyn LlmProvider>> = Some(
    Arc::new(LlamafileProvider::new(None))
);
```

**Update** `crates/pond-api/tests/onboarding_integration_test.rs` — add new `AppState` fields.

**Verification**:
```bash
cargo test -p pond-api
# POST /api/v1/chat {"message": "What is 2+2?"} → LLM response, not echo
# Second call with same session_id → LLM references prior context
```

---

### P5 — Pipeline Transport Channel (Deferred)

Add `PipelineInput` enum in `crates/pond-core/src/<quadrant>/domain/pipeline.rs`:
```rust
pub enum PipelineInput {
    Voice { text: String, session_id: String },
    Rest  { text: String, session_id: String, response_tx: tokio::sync::oneshot::Sender<String> },
}
```

Add `mpsc::channel::<PipelineInput>(8)` to `ChatService`. Wire voice loop and REST handler to the same sender.

**Defer until**: voice + REST concurrency is a measured problem on-device.

---

### P6 — Sensor/Camera Ports (Deferred)

New port files in `pond-core/src/<quadrant>/ports/`:
- `sensor_storage.rs`: `add_reading()`, `get_readings(since)`
- `vision_events.rs`: `add_event()`

Backed by `SqliteSensorStorage` and `SqliteVisionStorage` in `pond-infra`, writing to the P3 migration tables.

**Defer until**: actual sensors are being connected.

---

### P7 — Streaming LLM Responses (Deferred)

Add `complete_stream()` to `LlmProvider` returning `impl Stream<Item=Result<String>>`. Implement in `LlamafileProvider` using reqwest byte-stream SSE parsing. Add `GET /api/v1/chat/stream` axum SSE route.

**Defer until**: P1–P4 are stable on-device.

---

## Data Retention Policy

| Table | DB | Retention | Notes |
|-------|----|-----------|-------|
| `session_messages` | System | 500 msgs/session | Oldest pruned, keeps recency |
| `event_log` | Logs | 30 days | General telemetry |
| `sensor_readings` | Logs | 7 days | High-frequency, compact |
| `camera_events` | Logs | 14 days (acknowledged) | Unacknowledged alerts kept |
| `sessions` | System | Forever | Metadata only — tiny |
| `memory` | System | Forever | User-defined facts |
| `routines` | System | Until deleted | User automations |

---

## Jetson Orin Nano Performance Targets

| Metric | Target | Basis |
|--------|--------|-------|
| Context window | 8K tokens | 7B Q4 typical |
| Usable history budget | ~2,500 tokens (9,952 chars) | 8K − system prompt − response reserve |
| Inference speed | ~12 tokens/sec | Measured: Jetson Orin Nano 8GB |
| Response latency (512 token answer) | ~43 seconds | At 12 tok/sec |
| Max messages in context | ~50 (at 200 chars avg) | Within 9,952 char budget |
| DB pruning interval | 6 hours | Background tokio task |
| Transport queue depth | 8 | `mpsc::channel(8)` |

---

## Complexity Rejected

| Idea | Why Rejected |
|------|-------------|
| Vector embeddings for semantic history search | New dep (ort/candle), requires vocab model, marginal gain over recency for conversational assistant |
| External message broker (NATS, Redis) | Incompatible with local-first embedded deployment |
| Real tokenizer (tiktoken, sentencepiece) | Requires model-specific vocab file, compile overhead; chars/4 sufficient for budget management |
| Per-session context window state in DB | Stateless `trim_to_budget()` is simpler and always correct |
| Priority queue (voice > REST) | Single-user home assistant; voice and REST won't compete simultaneously; measure first |
| VAD (voice activity detection) | `with_duration()` builder exists — expose via CLI first; build VAD only if 5s window is measured as the problem |
| Time-series DB (InfluxDB, TimescaleDB) | External dep, violates local-first principle; SQLite + TTL pruning sufficient for MVP |

---

## Files to Create

| File | Purpose |
|------|---------|
| `crates/pond-core/src/models/services/context_budget.rs` | Token budget constants + `trim_to_budget()` |
| `crates/pond-infra/src/pruning.rs` | TTL pruning background task |
| `crates/pond-infra/migrations/logs/0002_sensor_readings.sql` | sensor_readings table + indexes |
| `crates/pond-infra/migrations/logs/0003_camera_events.sql` | camera_events table + indexes |
| `crates/pond-core/src/<quadrant>/domain/pipeline.rs` | `PipelineInput` enum (P5, deferred) |

## Files to Modify

| File | Change |
|------|--------|
| `crates/pond-core/src/user_data/ports/session_storage.rs` | Add `get_recent_messages()` method |
| `crates/pond-core/src/user_data/mocks/mock_session.rs` | Implement `get_recent_messages()` |
| `crates/pond-core/src/shared/services/chat.rs` | Replace `get_messages()` with budget-aware load |
| `crates/pond-core/src/<quadrant>/services/mod.rs` | Expose `context_budget` module |
| `crates/pond-infra/src/sqlite_session_storage.rs` | Implement `get_recent_messages()` (DESC LIMIT then reverse) |
| `crates/pond-infra/src/lib.rs` | Expose `pruning` module |
| `crates/pond-adapters-llamafile/src/lib.rs` | Add `max_tokens`, `temperature` builder fields |
| `crates/pond-adapters-ollama/src/lib.rs` | Same config changes |
| `crates/pond-api/src/lib.rs` | Add `agent`, `llm_provider` to AppState |
| `crates/pond-api/src/routes.rs` | Wire `ChatService` into REST `/chat` handler |
| `crates/pond-api/tests/onboarding_integration_test.rs` | Add new AppState fields |
| `crates/pond-server/src/main.rs` | Populate `agent`/`llm_provider` + spawn pruning task |

---

---

## Biometric User Differentiation

### Motivation

GIAP needs to distinguish which household member is speaking, both at the wake-word stage and during conversation. This enables:
- Personalised system prompts (different personality per user)
- Per-user settings (wake word, language, TTS voice)
- Attributing conversation sessions to the correct user profile
- Security: only registered users can trigger sensitive commands

Two modalities are stored — voice prints (speaker embeddings) and face embeddings — because either can work independently when the other is unavailable.

### Privacy-First Design

| Principle | Implementation |
|-----------|---------------|
| **Never store raw audio/video** | Discard after embedding extraction |
| **Embeddings only** | Fixed-size float32 BLOBs — no reconstructable biometric |
| **Local storage only** | No cloud sync; embeddings stay in `pond_system.db` |
| **Threshold tunable** | Each registration has its own similarity threshold |
| **Audit trail** | All identification events logged to `pond_logs.db` |

### Storage Schema — System DB (`pond_system.db`)

#### `speaker_embeddings`

Voice prints extracted from enrollment audio samples.

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | TEXT | PK | UUID |
| `user_id` | TEXT | NOT NULL, FK → user_profiles(id) ON DELETE CASCADE | |
| `embedding` | BLOB | NOT NULL | float32 vector, little-endian serialized |
| `model_name` | TEXT | NOT NULL | `"x_vector_512"`, `"resemblyzer_256"` |
| `embedding_dim` | INTEGER | NOT NULL | 256 or 512 |
| `audio_duration_ms` | INTEGER | Nullable | Length of enrollment sample |
| `created_at` | TEXT | NOT NULL DEFAULT datetime('now') | |
| `last_verified_at` | TEXT | Nullable | Last time this embedding matched |

Index: `idx_speaker_embeddings_user_id` on `(user_id)`

#### `speaker_registrations`

Maps a user to their active voice print with matching parameters.

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | TEXT | PK | UUID |
| `user_id` | TEXT | NOT NULL, FK → user_profiles(id) ON DELETE CASCADE | |
| `embedding_id` | TEXT | NOT NULL, FK → speaker_embeddings(id) | |
| `similarity_threshold` | REAL | NOT NULL DEFAULT 0.5 | Cosine similarity 0–1 |
| `enrollment_count` | INTEGER | NOT NULL DEFAULT 1 | Voice samples used to enroll |
| `is_primary` | INTEGER | NOT NULL DEFAULT 0 | Primary voice print for this user |
| `created_at` | TEXT | NOT NULL DEFAULT datetime('now') | |

#### `face_embeddings`

Face embeddings extracted from camera frames during enrollment.

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | TEXT | PK | UUID |
| `user_id` | TEXT | NOT NULL, FK → user_profiles(id) ON DELETE CASCADE | |
| `embedding` | BLOB | NOT NULL | float32 vector, little-endian serialized |
| `model_name` | TEXT | NOT NULL | `"arcface_512"`, `"mobilefacenet_128"` |
| `embedding_dim` | INTEGER | NOT NULL | 128 or 512 |
| `created_at` | TEXT | NOT NULL DEFAULT datetime('now') | |
| `last_verified_at` | TEXT | Nullable | |

Index: `idx_face_embeddings_user_id` on `(user_id)`

#### `face_registrations`

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | TEXT | PK | UUID |
| `user_id` | TEXT | NOT NULL, FK → user_profiles(id) ON DELETE CASCADE | |
| `embedding_id` | TEXT | NOT NULL, FK → face_embeddings(id) | |
| `similarity_threshold` | REAL | NOT NULL DEFAULT 0.6 | Cosine similarity 0–1 |
| `enrollment_count` | INTEGER | NOT NULL DEFAULT 1 | |
| `is_primary` | INTEGER | NOT NULL DEFAULT 0 | |
| `created_at` | TEXT | NOT NULL DEFAULT datetime('now') | |

### Storage Schema — Logs DB (`pond_logs.db`)

#### `diarization_logs`

Per-session speaker attribution — who said what and when.

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | TEXT | PK | UUID |
| `session_id` | TEXT | NOT NULL, FK → sessions(id) ON DELETE CASCADE | |
| `speaker_user_id` | TEXT | Nullable | NULL if speaker unrecognised |
| `start_ms` | INTEGER | NOT NULL | Millisecond offset within session audio |
| `end_ms` | INTEGER | NOT NULL | |
| `confidence` | REAL | Nullable | 0–1 match confidence |
| `created_at` | TEXT | NOT NULL DEFAULT datetime('now') | |

Index: `idx_diarization_logs_session_id` on `(session_id)`

#### `biometric_audit_log`

Audit trail for all identification events (successful or failed).

| Column | Type | Constraints | Notes |
|--------|------|-------------|-------|
| `id` | INTEGER | PK AUTOINCREMENT | |
| `modality` | TEXT | NOT NULL | `"voice"` or `"face"` |
| `result` | TEXT | NOT NULL | `"identified"`, `"unknown"`, `"rejected"` |
| `matched_user_id` | TEXT | Nullable | If identified |
| `confidence` | REAL | Nullable | Similarity score |
| `created_at` | TEXT | NOT NULL DEFAULT datetime('now') | |

Index: `idx_biometric_audit_created_at` on `(created_at)` — prune after 30 days

### Embedding Dimensionality Reference

| Modality | Model | Dims | BLOB Size | Similarity | Threshold |
|----------|-------|------|-----------|------------|-----------|
| Voice | Resemblyzer | 256-d float32 | 1 KB | Cosine | 0.50 |
| Voice | x-vector/TDNN | 512-d float32 | 2 KB | Cosine | 0.55 |
| Face | MobileFaceNet | 128-d float32 | 512 B | Cosine | 0.55 |
| Face | ArcFace | 512-d float32 | 2 KB | Cosine | 0.60 |

### Rust Ecosystem

| Component | Crate | ARM64 | Notes |
|-----------|-------|-------|-------|
| Face inference | `ort` (ONNX Runtime) | Yes | **CPU execution provider**, on every platform — see note below |
| Face preprocessing | `image` + custom | Yes | Resize, normalize, BGR→RGB |
| Speaker embedding | subprocess/HTTP | Yes | Resemblyzer or pyannote via HTTP service |
| Vector similarity | `sqlite-vec` extension | Yes | SIMD-accelerated cosine in SQLite |
| Constant-time comparison | `subtle` crate | Yes | Prevents timing side-channel on threshold |

> **No ONNX workload in GIAP is GPU-accelerated today**, on any platform. This
> row previously read "GPU-accelerated on Jetson via CUDA"; that was wrong. An
> `ort` execution provider is used only if something calls
> `with_execution_providers`, and nothing in this workspace does — not
> `pond-adapters-face-onnx`, not `pond-adapters-vision-onnx`, not `piper-rs`,
> not `fastembed`. All four share one process-global ONNX Runtime, which on a
> Jetson is the CPU aarch64 build that `ensure_onnx_runtime()` downloads;
> JetPack ships no ONNX Runtime, and no prebuilt CUDA one exists for this
> target. Anyone who has sized a face or vision workload against the old claim
> should re-measure. See `crates/pond-adapters-piper/Cargo.toml` for the full
> account and what changing it would cost.

### Port Plan

```
pond-core/src/<quadrant>/ports/
  speaker_id.rs      register_speaker(user_id, audio) → SpeakerEmbedding
                     identify_speaker(audio) → Option<(user_id, confidence)>

  face_recognition.rs register_face(user_id, image) → FaceEmbedding
                      identify_face(image) → Option<(user_id, confidence)>

Adapters (deferred — implement when hardware is ready):
  pond-adapters-speaker-embed/   x-vector ONNX or HTTP bridge to Resemblyzer
  pond-adapters-face-onnx/       mtCNN + ArcFace via ort + TensorRT
```

### Migration Order

These follow the P3 migrations in the main pipeline:

```
crates/pond-infra/migrations/system/
  0003_speaker_embeddings.sql    — speaker_embeddings, speaker_registrations
  0004_face_embeddings.sql       — face_embeddings, face_registrations

crates/pond-infra/migrations/logs/
  0004_diarization_logs.sql      — diarization_logs
  0005_biometric_audit.sql       — biometric_audit_log (TTL: 30 days)
```

> [!IMPORTANT]
> Migration files must exist on disk before `db.rs` is compiled (sqlx compile-time macro).
> Run `cargo sqlx prepare` after adding any new migration.

---

## See Also

- Database schema: `docs/architecture/database_schema.md`
- Port/adapter guide: `docs/creating-ports-and-adapters.md`
- Implementation plan: `.ai/scratchpad.md`
