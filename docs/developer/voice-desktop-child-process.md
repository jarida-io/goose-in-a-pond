# Desktop voice: the child process

Where serve already runs room voice and `GET /api/v1/voice/room` answers `available: true`, the
desktop attaches to it instead of spawning anything: `VoiceChildProcess` subscribes to
`/voice/room/events`, whose `data:` lines are this same NDJSON, maps stop and start to
`PUT /voice/room {enabled}` and interrupt to `POST /voice/room/interrupt`, and authenticates with the
token the renderer paired with (`roomClient.ts`). The rest of this page is the child it spawns
everywhere else.

The desktop app's voice mode runs the terminal voice loop as a child process. The Electron main
process spawns:

```
pond-server chat --voice --json-events --session-id <uuid>
```

The child owns the microphone and the speaker. Wake word, end-of-speech detection, in-process
Whisper ASR, the `GooseAdapter` turn, Kokoro TTS, barge-in and interruption all run inside it,
exactly as in the terminal ([Voice mode](voice-mode.md)). The shell never opens the mic while a
session is active; it only relays events and writes two commands.

## Why a child process

Capture, recognition, inference and speech in one process cross no transport boundary
(cpal -> whisper-rs -> GooseAdapter -> Kokoro -> rodio). Cancellation is a `tokio::select!` over
the turn, and a turn is persisted once, only when it completes (`persist_confirmed_turn` in
`ChatService`). An HTTP pipeline per turn would have to rebuild all three across process hops.

## Components

| Layer | Location | Role |
|---|---|---|
| CLI flags | `crates/pond-server/src/main.rs` (`Commands::Chat`, `run_chat`) | `--json-events`: stdout carries NDJSON only; human output goes through the `out!` macro, which is a no-op in this mode. `--session-id`: the chat session to write to (default `default-session`) |
| Event source | `crates/pond-core/src/shared/services/chat.rs` | `ChatService::with_event_sink`; `run_loop` and `stream_response_inner` emit the events |
| Event types | `crates/pond-core/src/shared/domain/agent.rs` | `WorkflowEvent`, serde-tagged `event`, snake_case; golden tests pin each line |
| Stdin commands | `crates/pond-core/src/shared/services/voice_control.rs` | `parse_stdin_line`, `VoiceControl` (latched stop, interrupt edge), `TypedLineInput`; `run_chat` reads stdin with `pump_stdin` on a plain thread |
| Process manager | `pond-desktop/electron/main/voice/VoiceChildProcess.ts` | Spawn with piped stdin, readline on stdout and stderr, `interrupt`/`stop` to stdin, stderr tail, pidfile, kill on app exit |
| Line classification | `pond-desktop/electron/main/voice/ndjson.ts` | Pure: one line in, one event or exit notice out; `classifyEnd`, `encodeCommand`, `sessionToJoin` |
| Orphan reaping | `pond-desktop/electron/main/orphan.ts` | Pidfile + liveness + identity. Shared with the server sidecar via `ChildKind`: both are `pond-server`, so `VOICE_CHILD` requires the `chat` token and `SERVER_CHILD` requires `serve` |
| IPC | `pond-desktop/electron/main/ipc.ts`, `pond-desktop/src/shell/contract.ts` | `start_voice_session { sessionId }` (resumes that chat, or a fresh uuid when null; resolves to the id used), `stop_voice_session`, `interrupt_voice_turn` |
| Renderer | `pond-desktop/src/modes/voice/useVoiceSession.ts`, `VoiceMode.tsx` | `useVoiceSession` is the only listener for `voice-*` events. `VoiceMode` takes this path when `isDesktopShell()` is true, else the browser pipeline ([VOICE_MODE.md](../../pond-desktop/src/modes/VOICE_MODE.md)) |

## NDJSON contract (child stdout, one object per line)

