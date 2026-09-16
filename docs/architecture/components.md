# Component Breakdown

Each crate in `crates/` and the `pond-desktop/` app has a distinct responsibility within the hexagonal system. This document describes what each one does and how it relates to the others.

---

## Layer Overview

```
┌──────────────────────────────────────────────────────┐
│  Drivers (things that call the Core)                 │
│  pond-server (CLI + HTTP) · pond-desktop (Electron)  │
├──────────────────────────────────────────────────────┤
│  Application / API Layer                             │
│  pond-api  (Axum router + REST DTOs)                 │
├──────────────────────────────────────────────────────┤
│  DOMAIN CORE  (never imports framework deps)         │
│  pond-core: user_data/ · models/ · mcp/ · security/  │
│            · shared/  (each: domain/ ports/ services/)│
├──────────────────────────────────────────────────────┤
│  Driven Adapters (implement Core ports)              │
│  pond-infra · pond-infra-scheduler                   │
│  pond-adapters-goose · pond-adapters-weather         │
│  pond-adapters-whisper · pond-adapters-piper         │
│  pond-adapters-ollama · pond-adapters-llamafile      │
│  pond-adapters-local-inference · pond-adapters-mcp-memory │
│  pond-mcp-server                                     │
└──────────────────────────────────────────────────────┘
```

---

## Core Crates

### `pond-core` — The Brain

The heart of GIAP. Contains all domain logic with zero external framework dependencies.

Files are grouped into four quadrants around the user, plus a `shared/` module
for agent-loop plumbing. Every quadrant carries its own `domain/`, `ports/`, and
`services/` (and a `mocks/` for test doubles).

| Quadrant | Purpose |
|---|---|
| `src/user_data/` | Facts about / owned by the household: `profile`, `memory`, `session`, `settings`, `skill`, `recipe`, `prompt_template`/`prompt_extra`, `schedule`, `onboarding`, `sensor`, `draft`, `face` |
| `src/models/` | Anything that runs or routes inference: `message`, `model_record`, `model_capabilities`, providers, `inference`/`inference_pool`, `embedding`, `voice_input`/`voice_output`, `wake_word`, catalog/storage/downloader, plus context-budget / prompt-builder / history / thought-filter services |
| `src/mcp/` | The tool surface: `extension_manager`, `marketplace`, `mcp_server`, `mcp_knowledge`, `notification`, and `tools::{registry, dispatcher, caller, cache, agent}` |
| `src/security/` | The enclosing boundary: `secret`, `handshake`, `telemetry`, `event_log`, `policy`, `turn_metrics`, `oauth_provider` |
| `src/shared/` | Agent-loop plumbing used by every quadrant: `agent` types, `chat` (run_loop), `stdin_input`, `print_output` |

Within each quadrant: `domain/` holds pure Rust types, `ports/` holds the
`async_trait` interface definitions (one file per capability), and `services/`
holds the use-case orchestrators (`ChatService`, `ContextCompactor`, …).

**Key invariant:** `pond-core` must never import from `goose::*`, `sqlx::*`, `axum::*`, or any HTTP/filesystem library.

### `pond-api` — HTTP Layer

Axum 0.8 router, route handlers, and shared DTOs. Exports `AppState` (the dependency container) and `build_router()`.

- All REST routes versioned under `/api/v1/`
- Authentication middleware (`auth_middleware`) validates Bearer tokens via `Handshake::validate_token()`
- Rate limiting middleware (100 req / 60s per client)
- Onboarding guard blocks protected routes until setup is complete

### `pond-server` — Composition Root

The runnable binary. Wires all adapters into `AppState` and starts the Axum server.

- Parses CLI arguments via `clap`
- Initializes both SQLite databases (`pond_system.db`, `pond_logs.db`)
- Registers the GIAP builtin MCP extension with Goose
- Starts `pond-api` HTTP server + optional voice loop

---

## Adapter Crates

### `pond-infra` — Persistence

SQLite repositories for all domain entities, implemented with SQLx.

