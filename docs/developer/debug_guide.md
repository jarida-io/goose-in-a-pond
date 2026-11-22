# Debug Guide

Where to look when the pond misbehaves, from the fastest check to the deepest.

## First checks

```bash
bash scripts/giap.sh doctor          # read-only health report; exits 1 on any FAIL
cargo run -p pond-server -- status   # version, data dir, provider/model, wake word, onboarding step
```

`giap.sh doctor` covers the goose submodule pin, whether the web UI in `pond-desktop/dist` is
built or the `build.rs` placeholder, stray `target/debug/pond-server` binaries, CUDA on a Jetson,
duplicate service units and server processes, disk and memory headroom, the three Node ranges,
the secret store, settings hygiene and remote access. Fix what it reports before anything else.

The server picks its own ports. Read them instead of assuming 4000:

```bash
DATA="$HOME/Library/Application Support/goose-in-a-pond"   # Linux: ~/.local/share/goose-in-a-pond
PORT=$(cat "$DATA/.runtime_api_port")                       # HTTP, 127.0.0.1, 4000..4009
cat "$DATA/.runtime_https_port"                             # HTTPS, all interfaces, from 4443
curl "http://127.0.0.1:$PORT/api/v1/health"                 # {"status":"ok","version":...} on loopback
```

`POND_DATA_DIR` moves the whole data directory, which is how `scripts/live-test.sh` keeps a
scratch pond away from a real one.

## Logs

| Sink | Where | What |
|---|---|---|
| Console | stdout (stderr under `chat --json-events`) | As filtered below; `chat` shows only WARN and above |
| Files | `<data_dir>/logs/pond.log.YYYY-MM-DD` | Daily, UTC timestamps, never pruned |
| Event log | `event_log` in `pond_logs.db`; `GET /api/v1/logs?level=WARN` | WARN and above from anywhere, plus INFO on `giap::trace*` targets |

Set up in `crates/pond-server/src/tracing_setup.rs`. With no `RUST_LOG`, the filter is `info`,
with `debug` for `pond_server`, `pond_core::shared::services::chat` and `pond_adapters_whisper`,
and goose, llama.cpp, ggml, whisper.cpp, ONNX Runtime and rmcp muted to `error`/`warn`.
`serve --debug` turns everything to `debug` except those noisy crates. `RUST_LOG` replaces both
filters, for console and file:

```bash
RUST_LOG=info,pond_adapters_goose=debug cargo run -p pond-server -- serve
```

`bash scripts/giap.sh logs` tails today's file and has canned greps for errors, turn telemetry
(`turn_end`) and the engine/context lines. A systemd unit logs to these files, not to journald.

Structured events use explicit targets (`giap::trace`, `giap::kv`, `giap::vision`, `giap::runs`),
so a crate-level filter cannot hide them. Every settings, extension and schedule change is one
such line (`kind = "setting_changed"`, `"extension_changed"` or `"schedule_changed"`); see
`docs/architecture/settings-catalogue.md` section 5.5. Per-turn telemetry is also queryable:
`GET /api/v1/telemetry/turns` and `GET /api/v1/telemetry/summary`.

## Calling the API from a terminal

Every route outside the public allowlist (`PUBLIC_ROUTES` in
`crates/pond-api/src/middleware/mod.rs`) needs a bearer token from pairing.

| Status | Meaning | Fix |
|---|---|---|
| `401` | No token, or an unknown one | Pair, or run the server with `POND_DEV_ALLOW_LOOPBACK=1` (development only: any local process then reaches every protected route) |
| `403 onboarding_required` | Onboarding is not `Completed` | Finish onboarding; see the [onboarding guide](../Onboarding_Doc/Onboarding_guide.md) |
| `429` | Rate limit: 600 requests/min per remote IP, 30/min on `/handshake*`; loopback is exempt | Slow down, or call from the host |

`pond-server dashboard` prints a one-time sign-in link for a browser on the host.

```bash
POND_DEV_ALLOW_LOOPBACK=1 cargo run -p pond-server -- serve
curl -X POST "http://127.0.0.1:$PORT/api/v1/chat" \
  -H 'Content-Type: application/json' -d '{"message":"hello"}'
curl "http://127.0.0.1:$PORT/api/v1/sessions"
```

## What the model is sent

