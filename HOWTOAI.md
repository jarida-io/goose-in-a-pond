# HOWTOAI.md

For coding agents working in this repository: where things are, what is expensive, and the traps
that have cost real time. The rules for commits and comments are in [AGENTS.md](AGENTS.md).

## 1. Read first

| When the work touches | Read |
|---|---|
| Anything | [AGENTS.md](AGENTS.md), then this file |
| A screen, a spoken reply, or any user-facing surface | [DESIGN.md](DESIGN.md) |
| Any of PAI-1 to PAI-8 | [docs/architecture/pai/00-checklist.md](docs/architecture/pai/00-checklist.md), at the start and the end |
| The Goose submodule | [docs/goose-patch-management.md](docs/goose-patch-management.md) |
| Anything else | The index in [docs/README.md](docs/README.md) |

Docs are claims; the code and `.github/workflows/ci.yml` are the evidence. When they disagree, fix the doc.

## 2. The front door

```bash
bash scripts/giap.sh status     # what this host is, what is built, what is running
bash scripts/giap.sh doctor     # read-only checks; exits 1 on any FAIL
bash scripts/giap.sh node       # a Node inside the repo's range, found or downloaded
```

`giap.sh` detects the host (Jetson, Linux, macOS) and whether CUDA actually works, and picks the cargo
features for it. Prefer `bash scripts/giap.sh build` to hand-written feature strings. Its doctor
catches what the build cannot report: goose submodule drift, a CPU-only build on a CUDA host, a
placeholder dashboard embedded because the UI was not built first, two service units competing for
one port, and a stray `target/debug` binary that `serve --native` prefers.

## 3. What a build costs

| Set | Example | Cost |
|---|---|---|
| Fast crates (no Goose, no llama.cpp, no ONNX Runtime) | `cargo test -p pond-core -p pond-api` | Seconds to a minute warm |
| Production binary | `cargo check -p pond-server -p pond-adapters-goose --all-targets` | Compiles the Goose submodule; 10 to 35 min cold |
| Heavy native, isolated in CI | `pond-inference`, `pond-adapters-vision-onnx`, `pond-adapters-face-onnx`, `pond-adapters-mesh-*`, `pond-adapters-lightning` | Each builds its own native stack |

The fast-crate list is in the `rust` job of `ci.yml`; copy it from there rather than from memory. A
crate on that list that grows a `goose` dependency breaks the hexagonal split, and CI will slow down
before anyone notices why.

`cargo check` does not compile `#[cfg(test)]` code. Run `cargo test` before believing a removal is complete.

`target/` fills the disk quickly. A full disk shows up as `ld: write() failed, errno=28`, which
reads like a miscompile. Check `du -sh target` before debugging a strange link error;
`rm -rf target/debug/incremental` recovers the most for the least.

## 4. Dependency rules

```
pond-core  <-  pond-infra, pond-api, pond-adapters-*, pond-mcp-server  <-  pond-server
```

- `pond-core` imports no framework: no Axum, SQLx, Goose or HTTP client. A Goose type in `pond-core`
  is a bug; add a port.
- Only `pond-server` knows every crate. It builds the adapters and wires them in `main.rs`.
- Every crate that depends on `goose` uses `default-features = false`, and `code-mode` stays off.
- `rmcp` is pinned once in `[workspace.dependencies]` (`=2.1.0`) so Goose and `pond-mcp-server`
  resolve to the same protocol crate. Change it only after checking both still build.
- The Goose crates' `workspace = true` dependencies resolve against this repo's root `Cargo.toml`,
  which mirrors `goose/Cargo.toml`'s `[workspace.dependencies]`. A submodule bump that adds a
  dependency there needs the same entry here.

## 5. Adding a capability

Pick the bounded context in `crates/pond-core/src/` (`user_data`, `models`, `mcp`, `security`,
`context`, `network`, `mesh`, `rides`, or `shared` when nothing else fits). Then:

1. Domain type in `<context>/domain/`.
2. Port trait in `<context>/ports/`, registered in its `mod.rs`.
3. Mock in `<context>/mocks/`, gated `#[cfg(any(test, feature = "test-mocks"))]`. Write the service
   tests against it first.
4. Adapter in an existing `pond-adapters-*` crate or a new one. Check whether Goose already has it.
5. Wire it in `crates/pond-server/src/main.rs` as `Arc<dyn Port>`.
6. Add it to `AppState` in `crates/pond-api/src/lib.rs` only if a route needs it.

The full walkthrough is [docs/developer/creating-ports-and-adapters.md](docs/developer/creating-ports-and-adapters.md).

## 6. Where things are

