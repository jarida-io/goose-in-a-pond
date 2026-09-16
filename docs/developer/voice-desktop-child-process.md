# Desktop voice via the terminal voice loop (child process)

The desktop app's voice mode runs the proven terminal voice loop as a child
process instead of re-implementing the pipeline over HTTP. The shell spawns:

```
pond-server chat --voice --json-events --session-id <uuid>
```

The child exclusively owns the microphone and speaker: wake-word detection,
VAD, in-process Whisper ASR, the GooseAdapter agent turn, in-process Piper
TTS, and barge-in all happen inside the child, exactly as in the terminal.
The shell never opens the mic while a voice session is active.

## Why

The terminal loop is reliable because it has no transport boundaries
(cpal -> whisper-rs -> GooseAdapter -> piper-rs -> rodio in one process),
working cancellation (`tokio::select!` + `JoinHandle::abort`, 50ms-polled TTS
interrupt flag), and exactly-once turn persistence (`persist_confirmed_turn`
fires only on uninterrupted completion). The previous desktop pipeline crossed
four transport hops per turn and had verified bugs (unauthenticated `/tts`
fetches, double event dispatch into the reducer, no-op `abort_recording`).

## Components

| Layer | Location | Role |
|-------|----------|------|
| CLI flags | `crates/pond-server/src/main.rs` | `--json-events` (pure NDJSON on stdout, diagnostics to stderr), `--session-id` (replaces the hardcoded `default-session`) |
| Event source | `crates/pond-core/src/shared/services/chat.rs` | `ChatService` builder takes `.with_event_sink(...)`; `run_loop` / `stream_response_inner` emit state, transcript, token, tool, and turn-complete events |
| Event types | `crates/pond-core/src/shared/domain/agent.rs` | `WorkflowEvent` serde-serializes to the NDJSON contract (`tag = "event"`, snake_case) |
| Process manager | `pond-desktop/electron/main/voice/VoiceChildProcess.ts` | Spawn with held-open piped stdin, a readline reader on stdout, orphan cleanup, kill on app exit |
| Line classification | `pond-desktop/electron/main/voice/ndjson.ts` | Pure: one line in, one event or exit notice out. Where the 30-odd golden tests live |
| Orphan reaping | `pond-desktop/electron/main/orphan.ts` | Pidfile + liveness + identity, for a child that outlived a hard kill of the shell. Shared with the pond-server sidecar, which is why it is parameterised over a `ChildKind`: both children are a `pond-server`, so the voice predicate requires the `chat` subcommand and the sidecar's requires `serve`, and neither can reap the other |
| IPC | `pond-desktop/electron/main/ipc.ts` | `start_voice_session` (returns the session uuid), `stop_voice_session` (stdin EOF, then kill after 3s) |
| Frontend | `pond-desktop/src/modes/voice/useVoiceSession.ts` | Single owner of all `voice-*` events; `VoiceMode.tsx` picks the child-process path when `isDesktopShell()` and the unchanged `WebVoiceBackend` pipeline in a plain browser |

## NDJSON contract (child stdout, one JSON object per line)

```json
{"event":"ready","session_id":"<uuid>"}
{"event":"state","state":"wait"}            // wait | listen | thinking | speak
{"event":"transcript","text":"..."}
{"event":"token","content":"..."}
{"event":"tool_call","tool":"giap__x","id":"..."}
{"event":"tool_result","tool":"giap__x","id":"...","content":"..."}
{"event":"turn_complete","session_id":"<uuid>"}
{"event":"error","message":"..."}
{"event":"exit","reason":"stdin_eof"}       // stdin_eof | dismissed | error
```

The shell maps these 1:1 to IPC events (`voice-ready`, `voice-state`,
`voice-transcript`, `voice-token`, `voice-tool-call`, `voice-tool-result`,
`voice-done`, `voice-error`) plus `voice-session-ended {code, reason}` on any
child exit. Frontend state mapping: wait -> idle, listen -> recording,
thinking -> thinking, speak -> speaking.