| Lever | Effect |
|---|---|
| `GIAP_CAPTURE_PAYLOAD=<dir>` | `provider_shim.rs` writes every provider call as an OpenAI chat body (system prompt, messages, tools) into `<dir>` |
| `GIAP_NO_TOOLS=1` | No extensions are spawned and no tools are offered; isolates prompt problems from tool problems |
| `pond-server agent tools`, `GET /api/v1/agent/tools` | The MCP tools the agent currently knows |
| `pond-server agent chat "message"` | One turn through the Goose loop, streamed to the terminal |
| `pond-server prompts show balanced` | The stored template a turn starts from; see [prompt_system.md](prompt_system.md) |

`GIAP_DUMP_PROMPT` exists only in `pond-inference`, which feeds the quarantined `pond-agent`
loop, so it shows nothing on the live Goose path.

## Voice

`pond-server chat --voice` turns on the whole stack (wake word, speech detection, Whisper,
Kokoro) and fetches any part of it not yet on disk. Whisper runs in-process through `whisper-rs`;
there is no ASR server to start.

The desktop runs the same thing as a child process with NDJSON on stdout. Reproduce it exactly:

```bash
cargo run -p pond-server -- chat --voice --json-events
```

A healthy start prints `{"event":"ready",...}` then `{"event":"state","state":"wait"}`. The other
events are `warmup`, `transcript`, `token`, `tool_call`, `tool_result`, `turn_complete`,
`audio_level`, `error` and `exit`; states run `wait`, `listen`, `thinking`, `speak`. Kill the
probe when done: it holds the microphone and speaker. A staged desktop sidecar older than the
database's migrations dies before printing anything; restage with
`bash scripts/stage-server-sidecar.sh`. Startup diagnostics in this mode arrive as `error` events
or in the stderr tail, never on stdout. More in
[voice-desktop-child-process.md](voice-desktop-child-process.md) and [voice-mode.md](voice-mode.md).

## Model providers

The chat model is chosen in the app (Models page) or `PUT /api/v1/settings` (`chat_provider`,
`chat_model`). `local` runs in-process through Goose: llama.cpp for a `.gguf`, LiteRT-LM for a
`.litertlm` (`bash scripts/giap.sh litert status` checks the library).

```bash
cargo run -p pond-server -- models list --category gguf
curl http://127.0.0.1:11434/api/tags          # Ollama reachable? Then: ollama pull <model>
```

Model files live under `<data_dir>/models/` by category (`crates/pond-core/src/models/domain/model_layout.rs`):
`gguf/`, `litertlm/`, `llm/` (llamafile), `kokoro/voices/`, `embedding/`, and Whisper GGML
files at the top level.

## Databases

| File | Holds |
|---|---|
| `pond_system.db` | Settings (one row per key), sessions and messages, memory, profiles, devices, models, prompts, skills, onboarding |
| `pond_logs.db` | `event_log`, `turn_metrics`, sensor readings, camera events |
| `pond_vectors.db` | The derived personal-context vector index; rebuildable from `pond_system.db` |

```bash
sqlite3 "$DATA/pond_system.db" "SELECT key, value FROM settings ORDER BY key;"
sqlite3 "$DATA/pond_system.db" "SELECT current_step FROM onboarding_state;"
sqlite3 "$DATA/pond_system.db" "SELECT id, title, updated_at FROM sessions ORDER BY updated_at DESC LIMIT 10;"
sqlite3 "$DATA/pond_system.db" "SELECT version, description FROM _sqlx_migrations ORDER BY version DESC LIMIT 5;"
```

Migrations live in `crates/pond-infra/migrations/{system,logs,vectors}/` and are embedded at
compile time by `sqlx::migrate!`, then applied by `Database::init` on every start. Never edit
one that has shipped; add a new file. A binary older than the database fails at startup with
`migration N was previously applied but is missing in the resolved migrations`: rebuild it.

## Build and test

- A full disk reads like a miscompile (`ld: write() failed, errno=28`). Check `du -sh target`
  and clear `target/debug/incremental` first.
- `cargo check` does not compile `#[cfg(test)]` code; run `cargo test` before trusting a
  removal.
- One test: `cargo test -p <crate> <name_substring>`; with output: add `-- --nocapture`.
- Live-hardware tests are `#[ignore]`d: `cargo test -p <crate> -- --ignored`, with the env var
  each one names.
- Startup wiring, routes and migrations are only proven by `scripts/live-test.sh`; see the
  [testing guide](../testing/tdd_guide.md).
