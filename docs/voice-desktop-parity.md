# Desktop Voice Mode — Functional Parity with CLI

This document covers the changes that brought the desktop Tauri app's voice pipeline to
functional parity with the CLI's `pond-server chat --input whisper` experience.

---

## Overview

The CLI voice mode had a rich, low-latency pipeline: sentence-level streaming TTS, thinking
block filtering, VAD-based recording, conversational turn-taking, wake word one-breath flow,
and symbol normalization. The desktop voice mode was missing all of these — it waited for the
full LLM response before calling TTS, showed raw `<think>` blocks, used a fixed countdown
timer for recording, and forced the user to press buttons to interact.

These changes make both experiences functionally identical.

---

## Features Added

### 1. VAD-Aware Recording

**Files:** `pond-desktop/src-tauri/src/audio.rs`, `pond-desktop/src-tauri/src/commands/audio_cmd.rs`

The desktop no longer uses a countdown timer. Recording uses voice activity detection (VAD)
matching the CLI's `record_mono_f32_vad`:

1. Opens the mic and waits up to 10 seconds for speech onset (RMS > 0.018)
2. Once speech starts, records until 800ms of consecutive silence (RMS < 0.008)
3. Hard cap at 30 seconds total recording time
4. Emits `audio-level` events for waveform animation during recording

Tauri command: `record_with_vad` — blocks until the user finishes speaking, then returns
16 kHz mono WAV bytes. No manual stop needed.

### 2. Sentence-Level Streaming TTS

**File:** `pond-desktop/src-tauri/src/commands/audio_cmd.rs`

The voice pipeline (`run_voice_pipeline`) now processes SSE tokens incrementally:

```
SSE token stream --> ThinkBlockFilter --> SentenceBuffer
                                              |
                     for each complete sentence:
                        strip_markdown()
                        normalize_symbols()
                              |
                         POST /api/v1/tts --> play_wav_bytes()
                         (concurrent with more tokens arriving)
```

A `tokio::sync::mpsc` channel separates SSE parsing from TTS playback. The main task
parses SSE and sends sentences to the channel; a background task plays them sequentially.

### 3. Thinking Tone

**File:** `pond-desktop/src-tauri/src/commands/audio_cmd.rs`

A subtle 440 Hz sine pulse (8% volume, 1-second cycle) plays on a background thread while
the LLM is inferring. Stopped via an atomic flag when the first real sentence arrives.
Matches the CLI's `PiperOutput::start_thinking_tone()`.

### 4. Conversational Turn-Taking

**Files:** `pond-desktop/src/modes/voice/useVoicePipeline.ts`, `pond-desktop/src/modes/voice/TauriVoiceBackend.ts`

After Goose speaks, the app automatically starts recording for the user's next turn — no
wake word needed within an active conversation. If the user doesn't speak within 8 seconds,
the conversation ends and the app returns to passive wake-word listening.

State machine:
```
wait --> wake word --> think --> speak --> [auto-listen] --> think --> speak --> ...
                                               | (no speech 8s)
                                              wait
```

### 5. Dismissal Phrases

**Files:** `pond-desktop/src-tauri/src/tts_text.rs`, `pond-desktop/src-tauri/src/commands/audio_cmd.rs`

The voice pipeline recognizes dismissal phrases and speaks a farewell:

| Phrase | Action | TTS Response |
|--------|--------|-------------|
| "bye", "goodbye", "dismissed", "go to sleep", "stop" | Return to wake word mode | "Until next time. Just say my name when you need me." |
| "exit", "quit" | Full exit | "Goodbye! I'll be here whenever you need me." |

Emits `voice-dismissed` event so `TauriVoiceBackend.ts` can reset state.

### 6. Whisper Artifact Stripping

**Files:** `pond-desktop/src-tauri/src/tts_text.rs`, `pond-desktop/src-tauri/src/audio.rs`

`strip_whisper_artifacts()` removes non-speech tags and hallucinations before processing:

- Strips `[BLANK_AUDIO]`, `[MUSIC]`, `[NOISE]`, etc.
- Strips `(inaudible)`, `(music)`, `(laughing)`, etc.
- Rejects common hallucinations: "thank you", "thanks for watching", "um", "uh", etc.
- Rejects transcripts <= 2 characters
- Rejects repeated-word transcripts ("the the the")

