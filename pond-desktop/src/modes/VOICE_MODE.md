# Voice mode: renderer guide

`modes/voice/VoiceMode.tsx` is UI only. It picks one of two implementations at runtime:

| Path | When | Hook | Who owns the microphone |
|---|---|---|---|
| Desktop | `isDesktopShell()` is true (the preload bridge `window.giap` exists) | `useVoiceSession` | The `pond-server chat --voice --json-events` child the main process spawns (`electron/main/voice/VoiceChildProcess.ts`). The renderer never opens the mic |
| Browser | Otherwise: Vite in a browser, or the UI served by `pond-server` | `useVoicePipeline` -> `WebVoiceBackend` | The renderer (`getUserMedia`); audio goes to `POST /api/v1/transcribe`, replies stream from `/api/v1/chat/stream`, speech comes from `POST /api/v1/tts` |

`isDesktopShell()` tests the bridge itself. A check that silently returned false would drop the
desktop app onto the browser path, which looks like it works.

The child's wake word, end-of-speech detection, barge-in and stop/interrupt semantics are in
[Voice mode](../../../docs/developer/voice-mode.md) and
[Desktop voice: the child process](../../../docs/developer/voice-desktop-child-process.md).

## State

`VoiceState` in `state/reducer.ts`: `idle | wait | recording | thinking | speaking | error`.

| State | Label (`STATE_LABELS`) | Colour (`STATE_COLORS`) | Desktop source |
|---|---|---|---|
| `idle` | Tap to talk | `#8E8E93` | before `voice-ready`, and after a clean end |
| `wait` | Listening for wake word... | `#8C4BFF` | child `state: wait`, `voice-ready`, `voice-done` |
| `recording` | Listening... | `#8C4BFF` | child `state: listen` |
| `thinking` | Thinking... | `#FF9500` | child `state: thinking` |
| `speaking` | Speaking... | `#34C759` | child `state: speak` |
| `error` | the error message | `#FF3B30` | `voice-error`, or an unclean `voice-session-ended` |

The desktop view also shows "Warming up..." while `voice-warmup` says `warming`, "Starting voice
session..." until `voice-ready`, and "Server offline" when the server is down. On the desktop
path an error persists until Dismiss or Retry; nothing clears it on a timer.

## Desktop path: `useVoiceSession`

`useVoiceSession` is the only listener for `voice-*` events (the rule is at the top of the file).
It maps them onto app state:

| Event | Effect |
|---|---|
| `voice-warmup` | `warmingUp` while the state is `warming` |
| `voice-ready` | session active, `SET_SESSION_ID`, state `wait` |
| `voice-state` | `CHILD_STATE_MAP` (`listen` -> `recording`, `speak` -> `speaking`) |
| `voice-transcript` | appends the user message and an empty agent message for tokens to fill |
| `voice-token` | buffered, one `APPEND_AGENT_TOKEN` per animation frame |
| `voice-tool-call` / `voice-tool-result` | a context card, updated with its result by call id |
| `voice-done` | flushes tokens, marks the reply done, state `wait` |
| `voice-error` | error state with the message |
| `voice-audio-level` | `audioLevel` for the orb (mic level while listening, playback level while speaking) |
| `voice-session-ended` | session inactive; `stopped`, `stdin_eof` and `dismissed` return to `idle`, anything else shows `endedErrorMessage` (for `failed_to_start`, the last stderr line) |

Events carrying another session's id are dropped, so a previous child's late lines cannot reach
the screen. `startSession()` passes the chat view's current session id, so voice continues that
conversation; `null` starts a fresh one.

`VoiceModeChildProcess` starts the session on mount and stops it on unmount, deferred by one tick
so a quick remount (StrictMode, fast re-entry) keeps the running child instead of reloading its
models. Its controls:

| Control | Shown | Does |
|---|---|---|
| Interrupt | while `speaking` | `interrupt_voice_turn`: stops the reply, the turn is not saved, the child waits for the wake word |
| Stop session | while the session is active | `stop_voice_session`: the child exits and releases the mic |
| Dismiss / Retry | in `error` | clear the error / start a new session |
| Voice switcher, Clear conversation | header | pick the Kokoro voice / clear transcript and cards |

The orb flashes on `wait` -> `recording`, so a heard wake word is visible, not only a label change.

## Browser path: `useVoicePipeline`

`WebVoiceBackend` runs its own wake listener: it captures each burst of speech, posts it to
`/api/v1/transcribe`, and matches the wake word and stored variants with `containsTrigger`
(`modes/voice/wakePhrase.ts`, mirroring the server's whole-word matcher). After a reply it listens
for a follow-up and returns to the wake word after 8 s without speech (`NO_SPEECH_TIMEOUT_MS`).
Speech is split with `ReplySplitter` and each unit is fetched from `/api/v1/tts`
(`modes/voice/webAudioUtils.ts`).

Recording stops on its own after `voice_recording_duration_secs` (settings default 3 s; 30 s until
settings load), shown by the `CountdownRing` in `VoiceMode.tsx`. Send, Cancel, Record now and
Interrupt buttons drive it by hand.

## Components

| Component | Used by | Notes |
|---|---|---|
| `components/VoiceOrb.tsx` | both paths | Takes `state`, `size`, `audioLevel`, and `pulseKey` (desktop flash) |
| `modes/voice/VoiceStream.tsx` | desktop path | Three-column grid: the pond's lines left, the orb channel in the middle, yours right; context cards inline |
| `components/TranscriptFeed.tsx` | browser path | Compact transcript with context cards placed after the message that triggered them |
| `CountdownRing` (inline in `VoiceMode.tsx`) | browser path | Seconds left in a recording |
| `components/AudioWaves.tsx` | `WakeWordCalibration` only | Bar meter for the calibration recorder |

Cmd+Shift+V (Ctrl+Shift+V) summons voice mode from anywhere.

## Tests

| What | Where |
|---|---|
| Event handling, state mapping, end-reason messages | `modes/voice/useVoiceSession.test.ts` |
| Wake-phrase rules and calibration prompts | `modes/voice/wakePhrase.test.ts` |
| Browser speech splitting and normalization | `modes/voice/webAudioUtils.test.ts` |
| Desktop screen with a stubbed bridge | `tests/e2e/voice-session-child.spec.ts` |
| Browser pipeline | `tests/e2e/voice_pipeline.spec.ts` |
