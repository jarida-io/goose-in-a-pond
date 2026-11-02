# Components

Every workspace crate, plus the apps and services that live beside the workspace. Purposes are
taken from each crate's `lib.rs` module doc and `Cargo.toml`; the status column says whether a
default `pond-server` build links the crate and whether the serving path uses it.

How the pieces fit together is in [overview.md](./overview.md); one chat turn through them is in
[data_flow.md](./data_flow.md).

## Status legend

| Status | Meaning |
|---|---|
| **live** | Linked by a default `pond-server` build (features `goose-agent`, `local-inference`) and used at runtime |
| **live, gated by a setting** | Linked by default; does nothing until a setting turns it on |
| **feature `x`** | Compiled only with `cargo build -p pond-server --features x`; off by default |
| **quarantined** | Compiles behind a feature but cannot be activated (Q2-05) |
| **standalone** | Not a dependency of `pond-server`; has its own binary or consumer |

Release builds add features on top of the default: the macOS desktop sidecar is built with
`--features mesh` (`scripts/stage-server-sidecar.sh`), and the Jetson build adds
`pond-adapters-local-inference/cuda,pond-adapters-whisper/cuda` (`server_features` in
`scripts/giap.sh`, `scripts/jetson/deploy.sh`).

## Workspace crates (37)

### Composition, transport and core

| Crate | Purpose | Status |
|---|---|---|
| `pond-server` | The binary. CLI (`setup`, `serve`, `chat`, `status`, `onboard`, `models`, `agent`, `prompts`, `skills`, `recipes`, `memories`, `calibrate`, `pairing`, `setup-code`, `dashboard`), composition root that wires every adapter into `AppState`, the HTTP and HTTPS listeners, background loops (pruning, inference lane, account sync, consolidation), and the voice child (`chat --voice --json-events`) | live |
| `pond-api` | Axum router under `/api/v1/`, DTOs and `AppState`; CORS, rate-limit, Bearer-auth and onboarding middleware; detached chat runs (`runs.rs`); the thought filter; embeds `pond-desktop/dist` with `include_dir!` | live |
| `pond-core` | Pure domain: ports (traits), services and domain types, by bounded context (see below). No web, database, HTTP-client or agent-framework dependency | live |
| `pond-infra` | SQLite via SQLx for `pond_system.db`, `pond_logs.db` and the derived `pond_vectors.db`; also the encrypted file secret store, fastembed embedder, CPace pairing, TTL pruning, push relays | live |
| `pond-infra-scheduler` | `CronSchedulerAdapter` (`tokio-cron-scheduler`) for `SchedulerPort`, the sensor rules engine, JSON run history | live |
| `pond-net` | Every outbound HTTP client and its DNS policy; `public_only` builds the client for third-party-chosen URLs. `crates/pond-core/tests/egress_guard.rs` fails the build if a crate builds a `reqwest` client elsewhere | live |
| `pond-hf-cache` | Model-download cache in `huggingface_hub`'s on-disk layout | live |
| `pond-connect` | How a pond is found and reached: the BLE setup channel, advertising, Wi-Fi station and wired interfaces (mechanism; policy is `pond_core::network`) | live |

### Agent and inference

| Crate | Purpose | Status |
|---|---|---|
| `pond-adapters-goose` | `GooseAdapter`, the live `Agent`: wraps Block's Goose (submodule `goose/`), builds the per-turn prompt, selects tool groups, runs Goose's loop. Also `GiapProviderShim` (last-mile system-prompt and tool veto), `GiapGooseExtensionManager`, `register_giap_extensions`, `GooseOrchestrator` (child agents), LiteRT-LM model rows, vision-encoder and MTP-drafter attachment, Goose token counter | live |
| `pond-mcp-server` | GIAP's capabilities as 12 Goose builtin MCP extensions (`giap-toolkit`, `-memory`, `-schedule`, `-weather`, `-knowledge`, `-system`, `-device`, `-device-control`, `-sensors`, `-travel`, `-context`, `-orchestrator`), 37 `#[tool]` functions; each family is gated by an `ext_*_enabled` setting. See [mcp-compliance.md](./mcp-compliance.md) | live |
| `pond-adapters-local-inference` | `LocalInferenceLlmAdapter` (the `LlmProvider` used for side calls such as titles and consolidation when the chat provider is local), `ResourceAwareModelScheduler`, and `ToolCallerEngine` (optional parameter specialist, loaded only when `settings.tool_model` is set) | live (feature `local-inference`, on by default) |
| `pond-inference` | llama.cpp engine without Goose. Live part: `GgufEmbeddingProvider` (`embedding_provider = "gguf"`). `LlamaCppEngine` feeds only the quarantined `pond-agent` | live (embedder only) |
| `pond-adapters-ollama` | `LlmProvider` over Ollama `/api/chat`; pulls a missing model | live (side calls when `chat_provider = "ollama"`) |
| `pond-adapters-llamafile` | `LlmProvider` over an OpenAI-compatible `/v1/chat/completions` (llamafile, mistral.rs server) | live (side-call fallback) |
| `pond-adapters-mistralrs` | Goose-free `MistralRsAgent` and `MistralRsProvider` to a mistral.rs server; Mac-only measurement path (`agent_backend = "mistralrs"`) | feature `mistralrs-agent` |
| `pond-agent` | GIAP's own agent loop over an inference provider | quarantined (feature `pond-agent`; `agent_backend = "pond"` is rejected with 422 and rewritten to `goose` at startup) |