In `--json-events` mode stdout must contain zero non-JSON bytes; any new
`println!` reachable from `run_chat` must go through the `out!` macro (or
stderr). The spawn-binary contract test enforces this.

## Stopping and interruption

- Stop = close the child's stdin (the shell does this in `stop_voice_session`);
  the child exits cleanly with `exit: stdin_eof`. Kill is the 3-second fallback.
- Barge-in and wake-word interruption happen inside the child, where they
  already work. The shell's `AudioKillSwitch` cannot reach the child's audio.
- The `InstantActivation` race that broke stdin / `--no-wake-word` mode is
  fixed: `StreamingWakeWordDetector::supports_interruption()` gates the
  `run_loop` interrupt race (`InstantActivation` returns false).

## Tests

| Layer | Command |
|-------|---------|
| Event serialization goldens, run_loop race/persistence regressions | `cargo test -p pond-core` |
| Pipeline integration (stdin mode completes turns) | `SQLX_OFFLINE=true cargo test -p pond-server --test pipeline_integration_test` |
| Spawn-binary NDJSON contract | `SQLX_OFFLINE=true cargo test -p pond-server --test json_events_contract_test` |
| Shell NDJSON parser goldens, orphan reaping, the child driver's races | `cd pond-desktop && npx vitest run --project main` |
| Hook single-dispatch regression + state mapping | `cd pond-desktop && npm test` |
| Orb/transcript E2E with a stubbed bridge | `cd pond-desktop && npx playwright test tests/e2e/voice-session-child.spec.ts` |

## Live-hardware runbook (not covered by automation)

macOS:
1. `npm run build` in pond-desktop, then `cargo build -p pond-server`, then
   `POND_SERVER_BIN=<path> npm run dev:electron`.
2. Enter voice mode; confirm the mic permission prompt attributes to the app
   and `voice-ready` arrives (orb leaves the connecting state).
3. Speak the wake word; confirm ping, live transcript, spoken reply, and the
   turn appearing in the chat sidebar under the voice session id.
4. Barge in mid-reply; TTS must stop within ~1s.
5. While in a voice session, confirm the shell wake listener is suspended
   (no transcribe requests in serve logs) and `record_with_vad` is refused.
6. Leave voice mode; confirm the child exits (no `pond-server chat` process
   left) and the wake listener resumes. Also `kill -9` the shell and confirm
   no orphaned child survives the next app start.
7. Run a UI text chat during a voice turn; check both logs for SQLITE_BUSY.

Jetson (nano@nano.local):
1. Build on-device with `bash scripts/jetson.sh build --cuda`; keep the
   child's Whisper tier at tiny/base (it duplicates serve's CUDA context).
2. `drop_caches` before load (NvMap ~586MiB wall), watch `tegrastats` during a
   session for GPU/NvMap headroom.
3. Verify ALSA tolerates the shell-suspended/child-active capture handoff.
4. Soak 30+ turns, then `sqlite3 pond_system.db 'PRAGMA integrity_check;'`.

Known open items: macOS TCC attribution for a bundle-spawned child is still
unverified on a packaged .app, and the chain is now one hop longer
(app -> pond-server -> node) than it was under Tauri; dual-writer WAL
contention (serve + child) needs the soak test above.

Packaging is no longer one of them: `electron-builder.yml` ships pond-server as
an `extraResource` at `Contents/Resources/pond-server`, the main process
resolves it through `process.resourcesPath`, and `scripts/verify-sidecar.sh`
refuses to package without a real one. `POND_SERVER_BIN` survives as a
deliberate override for dev and tests.

One defect worth knowing while reading `stop_voice_session`: closing stdin is
the documented clean-exit signal, but in the shipped configuration the child
never reads stdin — voice mode wires `WhisperRsInput`, which does not — so the
grace period always expires and the stop escalates to a kill. That is a defect
in the child rather than the shell, and fixing it (a stdin-EOF watcher racing
the run loop) would also make the pidfile orphan reaper deletable.