Applied in both the wake listener (before trigger matching) and the voice pipeline (after
transcription, before sending to LLM).

### 7. Auto-Start Recording on Mount

**File:** `pond-desktop/src/modes/voice/VoiceMode.tsx`

When VoiceMode mounts with no wake word configured, it auto-starts VAD recording instead of
showing a "Start Listening" button. Matches the CLI's `InstantActivation` behavior.

---

## Bug Fixes

### SSE Chunk-Boundary Corruption

**File:** `pond-desktop/src-tauri/src/commands/audio_cmd.rs`

The SSE parser used `text.lines()` directly on each HTTP chunk. HTTP chunked transfer can
split at any byte boundary, causing partial JSON lines to be silently dropped. Fixed with a
`line_buf` that carries partial lines across chunks.

### Think Block Filtering (Server-Side)

**File:** `crates/pond-api/src/routes.rs`

Both `/api/v1/chat/stream` and `/api/v1/agent/chat/stream` now call `filter_thinking()` on
every `AgentStreamEvent::Text` before serializing to SSE. Raw `<think>` blocks no longer
reach clients.

### Think Block Filtering (GooseAdapter)

**File:** `crates/pond-adapters-goose/src/goose_agent.rs`

`GooseAdapter::chat_stream()` bypasses `LocalInferenceLlmAdapter` (which had
`strip_thinking_tokens`), using Goose's internal provider directly. Added
`strip_thinking_tokens()` at the source — every `AgentStreamEvent::Text` is filtered before
yielding, handling both `<think>...</think>` and Gemma 4 `<|channel>thought...<channel|>`
formats.

### Think Block Filtering (GGUF Adapter)

**File:** `crates/pond-adapters-local-inference/src/lib.rs`

`strip_thinking_tokens()` expanded from Gemma 4 only to also handle the standard
`<think>...</think>` format used by Qwen3, DeepSeek-R1, QwQ, and other reasoning models.

### Think Block Filtering (Desktop Client)

**Files:** `pond-desktop/src-tauri/src/canvas_feed.rs`, `pond-desktop/src/sections/Chat.tsx`, `pond-desktop/src/lib/thinkFilter.ts`

Defense-in-depth: `dispatch_sse_event()` filters thinking blocks before emitting
`response-token` events. Chat.tsx also filters via `filterThinking()` from the TypeScript
port in `lib/thinkFilter.ts`. The static Mutex TOCTOU race was fixed (single lock
acquisition).

### Tool Call Card Crash

**Files:** `pond-desktop/src-tauri/src/canvas_feed.rs`, `pond-desktop/src/components/ContextCard.tsx`

Tool call events sent `null` data which crashed ContextCard's property accesses, causing
React to unmount the entire tree (blank screen). Fixed: Rust sends `{}` instead of `null`;
TypeScript components use optional chaining and null guards. Added `Some("tool_result")`
match arm so actual tool results are forwarded to the frontend.

### CanvasOverlay Transcript Payload Type

**File:** `pond-desktop/src/canvas/CanvasOverlay.tsx`

Changed `listen<string>("transcript", ...)` to `listen<{ text: string }>("transcript", ...)`
to match the actual `TranscriptResult { text: String }` payload from Rust.

### Silence Detection Stale Closure

**File:** `pond-desktop/src/modes/VoiceMode.tsx`

The silence detection effect captured `state.voiceState` in a `useEffect([], ...)` closure,
freezing it at the mount-time value. Added `voiceStateRef` that's updated on every render.

---

## Architecture

### Three Levels of Think Block Defense

| Level | Location | Scope |
|-------|----------|-------|
| Source | `goose_agent.rs` `strip_thinking_tokens()` | All consumers |
| Server | `routes.rs` `filter_thinking()` | HTTP SSE clients |
| Client | `canvas_feed.rs` + `Chat.tsx` | UI display |

### SSE Event Format

After cleanup, text events use a single format:
```json
{"type": "text", "content": "Hello"}
```

The redundant `"token"` field was removed from both `chat_stream` and `agent_chat_stream`.

---

## Testing

- `cargo test -p pond-core` — 221 tests pass (includes 48 normalization tests)
- `cargo build -p pond-server` — compiles clean
- `cd pond-desktop/src-tauri && cargo build` — compiles clean
- `cd pond-desktop && npx vitest run` — 128/130 pass (2 pre-existing Schedules failures)