### Voice and audio

| Crate | Purpose | Status |
|---|---|---|
| `pond-voice` | Leaf crate of voice primitives: DSP, barge-in, text splitting, turn and control types | live |
| `pond-audio` | The single microphone owner (cpal): one input device, many subscribers; `MicEnergy` for barge-in | live |
| `pond-audio-out` | Speaker side for every TTS engine: output device, playback, the working tone | live |
| `pond-adapters-audio-devices` | Finds, tests and watches microphones and speakers (cpal) for pond-core's audio-device policy | live |
| `pond-adapters-whisper` | In-process ASR with `whisper-rs`: `WhisperRsInput` (voice input) and `WhisperKeywordDetector` (wake word) | live |
| `pond-adapters-kokoro` | Kokoro-82M TTS (`KokoroOutput`) via `ort`; the only TTS engine | live |
| `pond-adapters-silero` | Silero VAD (ONNX) as a `SpeechDetector` for end of speech; `vad_backend = "rms"` falls back to the energy gate | live |
| `pond-ear` | Std-only wake word, voice activity, Voice Match and echo cancellation on one front end, with its own `pond-ear` binary. See [pond-ear.md](../developer/pond-ear.md) | standalone (no crate depends on it) |

### Devices, sensing and context sources

| Crate | Purpose | Status |
|---|---|---|
| `pond-adapters-matter` | `DeviceControlPort` over GIAP's own `matter-server/` (matter.js) on the loopback `giap-matter` WebSocket; installs and spawns that server under `<data_dir>/matter-server/app/` | live |
| `pond-adapters-vision` | camera capture (a Jetson's NVDEC, NVJPG and VIC through gst-launch, including VIC's 416-wide detector picture at 5 fps, else the camera's own JPEGs or full decoding through ffmpeg), motion differencing, and person tracks that raise one event per person, into `camera_events` and the EventBus; the `VisionSupervisor` that keeps capture matched to settings and camera consent; the live-frame registry behind the camera routes | live, gated by settings (`cameras_enabled` and `vision_enabled`, plus a configured or discovered camera) |
| `pond-adapters-vision-onnx` | YOLOX-Nano: where people, pets and packages are, for the person tracks and motion labels | feature `vision-onnx` |
| `pond-adapters-face-onnx` | ONNX face detection and embedding (`FaceEmbeddingExtractor`) | feature `face-onnx` |
| `pond-adapters-weather` | `OpenMeteoWeatherAdapter` and `Geocoder` (keyless Open-Meteo, 15-minute cache); defines the `WeatherProvider` trait itself | live |
| `pond-adapters-caldav` | Read-only CalDAV context source for the personal-context index | live, gated by a connected account |
| `pond-adapters-imap` | Read-only IMAP context source (implicit TLS on 993 only) | live, gated by a connected account |
| `pond-adapters-uber` | Uber Riders API as a `RideProvider` (fares, requests, status, cancellation) on a member's own account; switched on in `pond-server/src/ride_booking.rs`, routes in `pond-api/src/rides.rs`. See [rides-and-maps.md](./rides-and-maps.md) | live, gated by a connected Uber account |
| `pond-adapters-mcp-memory` | `McpKnowledgePort` over Goose `MemoryServer`'s flat-file format | feature `mcp-memory` |

### Private mesh

| Crate | Purpose | Status |
|---|---|---|
| `pond-mesh-protocol` | Mesh wire protocol: hashing, identity, messages; no tokio, sqlx, reqwest, goose or rmcp | feature `mesh` |
| `pond-adapters-mesh-libp2p` | libp2p `MeshTransport` (TCP, Noise, Yamux, Kademlia, GossipSub, Relay v2, DCUtR) | feature `mesh`, then `settings.mesh_enabled` |
| `pond-adapters-mesh-inference` | Borrow and lend inference over the mesh (`MeshInferenceService`) | feature `mesh`, then `settings.mesh_enabled` |
| `pond-adapters-lightning` | Lightning settlement via `breez-sdk-spark` | feature `lightning`, then `settings.lightning_enabled` |

## `pond-core` layout

| Module | Holds |
|---|---|
| `user_data/` | Household-owned data and the services over it: profile, sessions, settings (`Settings`, `settings_validation`), memory (extraction, consolidation, relevance, cleanup), skills, recipes, prompt templates and extras, schedules and reminders, sensors, devices, faces, drafts, proposals, suggestions, onboarding, the inference lane |
| `models/` | Inference-facing ports and services: `Agent`, `LlmProvider`, `InferenceProvider`, `EmbeddingProvider`, `TokenCounter`, `AnswerReviewer`, voice input and output, wake word; model records, capabilities, device profile, vision encoder; `services/context/` (context budget and governor, turn trimmer, prefix cache, answer contract, memory block) |
| `mcp/` | The tool surface: extension manager and marketplace ports, tool registry, tool selection, `ToolCaller`, notifications, web-search status |
| `security/` | Secrets, handshake, audit and event log, policy, redaction, telemetry and `TurnMetrics`, draft authority |
| `shared/` | `ChatService` (`services/chat.rs`), agent request and stream types, turn stats, event bus, egress gate, delegation and turn authority, session titles and summaries |
| `context/` | Personal-context index: ingest, chunking, retention, retrieval, vector index port |
| `mesh/`, `network/`, `rides/` | Mesh policy, pond discovery and claiming policy, ride ordering; adapters live in the crates above |
| `prompts.rs`, `system_prompts_template.md` | Built-in personas and system-prompt assembly |

Each module keeps the `domain/`, `ports/`, `services/` (and `mocks/`, behind the `test-mocks`
feature) split. `pond-core` depends on `pond-voice`, tokio, serde, tera and similar libraries,
never on sqlx, axum, reqwest or goose.

## Outside the Cargo workspace

| Path | What it is | Relationship to the pond |
|---|---|---|
| `pond-desktop/` | Electron shell, macOS only, all TypeScript. `electron/main/` is the main process (window, `app://giap` protocol, tray, hotkeys, IPC, the `pond-server` sidecar, the voice child driver `voice/VoiceChildProcess.ts`); `electron/preload/` validates every channel; `src/` is the React renderer (`modes/`, `sections/`, `hub/`, `state/chatRunStore.ts`, `settings/`, `api/PondApiClient.ts`); `src/shell/contract.ts` is the IPC contract | Its built `dist/` is also embedded in `pond-api` and served to the Jetson dashboard and phones |
| `native/pondnet/` | Go module: the embedded userspace networking helper (Headscale tailnet node, authenticated CONNECT proxy), mobile bindings, `pond-enrollment` and `pond-provision` commands. See its [README](../../native/pondnet/README.md) and [pond-networking.md](./pond-networking.md) | Run by `pond-server` (`embedded_network.rs`) for remote access |
| `matter-server/` | Node/TypeScript Matter controller (matter.js) behind the `giap-matter` WebSocket protocol ([matter-protocol.md](../matter-protocol.md)) | Installed and spawned by `pond-adapters-matter`, loopback only |
| `services/pondcredentials/` | Its own Cargo workspace: `pondcredentials` (HTTP service that signs Apple Music developer tokens) and `pond-apple-token` (shared signer). See [pondcredentials.md](./pondcredentials.md) | `pond-api` depends on `pond-apple-token` by path |
| `extensions/music/` | TypeScript stdio MCP extension that plays Apple Music on a Mac. See its [README](../../extensions/music/README.md) and [music-player.md](./music-player.md) | Installed from the extension marketplace; Goose launches it |
| `goose/` | Submodule pinned to `jarida-io/Goose:main` (upstream plus GIAP's patch set, [goose-patch-management.md](../goose-patch-management.md)) | Built into `pond-adapters-goose`; Goose keeps its own state under `<data_dir>/engine/` |