| Line | Shell event | Notes |
|---|---|---|
| `{"event":"warmup","state":"warming"}` | `voice-warmup` (payload: the state string) | Only when the provider is `local`/`gguf` and the prefix prewarm runs; then one of `ready`, `skipped`, `failed`. Before `ready` |
| `{"event":"ready","session_id":"<uuid>"}` | `voice-ready` | After the models load and the greeting is spoken. Most startup diagnostics are held and sent right after it as `error` lines; a Whisper model that fails to load and a lock refusal are reported before it |
| `{"event":"state","state":"wait"}` | `voice-state` (payload: the state string) | `wait`, `listen`, `thinking`, `speak` |
| `{"event":"transcript","text":"..."}` | `voice-transcript` | The confirmed request, before the model starts |
| `{"event":"token","content":"..."}` | `voice-token` | Reply text, thinking stripped |
| `{"event":"tool_call","tool":"...","id":"..."}` | `voice-tool-call` | |
| `{"event":"tool_result","tool":"...","id":"...","content":"..."}` | `voice-tool-result` | `content` truncated to 2000 chars |
| `{"event":"turn_complete","session_id":"<uuid>"}` | `voice-done` | Only after the turn was persisted |
| `{"event":"error","message":"..."}` | `voice-error` | Recoverable; the session continues |
| `{"event":"audio_level","rms":0.42}` | `voice-audio-level` | Mic level while waiting or recording, playback level while speaking; throttled to 100 ms and a 0.02 change |
| `{"event":"exit","reason":"..."}` | held, then `voice-session-ended` | Reasons below |

Exit reasons written by the child: `stopped` (a `stop` command), `stdin_eof`, `dismissed` (the user
said "exit" or "quit"), `error` (the loop failed), `microphone_unavailable` (the mic never opened
after the retry budget), `already_running` (another voice session holds the lock).

`voice-session-ended` is made by the shell once both stdout and the process have closed:
`{ code, reason, session_id, detail }`. `reason` is the child's `exit` reason, or `crashed` if the
child died after `ready` without one, or `failed_to_start` if it died before `ready`. Only
`failed_to_start` carries `detail`, the last 20 stderr lines, because under `--json-events` a
startup failure prints nothing on stdout. The renderer treats `stopped`, `stdin_eof` and
`dismissed` as clean and shows any other reason as an error.

Renderer state mapping (`CHILD_STATE_MAP` in `useVoiceSession.ts`): `wait` -> `wait`, `listen` ->
`recording`, `thinking` -> `thinking`, `speak` -> `speaking`.

stdout must carry nothing but these lines. Anything printed from `run_chat` goes through `out!`
(stdout, suppressed here) or `eprintln!` (stderr); `ChatService` prints only when
`stdout_diagnostics` is on, which `--json-events` turns off. The spawn-binary contract test
enforces it.

## Stdin commands (shell to child, one object per line)

```json
{"cmd":"interrupt"}
{"cmd":"stop"}
```

Read only under `--json-events`; the terminal `chat` keeps Ctrl-C.

- `interrupt` ends the turn in flight: speech stops, the agent stream is aborted, nothing is
  persisted and no `turn_complete` is sent. The child goes back to waiting for the wake word
  (`state: wait`). An interrupt that arrives between turns is dropped, not saved for the next.
- `stop` ends the session from any state: a turn in flight is abandoned like an interrupt, a
  wake-word wait or a recording is cancelled and releases the mic, and the child emits
  `exit: stopped` and exits 0. A turn that already finished is persisted first; a stop that lands
  during a persist or a farewell waits for it.
- EOF on stdin is a stop, reported as `exit: stdin_eof`. When the shell sends `stop` and then
  closes stdin, the first reason wins.
- A line that starts with `{` but is not a known command is logged and ignored. Extra fields on a
  known command are ignored.
- With no speech model, stdin also carries typed turns: any line that does not start with `{` is a
  turn, and EOF ends input after the queued turns run. `stop` still stops at once.

The reader is a plain thread, not `spawn_blocking`: a stdin read cannot be cancelled, and runtime
shutdown waits on blocking tasks. Captures are cancelled by a flag set when their future is
dropped, so a stop during a recording does not leave a thread holding the mic.

## Stopping and interruption

- Stop: `VoiceChildProcess.stop()` writes `stop`, closes stdin, and polls for up to 3 s
  (`GRACEFUL_STOP_MS`) before `SIGKILL`. The kill is for a stuck child or a sidecar staged before
  the child read stdin.
- Interrupt: the voice screen shows Interrupt while the state is `speaking`; it calls
  `interrupt_voice_turn`, which writes `interrupt` and never ends the session.