| Looking for | Path |
|---|---|
| Turn persistence | `crates/pond-core/src/shared/services/chat.rs` (`ChatService`); handlers never persist inline |
| Memory extraction (batched, on the inference lane) | `crates/pond-core/src/user_data/services/memory_extraction.rs` |
| The live agent engine | `crates/pond-adapters-goose/src/goose_agent.rs` (`GooseAdapter`) |
| System prompt and tool veto at the provider | `crates/pond-adapters-goose/src/provider_shim.rs` |
| Routes and handlers | `crates/pond-api/src/routes.rs` |
| Startup wiring, CLI | `crates/pond-server/src/main.rs` |
| Port numbers | `crates/pond-server/src/ports.rs` |
| Migrations | `crates/pond-infra/migrations/{system,logs,vectors}/` |
| Settings storage | `crates/pond-infra/src/sqlite_settings.rs` |
| MCP tools the model sees | `crates/pond-mcp-server/src/*.rs` (one `*McpServer` per module) |
| Which MCP servers start | `crates/pond-adapters-goose/src/giap_registration.rs` |
| My Pond (administration): unlock, rules, host adapter, routes | `pond-core/src/security/services/admin_elevation.rs`, `pond-core/src/shared/services/pond_admin.rs`, `pond-infra/src/host_pond_admin.rs`, `pond-api/src/pond_admin.rs` |
| Desktop IPC contract | `pond-desktop/src/shell/contract.ts` |
| Desktop voice child | `pond-desktop/electron/main/voice/` |
| Goose itself | `goose/crates/goose/src/` |

## 7. Traps

**Settings.** One row per field in a key-value table. A new field needs an `upsert!` in
`update_fields` and an arm in `apply_key`, or it saves, applies, and is gone on the next read
(`roundtrip_persists_every_field`). `every_settings_field_is_dispositioned` fails the build until the
field is classified as UI-wired or headless.

**MCP servers.** Each module in `pond-mcp-server` has an `init_*_deps` that fills its `OnceLock`s and
a `spawn_*_server` that Goose's builtin registry calls. If the deps were never initialised in the
running binary, the spawn logs an error and the server silently does not start; its tools are simply
missing. Initialise them in every binary that registers the server. `register_giap_extensions` decides which servers
start from the `ext_*_enabled` settings. `JsonSchema` field docs in this crate are tool descriptions
the model reads; edit them as prompt text.

**Goose.**
- Goose's session store latches its path on first use. `pin_goose_state_under()` in `main.rs` sets
  `GOOSE_PATH_ROOT` to `<data_dir>/engine` before anything touches it. Keep it first.
- All chat history lives in `pond_system.db`. Goose keeps its own `sessions.db`; the API never reads it.
- `Message` has no `.text()`; use `.as_concat_text()`. The agent's provider is private; use
  `agent.update_provider(...)`. Goose has no system role; system content goes in `Provider::stream`'s
  `system` argument.

**SQLite.**
- Migrations go through `sqlx::migrate!`, so the files must exist when `db.rs` compiles. Plain
  `sqlx::query` with several statements runs only the first.
- `datetime('now')` yields `YYYY-MM-DD HH:MM:SS` with no `T` or zone; parse it with
  `NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")`. Add `rowid` as a tiebreaker when sorting
  by these one-second timestamps.

**Ports.** HTTP binds `127.0.0.1:4000`, HTTPS `0.0.0.0:4443`, each trying the next nine ports when
taken. Never assume the port: read `<data_dir>/.runtime_api_port` and `.runtime_https_port`, and pass
`--port` explicitly when a service may already hold one.

**Source-reading tests.** Some tests read source as text (`*_is_wired.rs`, `*_wiring.rs`,
`egress_guard.rs`, `stream_handler_parity.rs`). After editing comments, run the touched crates' tests.

**The desktop sidecar.** `pond-desktop/resources/` is gitignored. A staged `pond-server` older than
the database's migrations dies at startup with no NDJSON output. Restage with
`bash scripts/stage-server-sidecar.sh` after a migration change.

## 8. Tests

| Layer | Command |
|---|---|
| Fast crates | the `cargo test` list in `ci.yml` |
| One test | `cargo test -p <crate> <name>` |
| Live hardware (skipped by default) | `cargo test -p <crate> -- --ignored` |
| The real binary over HTTP | `scripts/live-test.sh` (required for migrations, routes, handlers, startup wiring) |
| Desktop | `cd pond-desktop && npm test && npm run typecheck && npx playwright test` |

Unit tests build each database from empty in one process with hand-built adapters. They never
exercise startup ordering, migrating a database that already has rows, auth middleware or `main.rs`
wiring. `live-test.sh` does.

## 9. The Jetson

Test and measure on the Orin Nano first. Run the same on a Mac afterwards only when the maintainer
asks: the device is what ships, and the shared Mac is too loaded for a clean run. The device has its
own costs. Building on it takes the pond offline, a cold CUDA build costs 40 to 90 minutes, and the
board is short of memory while the pond serves. So stop the service before a cargo build there, and
never measure while anything builds or another session is testing on it.

```bash
bash scripts/jetson.sh deploy --branch <branch>   # a branch pushed to the device's origin, to test before merging
bash scripts/giap.sh deploy           # checks HEAD is on both remotes, then deploys main
bash scripts/jetson.sh build --cuda   # on the device
bash scripts/jetson-emu.sh doctor     # what the Mac can emulate (decisions, never performance)
```

The device's `origin` is a personal fork; a deploy from a commit pushed only to the org remote builds
stale code and still reports success. CUDA cannot be cross-compiled for `local-inference`. CI sets
`RUSTFLAGS` empty because `.cargo/config.toml`'s `-C target-cpu=native` produces binaries that
SIGILL on other CPUs; any cross or container build needs the same override.
