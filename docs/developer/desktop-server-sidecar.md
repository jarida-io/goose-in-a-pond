# How the desktop shell owns the pond-server sidecar

The Electron shell starts `pond-server serve` as a child process, watches it, and takes it down
again. This covers where that binary comes from, how the shell finds it, and which parts of the
lifecycle are load-bearing.

## Staging and packaging

| Step | Command | What it does |
|---|---|---|
| Stage | `npm run stage:server` (`scripts/stage-server-sidecar.sh`) | Stages a recorded LiteRT-LM package into `resources/litert-lm/` (warns and skips when none is recorded); builds the web UI; builds `pond-server` release (default features, mesh included) with `RUSTFLAGS=""` and `SQLX_OFFLINE=true`; copies it to `pond-desktop/resources/pond-server`, writes `resources/.dist-stamp`, and builds the Go `pondnet` helper into `resources/` |
| Verify | `scripts/verify-sidecar.sh` | Refuses to package unless the sidecar and `pondnet` exist and run, the sidecar is a Mach-O of the right architecture, its dist stamp matches the `dist/` about to ship, and any staged LiteRT-LM library is complete, signed, loadable and listed under `mac.binaries` |
| Package | `npm run bundle:app` (`.app` and `.dmg`) or `npm run pack:dir` (unpacked) | Stage, `build:main`, verify, then `electron-builder --mac` |

`RUSTFLAGS` is emptied because `.cargo/config.toml` sets `-C target-cpu=native`, and a shipped
binary built that way can SIGILL on another CPU. The dist stamp exists because `dist/` feeds two
consumers: the copy compiled into the sidecar (served to the LAN and phones) and the renderer
inside the app; if they drift, the two surfaces show different versions.

`pond-desktop/electron-builder.yml` puts `pond-server`, `pondnet`, `litert-lm/` and the Matter
controller's sources under `Contents/Resources/`. Nothing is signed (`identity: null`,
`hardenedRuntime: false`).

`pond-desktop/resources/` is gitignored, so a staged sidecar goes stale silently. One older than the
database's applied migrations dies at startup; restage after anything that touches migrations.

## Finding the binary

`resolveServerBinary` (`electron/main/serverProcess.ts`) takes, in order: `POND_SERVER_BIN` if it
exists; inside a packaged app, `Contents/Resources/pond-server` only; otherwise
`target/release/pond-server`, `target/debug/pond-server`, then `pond-desktop/resources/pond-server`.

When `GIAP_SERVER_PORT` is set (as `pond-server serve --native` does when it launches the app),
the shell is **parent-managed**: it never spawns, kills or reaps a server and only waits for the
one at that port.

## Why the lifecycle is strict

POSIX does not kill a child when its parent dies, so the shell is the only thing that will ever
shut its server down. Two properties follow:

- **One launch starts at most one server.** `--port` is a start port for the server's
  `bind_with_fallback`, not a hard bind, and there is no strict-port flag. A second instance never
  fails: it binds the next free port, overwrites `.runtime_api_port`, and writes the same SQLite
  database as the first.
- **A server the shell did not start is never inherited by accident.** `/api/v1/health` returns
  `{status, version}` to a loopback peer and `{status}` to anyone else, with no pid or port, so the
  shell cannot tell its own child from a stranger's server. An orphan on port 4000 would be adopted
  by every later launch.

## Components

| Concern | File | Notes |
|---|---|---|
| Lifecycle | `electron/main/serverProcess.ts` | Locate, spawn, watch, replace, shut down |
| Quit and health loop | `electron/main/lifecycle.ts` | One idempotent teardown; a cancellable loop (10 s while healthy) |
| Orphan reaping | `electron/main/orphan.ts` | Per-user pidfiles under the OS temp dir; liveness and identity checks; shared with the voice child |
| Data dir and port file | `electron/main/dataDir.ts` | Mirrors the server's `default_data_dir`; reads `.runtime_host_credential`, which the renderer gets over IPC (`host_credential`, `electron/main/ipc.ts`) and sends as `X-Pond-Host-Credential` |
| IPC sender and navigation | `electron/main/navigation.ts` | The app's origins; `ipc.ts` refuses any other sender, and no window leaves them (see [auth-network-posture.md](../auth-network-posture.md)) |
| Wiring | `electron/main/index.ts` | Events, window, the real implementation of every seam |

