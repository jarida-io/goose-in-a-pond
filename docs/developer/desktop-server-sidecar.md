# How the desktop shell owns the pond-server sidecar

The Electron shell starts `pond-server serve` as a child process, watches it, and takes it down
again. This is what that costs, and which parts are load-bearing.

## Why

The shell needs a server. In the packaged app that server is a sidecar at
`Contents/Resources/pond-server`; in dev it is whatever `resolveServerBinary` finds under `target/`.
Either way the shell spawns it, owns it, and is the only thing that will ever shut it down — POSIX
does not kill a child when its parent dies.

Two properties matter more than they look, and both were violated at once in September 2026:

- **One launch must start at most one server.** `--port` is a *start* port for the server's
  `bind_with_fallback`, not a hard bind, and the CLI has no strict-port flag. A second instance
  therefore never fails. It binds the next free port, overwrites `.runtime_api_port`, and writes
  the same SQLite database as the first.
- **A server the shell did not start must not be inherited by accident.** `ensureRunning`
  health-checks before it spawns, and `/api/v1/health` returns only `{status, version}` — there is
  no pid or port in it, so the shell structurally cannot tell its own child from a stranger's
  server. An orphan on port 4000 is adopted, silently, by every subsequent launch.

## Components

| Concern | File | Notes |
|---|---|---|
| Lifecycle | `pond-desktop/electron/main/serverProcess.ts` | Locate, spawn, watch, replace, shut down |
| Quit and health loop | `pond-desktop/electron/main/lifecycle.ts` | One idempotent teardown; a cancellable loop |
| Orphan reaping | `pond-desktop/electron/main/orphan.ts` | Pidfile, liveness, identity. Shared with the voice child |
| Data dir and port file | `pond-desktop/electron/main/dataDir.ts` | Mirrors the server's `default_data_dir` |
| Wiring | `pond-desktop/electron/main/index.ts` | Events, window, the real implementations of every seam |

## Lifecycle and budgets

`ensureRunning` is serialised behind a promise lock, so the startup probe, the health loop and the
renderer's `ensure_server_running` cannot race into two children. Inside, in order:

1. Reap an orphan from a previous run — **before** the health check, because an orphan is healthy
   and checking first means adopting it. Once per process, so a recovery ten minutes in does not
   kill a server legitimately adopted at startup. Never in parent-managed mode.
2. Health check. A healthy server is adopted and nothing is spawned.
3. Drop a child that has already ended, **kill** one that is alive but never became healthy, and
   only then spawn. The assignment goes through `adoptChild`, which throws rather than overwrite a
   live child.
4. Poll until healthy, the child dies, or the budget runs out.

Both budgets are 120 seconds, deliberately the same: it is one binary doing one cold start, and
loading face recognition, Whisper and TTS routinely takes over a minute. The real asymmetry is how
the wait *ends* — a self-spawned wait can stop early because the shell holds the `ChildProcess` and
can watch it die, so a bad argument surfaces in seconds with its exit status rather than two minutes
later as "not healthy". A parent-managed wait can only ever time out.

Do not shorten the self-spawned budget. A short budget plus the kill in step 3 is a restart loop
that kills a server ten seconds into a ninety-second start.

## Ports

The shell assumes 4000, spawns with `--port 4000`, and then — only while its own child is starting,
only for a port file written after that spawn, and only if the port answers — adopts whatever
`.runtime_api_port` names. The mtime check is what stops yesterday's file outranking today's spawn.
When it moves it emits `server-url`, `PondApiClient.setBase` redirects the singleton, and the
renderer follows without a reload.

`resolveDataDir` does **not** use Electron's `app.getPath("userData")`. That is `~/.config` on Linux
while the server uses Rust's `dirs::data_dir()`, `~/.local/share`.

## Quitting

One idempotent teardown, wired to `before-quit`, `will-quit`, `process.on("exit")` and
SIGINT/SIGTERM. It stops the health loop first (a queued tick would otherwise respawn the server
during teardown), then kills the voice child, then the server — the microphone and speaker are
released before the server that outlives them.

`releaseChildren` and `releaseUi` are separate on purpose: killing a child is a synchronous syscall
and is legal from any exit hook, while the Electron calls behind `releaseUi` are not, and throwing
there would mask the kills that mattered.

SIGKILL reaches none of these, which is why the sidecar carries a pidfile reaper. Both children do;
`orphan.ts` is parameterised over a `ChildKind` because both are a `pond-server` and matching the
binary alone would have each reaper killing the other's process.

## Tests

| What | How |
|---|---|
| Lifecycle, budgets, reaping, port adoption | `cd pond-desktop && npx vitest run --project main` |
| The whole renderer and main suites | `cd pond-desktop && npm test` |
| A real Electron launch and the preload bridge | `cd pond-desktop && node scripts/smoke.mjs` |

The `ServerProcess` test seams (`orphanDeps`, `writePid`, `removePid`) are **not** optional. Without
them the suite reads the real pidfile under `tmpdir` and can SIGKILL a `pond-server` running in
another terminal.

## Live runbook, which is what actually catches this class of bug

None of the automated tests above would have caught the double spawn: every one of them builds a
`ServerProcess` by hand, and the defect lived in the interaction between a timeout and a timer.

```bash
pgrep -af "pond-server serve"
```

Stop anything that prints — a server left in an IDE terminal holds port 4000, and the shell will
adopt it rather than starting its own sidecar, so the packaged path goes untested. Then:

1. Launch the app. Within 30 seconds — past the health loop's first tick, which is the window the
   bug lived in — `pgrep -af "pond-server serve"` must show **exactly one** process, and
   `lsof -nP -iTCP:4000-4009 -sTCP:LISTEN` exactly one listener.
2. `.runtime_api_port` in the data directory must name the port that listener is on.
3. Quit the app. `pgrep` must show nothing.
4. Launch again, `kill -9` the Electron main process so no quit path runs, and confirm the sidecar
   is orphaned. Launch once more: the log must say it reaped the orphan and spawned, **not**
   `connected to an existing pond-server`.
