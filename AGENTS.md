# AGENTS.md

Instructions for AI coding agents working in this repository. Build, test and commit conventions are in [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md); follow it.

Where things are, what is expensive to build, and the known traps: [HOWTOAI.md](HOWTOAI.md). Work on any
user-facing surface starts at [DESIGN.md](DESIGN.md); work on PAI-1 to PAI-8 starts and ends with
[docs/architecture/pai/00-checklist.md](docs/architecture/pai/00-checklist.md).

## Commits and pull requests

- Don't credit yourself. No `Co-Authored-By:` trailer for an AI tool or model, no "Generated with …" line and no session link, in commits, pull request descriptions or review comments. Human co-authors are fine.
- Follow the commit format in [docs/CONTRIBUTING.md § Commit Messages](docs/CONTRIBUTING.md#commit-messages).

## Comments

The full rules are in [docs/CONTRIBUTING.md § Comments](docs/CONTRIBUTING.md#comments). In short:

- Default to no comment. Add one only for what the code can't show: a constraint, an invariant, a non-obvious reason, or a quirk of a dependency.
- One line where possible. Don't write a paragraph where a line will do.
- No history: no "used to", dates, commit hashes, PR or issue numbers, phase or workstream tags, or incident write-ups. Put those in the commit message or the pull request.
- Don't narrate the code or your reasoning, and don't leave notes for the reviewer ("fixed X", "as requested", "new:").
- When you change code, fix or delete the comments it makes wrong.
- Leave these alone unless you mean to change what a program reads: clap doc comments (the `--help` text), `JsonSchema` field docs in `pond-mcp-server` (tool descriptions the model sees), directives such as `// SAFETY:`, `@ts-expect-error` and `eslint-disable`, and code blocks in doc comments (doctests).

Some tests read source files as text (`*_is_wired.rs`, `*_wiring.rs`, `egress_guard.rs`, `stream_handler_parity.rs`). After editing comments, run the tests of the crates you touched.

## Commands

The full list, mirroring `.github/workflows/ci.yml`, is [CONTRIBUTING § Checks](docs/CONTRIBUTING.md#checks).

```bash
bash scripts/giap.sh check                  # the CI checks for the areas this branch changes (--all, --dry-run)
cargo test -p <crate> <name>                # one test, by substring
scripts/live-test.sh [--no-build] [--keep]  # the real binary over HTTP; required for migrations, routes, handlers, startup wiring

cargo run -p pond-server -- serve [--port N] [--native]   # API on 127.0.0.1:4000; --native also opens the desktop app (macOS)
cargo run -p pond-server -- chat --voice [--json-events]  # the voice loop; the desktop runs it with --json-events
cargo run -p pond-server -- --help                        # setup, status, models, memories, pairing, setup-code, ...

cd pond-desktop
npm run dev           # Vite on :1420 plus `pond-server serve --native`
npm run dev:vite      # renderer only; panels that need the server show their error state
npm run dev:electron  # Vite, tsup --watch and the Electron shell
npm run stage:server  # build a release pond-server and stage it as the sidecar (slow)
npm run bundle:app    # stage the pond-server sidecar, verify it, package the .app and .dmg
npm run pack:dir      # the same, unpacked: quicker when checking the bundle's shape

bash scripts/jetson-emu.sh doctor|run|test|box  # take the Orin's decisions on a Mac, never its performance
```

`jetson-emu.sh` sets `POND_DEVICE_PROFILE` (`orin-nano-8gb`, `orin-nano-8gb-cpu`, `orin-nx-16gb`;
unset emulates nothing). `run` and `test` reproduce the board's choices, such as the context size from
`context_size_for_budget`, but cannot hold the process to the board's 7,620 MB: macOS has no cgroups.
`box` adds aarch64 codegen and enforces that ceiling in a container. No tier has CUDA or yields a
KV-cost or throughput number.

## How GIAP fits together

### A chat turn

1. `POST /api/v1/chat/stream` (`chat_stream` in `crates/pond-api/src/routes.rs`) refuses with `409`
   before saving anything when no model is chosen (`refuse_without_a_model`; `chat_provider` starts
   empty). `ChatService` saves the user message before inference and the assistant turn after it.
2. `GooseAdapter::chat_stream` (`crates/pond-adapters-goose/src/goose_agent.rs`) picks the provider in
   `ensure_provider_current` from `chat_provider`: `local`/`gguf` in process, `llamafile`, `ollama`,
   `mistralrs` (an OpenAI-compatible server) or `mesh`. Every provider is wrapped in
   `GiapProviderShim`, which enforces GIAP's system prompt and tool veto.
3. In process, `goose-local-inference` picks the backend from the model registry entry's
   `backend_id`: llama.cpp by default, LiteRT-LM for `.litertlm` models. MLX exists in Goose behind
   a feature GIAP does not enable.
4. Memories and personal context ride the turn's user message in `<system-context>`, never the
   system prompt, so the system prefix and its KV cache stay reusable across turns.
5. Tools are GIAP's MCP servers, run in process as Goose builtin extensions over an in-memory duplex
   (`pond_mcp_server::spawn_*_server`), not as subprocesses.

### Background work: the inference lane

One slot (`crates/pond-server/src/inference_lane_runner.rs`; policy in
`crates/pond-core/src/user_data/services/inference_lane.rs`) runs compaction, consolidation,
titling, proactive review, summary refresh, memory extraction and suggestion generation. Nothing is
pre-empted mid-call: no job starts while a turn is in flight, user activity cancels consolidation,
and a quiet compaction of the session a turn arrives on makes that turn wait (`QuietPasses`). The
embedding slot (`crates/pond-server/src/embedding_slot_runner.rs`, pass in
`crates/pond-core/src/context/embedding_slot.rs`) is not a lane job: it loads the Memory-role
embedder and re-embeds stale rows beside a chore, never beside a turn, a compaction or a due
long-task step (`embedding_may_run`), and yields between batches. Memory extraction is a lane job (`BatchExtractionService` in
`user_data/services/memory_extraction.rs`), not part of a turn; the only in-turn saves are the
model's own `giap-memory` tool calls.

### Engines that are not the live path

`pond-agent` is quarantined: `agent_backend = "pond"` is refused with `422` and rewritten to
`goose` at startup, and its feature is off by default. `pond-inference` serves the live pond only
as the GGUF embedder (`embedding_provider = "gguf"`; the default is `fastembed`).

### State

`<data_dir>` is `POND_DATA_DIR`, else the OS data directory plus `goose-in-a-pond`. It holds
`pond_system.db` (all chat history), `pond_logs.db` and `pond_vectors.db` (opened after the system
database, which it attaches), plus Goose's own state under `engine/`, which the API never reads. The
running server writes `.runtime_api_port` and `.runtime_https_port` there.

### Startup order in `crates/pond-server/src/main.rs`

- `pin_goose_state_under()` runs first, before anything touches Goose's session storage.
- The network mode, ONNX Runtime and the LiteRT library directory are settled before any provider
  is built.
- `register_giap_extensions` runs before any agent session, and each MCP module's `init_*_deps`
  runs in every binary that registers it (see HOWTOAI's MCP trap).
- `serve` stops on Ctrl-C or SIGTERM, which is what `systemctl stop` and `docker stop` send.
  `release_local_models` runs before exit; LiteRT's exit-time destructors crash otherwise.

### MCP servers

`crates/pond-adapters-goose/src/giap_registration.rs` registers them once at startup. The toolkit
and vision servers are always on. Memory, schedule, weather, knowledge (web search lives here),
system, device and sensors are on by default; travel, context and orchestrator are off. Each
`ext_*_enabled` setting gates its server, and `GIAP_NO_TOOLS` registers none.

### Phones, pairing and the network

Phones (the Goose On The Go app) pair with CPace (`/handshake/pake/*`) using a pairing code, a
spoken code or the printed setup card, then talk HTTPS on `:4443` against a pinned self-signed
certificate. `auth_middleware` (`crates/pond-api/src/middleware/mod.rs`) guards every non-public
route. `POND_DEV_ALLOW_LOOPBACK=1` lets loopback callers through for development;
`live-test.sh` re-runs its auth checks with it off. `pond-net` owns every outbound HTTP client,
`pond-connect` the BLE setup channel, Wi-Fi and mDNS, `native/pondnet` (Go) the embedded tailnet
node for remote access, and the `mesh` feature pond-to-pond compute.

### The desktop app

Electron and TypeScript (`pond-desktop/`): `electron/` is the main process and preload, `src/` the
renderer. It is packaged for macOS; the Jetson kiosk runs the same app unpackaged.

- The renderer loads from `app://giap`, not `file://`: `dist/index.html` uses absolute `/assets/`
  paths and the server's CORS allowlist names that origin. Don't switch Vite to `base: './'`; the
  same `dist` is embedded into `pond-server` (`include_dir!`) and served to browsers and phones.
- `src/shell/contract.ts` is the IPC contract both sides import. Commands and events are closed
  unions with runtime allowlists, and the preload refuses any other channel.
- The shell finds its server in `electron/main/serverProcess.ts`: `POND_SERVER_BIN`, the bundled
  sidecar, then `target/release` and `target/debug`. It attaches to a healthy server on `:4000`
  before spawning one. The sidecar in `pond-desktop/resources/` is gitignored; restage it with
  `bash scripts/stage-server-sidecar.sh` after a migration change.
- Icons come from `src/assets/goose-logo.png` via `pond-desktop/scripts/make-icons.py`. Runtime
  assets (tray, About) belong in `electron/assets/`, listed in `electron-builder.yml`; `build/` is
  packaging-only, and a runtime asset placed there works in dev and ships missing.

### Voice

Serve runs room voice itself (`crates/pond-server/src/room_voice.rs`): the voice loop on the pond's
own microphone and speaker, with serve's agent, Whisper and Kokoro, wherever the models are there, a
microphone and speaker pair works and no `chat --voice` holds the voice lock (`GET /voice/room` says
which). `voice_room_enabled` switches it.

In the shell: `VoiceMode` → `useVoiceSession` (the only `voice-*` listener) → `start_voice_session`
→ `VoiceChildProcess` (`electron/main/voice/`). Where `GET /voice/room` says `available`, it attaches:
the room's SSE lines become the same `voice-*` events, and start, stop and interrupt become
`PUT /voice/room` and `POST /voice/room/interrupt`, with the renderer's own token. Otherwise it spawns
`pond-server chat --voice --json-events`, and the child owns the microphone and speaker. It writes NDJSON events (`warmup` lines may precede
`ready`, then `state`, `transcript`, `token`, ...) and reads `{"cmd":"interrupt"}` and
`{"cmd":"stop"}` on stdin. A second voice process exits with `already_running`, so a child you start
by hand to diagnose the shell keeps the microphone, and the desktop's voice, until you kill it.

- `VoiceChildProcess` keeps two guards that encode real races. A close from a killed session never
  reaps a newer one: each callback holds its own session object, not a flag. `start` and `stop` are
  serialised, or an overlapping pair spawns two children on the mic. It listens for `close`, not
  `exit`, so the last stderr line, usually the fatal one, is in the tail.
- `out!` prints nothing under `--json-events`. Startup problems reach the shell as NDJSON `error`
  events through `deferred_diagnostics`, or in the stderr tail attached to `voice-session-ended`.
- `isDesktopShell()` (`src/shell/index.ts`) is true only when the preload bridge `window.giap`
  exists. Everywhere else (a browser, the dashboard on phones and the LAN, the kiosk's browser
  fallback) `useVoicePipeline` drives `WebVoiceBackend` over `/api/v1/transcribe`,
  `/api/v1/chat/stream` and `/api/v1/tts`. A check that wrongly returns false drops the desktop onto
  that path and still appears to work.

A turn, where Silero is available (`crates/pond-server/src/voice_rig.rs` picks): the ear
(`pond-adapters-ear` running `pond-ear` on its own thread, fed the mic and, through
`pond_audio_out::tap`, everything played) hears the wake word, either its "hey goose" CNN or
Whisper on each speech segment, and endpoints the request with Silero on echo-cancelled audio →
Whisper transcribes it → think → tokens split into sentences (a short first clause, and clauses
when synthesis falls behind) and spoken by Kokoro while the reply
streams (`pond-adapters-kokoro`, `pond-audio-out`) → the wake word, or an enrolled household voice,
interrupts the reply (`InterruptWatch`) → a follow-up listen without the wake word, only when the
reply ended with a question (`asks_for_an_answer`). With
`vad_backend = "rms"` or no Silero, the older path runs: `WhisperKeywordDetector` on a 1.4 s window,
RMS gates, and barge-in on microphone energy (`pond_voice::barge`). `pond-audio` is the single
microphone owner and `pond-audio-out`'s audio bus the single speaker owner: one stream, one sound at
a time, each holding the bus's `Floor` from one queue, and its state published. Voice Match is
built in `pond-ear` but nothing enrols anyone yet.
