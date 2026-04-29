# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

---

## Build Commands

```bash
# Fast server build (skips Goose recompile, ~2s)
cargo build -p pond-core -p pond-infra -p pond-api -p pond-server

# Full workspace (compiles Goose submodule — 10+ min first time)
cargo build --workspace

# Cross-compile for Jetson Orin Nano (requires `cargo install cross` + Docker)
SQLX_OFFLINE=true make server       # CPU-only
# CUDA: build natively on the Jetson — see Makefile: `make server-cuda`
```

## Test Commands

```bash
# Rust — single crate (fastest iteration loop)
cargo test -p pond-core
cargo test -p pond-api
cargo test -p pond-adapters-ollama

# Run a single test by name
cargo test -p pond-core -- classify_request::tests::think_keywords_route_to_think

# Live integration tests (require real hardware/services — #[ignore] by default)
GIAP_LLAMAFILE_URL=http://127.0.0.1:8080 \
  cargo test -p pond-server --test live_provider_test -- --ignored llamafile

GIAP_OLLAMA_URL=http://127.0.0.1:11434 GIAP_OLLAMA_MODEL=gemma3:4b \
  cargo test -p pond-server --test live_provider_test -- --ignored ollama

# Frontend unit tests (vitest, happy-dom, no Tauri required)
cd pond-desktop && npm test          # vitest run
cd pond-desktop && npx vitest --watch  # watch mode

# Playwright E2E tests (auto-starts Vite dev server, all API calls mocked)
cd pond-desktop && npx playwright test tests/e2e/
cd pond-desktop && npx playwright test tests/e2e/chat.spec.ts --ui

# Live E2E against real pond-server (optional, skipped when var unset)
GIAP_SERVER_URL=http://127.0.0.1:4000 \
  cd pond-desktop && npx playwright test tests/e2e/
```

## Run the Server

```bash
# First-time setup (downloads Whisper ASR model)
cargo run -p pond-server -- setup

# Serve (REST API on port 4000, web dashboard at http://localhost:4000)
cargo run -p pond-server -- serve

# With native Tauri desktop app
cargo run -p pond-server -- serve --native

# Desktop dev mode (starts Vite + pond-server together)
cd pond-desktop && npm run dev
# OR just Tauri
cd pond-desktop && npm run tauri dev
```

## Lint & Format

```bash
cargo fmt
cargo clippy
```

---

## Architecture

GIAP uses **hexagonal (ports & adapters)** architecture. The rule: `pond-core` never imports from Goose, SQLx, Axum, or any external framework. All I/O crosses a port trait.

```
pond-server  (binary — wires adapters into AppState, starts Axum + Tauri)
    │
pond-api     (Axum HTTP router, AppState struct, SSE streaming, REST handlers)
    │
pond-core    (pure Rust domain — no external deps)
  ├── domain/    ChatMessage, Device, Schedule, MemoryFragment, Settings …
  ├── ports/     async_trait interfaces — LlmProvider, Agent, VoiceInput, SchedulerPort …
  └── services/  ModelRouter, ChatService, request_classifier, mock impls
    │
pond-infra           (SQLite via SQLx — two databases)
pond-infra-scheduler (tokio-cron-scheduler adapter)
pond-adapters-*      (one crate per external capability)
pond-desktop         (Tauri 2 + React 19 + Vite — the GUI)
```

### Adding a new capability

Follow the 5-step pattern (documented in `docs/creating-ports-and-adapters.md`):
1. **Domain type** in `crates/pond-core/src/domain/<name>.rs` — pure Rust, no external imports
2. **Port trait** in `crates/pond-core/src/ports/<name>.rs` — `#[async_trait]` interface
3. **Mock + tests** in `crates/pond-core/src/services/` — must pass before any real adapter
4. **Real adapter** as `crates/pond-adapters-<name>/` — implements the port trait
5. **Wire** in `crates/pond-server/src/main.rs` — inject into `AppState`

### LLM Routing (`ModelRouter`)

Every chat message is classified by `pond-core/src/services/request_classifier.rs` into one of three roles: **Chat** (default) · **Think** (reasoning keywords like "explain why", "analyze") · **Task** (agentic keywords like "remind me", "schedule", "turn on"). The `ModelRouter` routes each role to a separately configured provider.