## Lifecycle and budgets

`ensureRunning` is serialised behind a promise lock, so the startup probe, the health loop and the
renderer's `ensure_server_running` cannot race into two children. Inside, in order:

1. Reap an orphan from a previous run, **before** the health check: an orphan is healthy, and
   checking first means adopting it. Once per process, so a recovery later does not kill a server
   legitimately adopted at startup. Never when parent-managed.
2. Health check. A healthy server is adopted (`connected to an existing pond-server`) and nothing
   is spawned.
3. Drop a child that has already ended, kill one that is alive but never became healthy, and only
   then spawn `pond-server serve --port 4000`. `adoptChild` throws rather than overwrite a live
   child.
4. Poll until healthy, the child dies, or the budget runs out.

Both budgets are 120 seconds (`SPAWNED_POLL_ATTEMPTS`, `PARENT_MANAGED_POLL_ATTEMPTS`): one binary,
one cold start, and loading face recognition, Whisper and TTS routinely takes over a minute. A
self-spawned wait can still end early, because the shell holds the `ChildProcess` and sees it
exit, so a bad argument surfaces in seconds with its exit status. A parent-managed wait can only
time out. Do not shorten the self-spawned budget: a short budget plus the kill in step 3 is a
restart loop that kills a server partway through its start.

Kills are SIGTERM, a 3-second grace (`KILL_GRACE_MS`), then SIGKILL; SIGTERM first lets the server
flush the SQLite WAL.

## Ports

The shell assumes 4000 and spawns with `--port 4000`. Then, only while its own child is starting,
only for a port file written after that spawn (the mtime check stops yesterday's file outranking
today's spawn), and only if the port answers, it adopts whatever `.runtime_api_port` names. It
emits `server-url`, `PondApiClient.setBase` redirects the singleton, and the renderer follows
without a reload.

`resolveDataDir` does not use Electron's `app.getPath("userData")`: that is `~/.config` on Linux,
while the server uses Rust's `dirs::data_dir()`, `~/.local/share`.

## Quitting

One idempotent teardown, wired to `before-quit`, `will-quit`, `process.on("exit")` and
SIGINT/SIGTERM. It stops the health loop first (a queued tick would otherwise respawn the server
during teardown), then kills the voice child, then the server, so the microphone and speaker are
released before the server that outlives them.

`releaseChildren` and `releaseUi` are separate on purpose: killing a child is a synchronous syscall
and is legal from any exit hook, while the Electron calls behind `releaseUi` are not, and throwing
there would mask the kills that mattered.

SIGKILL reaches none of these, which is why both children carry a pidfile reaper. `orphan.ts` is
parameterised over a `ChildKind` (`SERVER_CHILD`, `VOICE_CHILD`) because both are a `pond-server`,
and matching the binary alone would have each reaper killing the other's process.

## Tests

| What | How |
|---|---|
| Lifecycle, budgets, reaping, port adoption | `cd pond-desktop && npx vitest run --project main` |
| The renderer and main suites | `cd pond-desktop && npm test` |
| A real Electron launch and the preload bridge | `cd pond-desktop && node scripts/smoke.mjs` |

The `ServerProcess` test seams (`readPortFile`, `orphanDeps`, `writePid`, `removePid`) are not
optional. Without them the suite reads the real pidfile and can SIGKILL a `pond-server` running in
another terminal.

## Live runbook

None of the automated tests above catches a double spawn: each builds a `ServerProcess` by hand,
and that class of defect lives in the interaction between a timeout and a timer. Before a release:

```bash
pgrep -af "pond-server serve"
```

Stop anything it prints: a server left in an IDE terminal holds port 4000, and the shell will adopt
it instead of starting its own sidecar, so the packaged path goes untested. Then:

1. Launch the app. Within 30 seconds, past the health loop's first tick,
   `pgrep -af "pond-server serve"` shows exactly one process and
   `lsof -nP -iTCP:4000-4009 -sTCP:LISTEN` exactly one listener.
2. `.runtime_api_port` in the data directory names the port that listener is on.
3. Quit the app. `pgrep` shows nothing.
4. Launch again, `kill -9` the Electron main process so no quit path runs, and confirm the sidecar
   is orphaned. Launch once more: the log says it reaped the orphan and spawned, not
   `connected to an existing pond-server`.