| Module | Port it implements |
|---|---|
| `sqlite_session_storage` | `SessionStorage` |
| `sqlite_device_registry` | `DeviceRegistry` |
| `sqlite_settings` | `SettingsRepository` |
| `sqlite_memory` | `MemoryRepository` |
| `sqlite_skill` | `UserSkillRepository` |
| `sqlite_recipe` | `AgentRecipeRepository` |
| `sqlite_prompt_template` | `PromptTemplateRepository` |
| `sqlite_prompt_extra` | `PromptExtraRepository` |
| `mock_handshake` | `Handshake` (in-memory, for dev/test) |

Two databases, initialized via `sqlx::migrate!()` on startup:
- `pond_system.db` — sessions, devices, settings, memory, skills, recipes, prompts
- `pond_logs.db` — event log, sensor readings, camera events

### `pond-infra-scheduler` — Cron Scheduling

`CronSchedulerAdapter` wraps `tokio-cron-scheduler`. Uses 6-field cron format with a leading seconds field (`"0 0 8 * * *"` = 08:00 daily). Dispatches tasks via `WebhookTaskExecutor`.

### `pond-adapters-goose` — Goose Agent (Primary Inference)

Wraps [Block's Goose](https://github.com/block/goose) as the primary LLM agent.

`GooseAdapter::chat()` on every turn:
1. Loads `Settings` from the DB
2. Fetches the active prompt template and renders it
3. Injects `PromptExtra` records and `UserSkill` content into the system prompt
4. Optionally injects recent memory fragments
5. Hot-swaps the Goose provider when the model config changes
6. Auto-loads the `"giap"` builtin MCP extension
7. Runs the full Goose agentic loop and returns aggregated text + tool metadata

### `pond-adapters-weather` — Weather

`OpenMeteoWeatherAdapter` implements `WeatherProvider` using the free [Open-Meteo](https://open-meteo.com) API with a 15-minute TTL cache. Location is configured via `Settings.lat` / `Settings.lon`. Exposed to the LLM exclusively through the `giap__get_current_weather` MCP tool.

### `pond-adapters-whisper` — Speech Recognition

`WhisperInput` posts audio to a running [whisper.cpp](https://github.com/ggerganov/whisper.cpp) HTTP server (`POST /inference`) — no C++ bindings required.

`WhisperKeywordDetector` listens for a configurable trigger word (default: `"goose"`) to implement wake-word detection.

### `pond-adapters-piper` — Text-to-Speech

`PiperOutput` spawns a [Piper](https://github.com/rhasspy/piper) subprocess, pipes text to stdin, and plays the raw audio output via `rodio`. Supports `en_US-lessac-medium` (default) and other ONNX voice models.

### `pond-adapters-ollama` — Ollama Provider

HTTP client for the Ollama `/api/chat` endpoint. Implements `LlmProvider` with support for `with_max_tokens()`, `with_temperature()`, and `with_system_prompt()`. Used for wiring into `ChatService` in tests and non-Goose contexts.

### `pond-adapters-llamafile` — Llamafile / OpenAI-compat Provider

HTTP client for `POST /v1/chat/completions` — compatible with llamafile, LM Studio, and any OpenAI-format endpoint. Also usable with `OllamaProvider` by setting `OLLAMA_HOST`.

### `pond-adapters-local-inference` — In-process GGUF

Loads GGUF model weights directly into the process via `llama-cpp-2`. No server process required. First build takes 5–15 min (compiles llama.cpp). Enable with `--features local-inference`. For CUDA on Jetson: add `--features pond-adapters-local-inference/cuda`.

### `pond-adapters-mcp-memory` — Flat-file MCP Memory

`GooseMcpMemoryAdapter` implements `McpMemoryPort` using a flat JSONL file. Enable with `--features mcp-memory`. Memory fragments are injected into every system prompt turn.

### `pond-mcp-server` — GIAP as a Goose Builtin MCP Extension

Exposes GIAP's smart home capabilities to the Goose agent as 9 MCP tools under the `giap__` prefix. Implements `rmcp`'s `#[tool_router]` macro.

| Tool | Description |
|---|---|
| `giap__get_current_weather` | Weather via Open-Meteo |
| `giap__list_registered_devices` | Device registry |
| `giap__list_schedules` | Cron tasks |
| `giap__get_user_profile` | Name, timezone, location |
| `giap__get_model_assignments` | LLM role assignments |
| `giap__recall_memories` | Search memory fragments |
| `giap__save_memory` | Persist a memory fragment |
| `giap__get_recipe` | Fetch a Goose recipe YAML |
| `giap__list_skills` | Active user skills |

---

## pond-desktop — Native Desktop App

An [Electron](https://electronjs.org) application providing a native UI for macOS. It is not
packaged for Linux: on the Jetson the UI is the dashboard `pond-server` already serves over HTTP,
and that board runs headless by design (GNOME held 2.6 GB of nvmap, see
`docs/developer/jetson-device-tuning.md`).

| Directory | Purpose |
|---|---|
| `electron/main/` | Main process — window, `app://` protocol, menu, tray, hotkeys, IPC, the pond-server sidecar's lifecycle, and the voice child driver |
| `electron/preload/` | The contextBridge. Validates every channel against the contract's allowlists before forwarding |
| `electron/assets/` | Icons the main process loads at runtime (tray template + @2x, About panel). Shipped inside the asar — distinct from `build/`, which the packager reads and does not copy |
| `build/` | Packaging-time only: `icon.icns` and its PNG master. Generated by `scripts/make-icons.py` from the brand mark |
| `src/shell/` | The IPC contract (closed unions for commands and events) and the renderer's side of the bridge |
| `src/` | React 19 + TypeScript renderer |
| `src/styles/` | Jarida design tokens and base CSS (offline fonts via fontsource) |
| `src/state/` | `AppState` reducer + `AppContext` (the shell's non-voice event listeners), and `chatRunStore` — the live chat turn (see below) |
| `src/api/` | `PondApiClient` — single class for all REST calls |
| `src/modes/` | `GuiMode` (drawer-navigated app), `VoiceMode` (full-window orb) |
| `src/sections/` | 10 GUI sections: Dashboard, Chat, Devices, Schedules, Memory, Skills, Models, Prompts, Settings, Agent |
| `src/canvas/` | `CanvasOverlay` — frosted-glass floating window |
| `src/components/` | Shared: `VoiceOrb`, `TranscriptFeed`, `ContextCard`, `StartupScreen` |
| `src/hub/` | `Hub` shell, plus `HubDrawer` + `ShellBar` — the navigation BOTH shells render |

**Window behaviour.** The window hides to the tray on close rather than
quitting, and remembers its position between runs — but only restores it when
a display still overlaps those bounds, so moving it to a second monitor and
then unplugging that monitor cannot strand it off-screen. On a small panel
(<=1100x700, e.g. the 7-inch Jetson kiosk display) it opens borderless and
fullscreen instead, and no saved position applies.

**Three modes:**
- **GUI mode** — drawer navigation app (1280×820 window)
- **Voice mode** — full-window voice orb with transcript feed
- **Canvas mode** — always-on-top translucent overlay for ambient display

The desktop app starts `pond-server` automatically via the `ensure_server_running` IPC command,
polls `server_health`, and displays a branded startup screen while the server comes online. When
launched by `pond-server serve --native` the parent pins the port through `GIAP_SERVER_PORT`, and
the shell must then attach rather than spawn — two servers fighting for one port present as a
blank window, not as an error.

**`--native` does nothing visible if the app is already running.** The shell holds a
single-instance lock, so a second launch quits within a few hundred milliseconds and raises the
EXISTING window — which is still attached to whichever server started it, not the new one. The
server used to report "Desktop app started" and return, leaving a server with no window on it and
a log claiming success; it now waits briefly, notices the child exited, and says so. Quit the
running app before `--native`, or just open the new server's URL in a browser.

### Detached runs — a turn that outlives its connection

`crates/pond-api/src/runs.rs`. A turn used to *be* the SSE response body, which
made dropping the connection its cancellation — deliberately, via the
`DropGuard` inside `pond-adapters-goose`'s chat stream. It also meant a reload
killed the answer mid-sentence: the user's message is persisted before inference
starts and the assistant's only after the token loop drains, so the question was
stored and the answer nowhere.

A turn is now a task driving a `RunHandle`, and every SSE body — the original
POST and every reattach — is a *subscriber*: replay the handle's ring buffer,
then tail its broadcast. The same shape `notifications_stream` uses.

| Route | For |
|---|---|
| `GET /sessions/{id}/active-run` | Discovery. A restarted client knows its session id and nothing else, so this is the only way back in |
| `GET /chat/runs/{run_id}/events?after_seq=&epoch=` | Replay, then tail |
| `POST /chat/runs/{run_id}/cancel`, `DELETE /sessions/{id}/active-run` | Stopping on purpose |

Three things to know before changing any of it:

**`resumable` defaults to false, and that default is load-bearing.** A run is
`Ephemeral` unless asked otherwise, meaning the last subscriber leaving cancels
it — today's exact contract. `WebVoiceBackend` fires a *speculative*
`/chat/stream` the moment trailing silence begins, before the pause is
confirmed, and aborts it when speech resumes. Detaching unconditionally would
let that speculative turn finish and persist a question-and-answer pair for a
half-sentence nobody finished saying. **The voice path must call the cancel
endpoint before that default can change.** It has not been rewired yet; until it
is, barge-in works because voice turns are still ephemeral.

**A cancelled turn keeps what it streamed, not what it was about to.** The
thought filter holds short text back until a turn ends, so a very short answer
cancelled early has said nothing — and a turn that said nothing has its
now-unanswerable user message removed rather than left dangling.

**The cap counts turns in flight, not history.** Finished runs stay reattachable
for a retention window, and a new turn supersedes the finished one on its own
session. Counting retained runs would make an ordinary ten-message conversation
start refusing turns halfway through.

### The chat turn is owned by the module, not the view

`GuiMode` picks a section with a `switch`, not a router, so picking anything in
the drawer **unmounts the section that was showing**. A chat turn cannot live in
that component: leaving Chat mid-answer would throw away the transcript, the
queued follow-ups and the streaming bubble, while the stream itself kept running
and decoded its tokens into state updates on a dead component, which React drops
silently. The answer arrived, was persisted, and was invisible to whoever asked.

So the turn lives in `src/state/chatRunStore.ts` — a module singleton read through
`useSyncExternalStore`, the same shape `hub/state/hubDataStore.ts` uses for
anything that must outlive a view. It owns the transcript, the busy flag, the
queue and its draining, and the loop that folds the `/chat/stream` frames.

| Concern | Owner |
|---|---|
| Transcript, streaming bubble, busy, queue, active session id | `chatRunStore` |
| Composer draft, attachment tray, which screen the section is on | The section |
| `sessionId`, `sessionToken`, `serverOnline`, context cards | `AppContext` |

Two surfaces subscribe — `sections/Chat.tsx` and `hub/views/ChatHub.tsx` — and
they render **one** conversation rather than keeping one each; the Hub draws a
projection of the shared message, which is what stops the two drifting over what
a frame means. Being at module scope, the driver cannot read app state or
dispatch, so `AppContextProvider` installs a small typed bridge
(`setChatRunBridge`) carrying the auth token and the three callbacks the turn
needs on the way back. It is installed there for the same reason the schedule
SSE listener is: a turn started in Chat is still arriving while you are looking
at Devices.

Returning to Chat lands on the "All chats" wall as it always has, with one
carve-out: a turn still running, or one that finished while nothing was mounted
to show it, opens straight into its thread and is marked read once shown.

**Across a reload, too.** A chat turn is sent with `resumable: true`, asking the
server to keep driving it when the connection drops — see *Detached runs* below.
The store writes a small pointer to `localStorage` (session, run, server epoch,
and how far it had read) the moment the turn names itself, and `resumeActiveRun`
uses it on the next start: ask the server what it is still driving, load the
session's persisted messages for the transcript, then reattach to the run and
tail the rest. No conversation content is kept in the browser — only enough to
ask the question.

**Boundary.** A *server* restart still loses the run. Everything a turn needs
lives in memory — the agent's state, its cancellation token, its authority
lease, its device claim — so the epoch in the pointer will not match, the client
is told `410 run_lost`, and it falls back to the persisted messages. That is the
honest answer rather than a recoverable one.