Provider switch at runtime: `PUT /api/v1/settings` with `chat_provider` / `chat_model` triggers `rebuild_model_router()` in `pond-api/src/routes.rs`, which hot-swaps `AppState.llm_provider` (a `RwLock`). Supported providers: `"llamafile"` · `"ollama"` · `"local"` (in-process GGUF via llama-cpp-2).

### GGUF Provider (`local`)

`LocalInferenceLlmAdapter` in `crates/pond-adapters-local-inference/src/lib.rs` wraps Goose's `LocalInferenceProvider`. Two construction paths:
- **HF format** (`"bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M"`): `new_with_data_dir()` derives filename, registers in Goose's global `LocalModelRegistry`, then calls `new()`.
- **Raw filename** (`"gemma-4-E2B-it-Q4_K_M.gguf"`): `new_with_data_dir()` strips the `.gguf` extension as a synthetic registry ID, registers with `local_path = $data_dir/models/gguf/{filename}`, then calls `new(stem)`.

The registry is a global `OnceLock<Mutex<LocalModelRegistry>>` in Goose. Registration must happen before `LocalInferenceProvider::from_env()` is called, because it calls `resolve_model_path(model_id)` which looks up the registry by ID at first inference.

### SSE Streaming

Chat responses stream over Server-Sent Events at `POST /api/v1/chat/stream`. Each event is a JSON line:
- `{"type":"text","content":"...","token":"..."}` — partial token
- `{"done":true,"session_id":"...","model_role":"chat|think|task","usage":{"prompt_tokens":N,"completion_tokens":N}}`
- `{"error":"..."}` — provider error

Token usage comes from Ollama (`prompt_eval_count`/`eval_count`); llamafile only emits usage in its final SSE chunk (many builds omit it entirely); GGUF (via Goose) discards usage internally.

### Databases

Two SQLite databases in `$DATA_DIR` (macOS default: `~/Library/Application Support/goose-in-a-pond/`):
- `pond_system.db` — settings, sessions, devices, onboarding, profiles, memory, skills
- `pond_logs.db` — sensor readings, camera events, telemetry

Migrations live in `crates/pond-infra/migrations/system/` and `migrations/logs/`, applied automatically via `sqlx::migrate!()` on startup. Cross-compile requires `SQLX_OFFLINE=true`.

### Desktop App (`pond-desktop`)

Tauri 2.0 app with three modes: **GUI sidebar** (React sections) · **Voice mode** (mic orb + TranscriptFeed) · **Canvas mode** (floating overlay, `Cmd+Shift+G`).

State lives in `src/state/` — a React context + `useReducer` pattern. All API calls go through `src/api/PondApiClient.ts` (singleton `api` export). In Tauri context, `AppContext.tsx` listens to Tauri events (`server-status`, `voice-state`) and auto-completes onboarding. In browser/Playwright context, it marks the server online immediately.

Playwright E2E tests in `tests/e2e/` use `helpers/api-mocks.ts` (`mockAllApiRoutes`) to intercept all API calls — no running pond-server required. Live E2E tests are gated by `GIAP_SERVER_URL` env var.

### Voice Pipeline

Whisper ASR (HTTP server at port 9000) → wake word detection → record → `POST /api/v1/transcribe` → chat. TTS via Piper subprocess or Piper HTTP server. Working voice config: `en_US-lessac-medium.onnx` (Piper).

### Goose Submodule

`goose/` is a git submodule of Block's Goose agent framework. `pond-adapters-goose` and `pond-adapters-local-inference` depend on it. The outer GIAP workspace re-declares Goose's transitive dependencies (rmcp, sacp, tree-sitter-*) in the root `Cargo.toml` to resolve workspace version conflicts — do not remove them.

### Known model identifiers (dev/test)

- **Llamafile**: `gemma-2-2b-it.Q4_K_M` (served on port 8080 by default)
- **Ollama**: `gemma3:4b` · `gemma4:latest`
- **GGUF**: `gemma-4-E2B-it-Q4_K_M.gguf` (3.1 GB, at `$DATA_DIR/models/gguf/`)
- **Piper TTS voice**: `en_US-lessac-medium.onnx`