- Barge-in and wake-word interruption happen inside the child. See
  [Voice pipeline interrupt](voice_pipeline_interrupt.md) for all four ways a turn ends early.
- `start` and `stop` are serialised in `VoiceChildProcess`, or an overlapping pair spawns two
  children holding the mic. A `close` from a killed session is matched by session identity, so it
  never reaps a newer one. The process manager binds to `close`, not `exit`: `exit` fires before
  stdio closes, and the last stderr line, usually the fatal one, would be missing.
- `VoiceMode` defers its stop by one tick on unmount, so a fast remount (StrictMode, quick
  re-entry) keeps the running child instead of reloading every model.

## Packaging

`pond-desktop/electron-builder.yml` ships `resources/pond-server` as an `extraResource` at
`Contents/Resources/pond-server`; the main process finds it through `process.resourcesPath`.
`scripts/stage-server-sidecar.sh` stages it and `scripts/verify-sidecar.sh` refuses to package
without a real one. `POND_SERVER_BIN` overrides the path for dev and tests.

`pond-desktop/resources/` is gitignored, so a staged sidecar goes stale silently. One older than the
database's applied migrations dies at startup with zero NDJSON lines, which the renderer reports as
`failed_to_start` with the stderr tail. Restage after any migration change. To reproduce, run the
child exactly as the shell does: healthy output is `{"event":"ready",...}` then
`{"event":"state","state":"wait"}`. Kill the probe when done; it holds the mic.

The pidfile reaper stays although a child exits on stdin EOF, which a hard kill of the shell
produces: a sidecar staged before stdin commands existed still needs it.

## Tests

| What | Command |
|---|---|
| Event goldens, `run_loop` races and persistence, stdin parser, interrupt and stop in every state | `cargo test -p pond-core` |
| Capture cancellation | `cargo test -p pond-adapters-whisper` |
| Typed stdin turns complete | `SQLX_OFFLINE=true cargo test -p pond-server --test pipeline_integration_test` |
| Spawned binary: NDJSON contract, `stop` exits 0 inside the shell's grace, commands are never typed turns | `SQLX_OFFLINE=true cargo test -p pond-server --test json_events_contract_test` |
| Line classification, command encoding, orphan reaping, child driver races, stop without SIGKILL | `cd pond-desktop && npx vitest run --project main` |
| Hook: single dispatch, state mapping, end-reason messages | `cd pond-desktop && npx vitest run --project renderer src/modes/voice/useVoiceSession.test.ts` |
| Orb, transcript and Interrupt with a stubbed bridge | `cd pond-desktop && npx playwright test tests/e2e/voice-session-child.spec.ts` |

CI's fast job runs `pond-core` and `pond-adapters-whisper`; it only compiles `pond-server`'s tests
(`cargo check --all-targets`), so run the two `pond-server` tests locally.

## Live-hardware runbook (not covered by automation)

macOS:

1. `cargo build -p pond-server`, then in `pond-desktop` run `npm run dev:electron` with
   `POND_SERVER_BIN` pointing at the build.
2. Enter voice mode; confirm the mic permission prompt names the app and `voice-ready` arrives
   (the orb leaves the connecting state).
3. Say the wake word; confirm the ping, the live transcript, the spoken reply, and the turn in the
   chat sidebar under the voice session id.
4. Talk over a reply: speech stops within about a second, the cut-off reply is not in the chat
   history, and what you said is answered. Then press Interrupt mid-reply: speech stops, the orb
   returns to the wake word, and the cut-off turn is not in the history.
5. Press Stop session, then leave voice mode; the shell log shows
   `voice child exited cleanly after stop` (not `killing`), no `pond-server chat` process remains,
   and the mic indicator goes out. `kill -9` the shell and confirm no orphaned child survives the
   next app start.
6. Run a text chat during a voice turn; check both logs for `SQLITE_BUSY` (serve and the child
   both write the WAL; a failed persist is reported as an `error` line).

Jetson: build on the device with `bash scripts/jetson.sh build --cuda`, drop caches before loading
models, watch `tegrastats` during a session, and soak 30+ turns before
`sqlite3 pond_system.db 'PRAGMA integrity_check;'`.

Open: macOS TCC attribution for a child spawned from a packaged `.app` is unverified, and
serve-plus-child WAL contention needs the soak above.
