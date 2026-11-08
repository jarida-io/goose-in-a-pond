# Voice mode

One loop: wake word, request, working tone, spoken answer. Serve runs it in the room on the pond's
own microphone and speaker (`room_voice.rs`, `GET /api/v1/voice/room`), and the desktop attaches to
that; the terminal (`pond-server chat --voice`) runs the same loop, and the desktop spawns it as a
child process where serve's room cannot listen
([Desktop voice: the child process](voice-desktop-child-process.md)).

| Stage | Code |
|---|---|
| Microphone (one owner, 16 kHz mono ring) | `crates/pond-audio` (`pond_audio::spawn`, `MicHandle`) |
| Device choice and supervision | `crates/pond-adapters-audio-devices`, started in `run_chat` with `Role::Voice` |
| Which front end listens | `build_listening` in `crates/pond-server/src/voice_rig.rs` |
| Wake word | The ear (`crates/pond-adapters-ear`): the "hey goose" CNN or Whisper on each speech segment (`voice_wake_detector`); without Silero, `WhisperKeywordDetector` in `crates/pond-adapters-whisper/src/lib.rs` |
| End of speech | The ear's Silero endpointer on echo-cancelled audio; without the ear, `pond-adapters-silero` or an RMS gate |
| Interrupting a reply | The ear's `InterruptWatch`: the wake word, or an enrolled voice; without the ear, energy barge-in |
| Speech to text | `WhisperRsInput` in `crates/pond-adapters-whisper/src/in_process.rs` (whisper-rs 0.16, in process) |
| The turn | `ChatService::run_loop` and `stream_response_inner` in `crates/pond-core/src/shared/services/chat.rs` |
| Speech text | `crates/pond-voice/src/text.rs` ([TTS text normalization](../tts-text-normalization.md)) |
| Text to speech | `KokoroOutput` in `crates/pond-adapters-kokoro` |
| Playback, wake ping, working tone | `crates/pond-audio-out` |

The ear runs `crates/pond-ear` (a hand-written wake word, VAD, echo canceller and Voice Match) on
its own thread; see [pond-ear](pond-ear.md). It is chosen whenever Silero is available and
`vad_backend` is not `rms`. Voice Match is not enrolled by anything yet, so an interrupt today is the
wake word.

## Running it

```bash
cargo run -p pond-server -- chat --voice
```

| Flag | Effect |
|---|---|
| `--voice` | Microphone input; downloads the configured Whisper model if missing |
| `--wake-word <phrase>` | Overrides `voice_wake_word` for this run |
| `--no-wake-word` | Every turn starts listening at once (`InstantActivation`) |
| `--tts kokoro\|none` | `none` prints replies instead of speaking them |
| `--session-id <id>` | Chat session to continue (default `default-session`) |
| `--json-events` | NDJSON on stdout for the desktop shell |
| `-P/--provider`, `-M/--model` | Override the chat provider and model (persisted unless `mock`) |

The banner names each part, or says why it is missing, because a silent assistant and a broken one
look the same from outside:

```
  Goose in a Pond 0.1.0 — voice

  Listen   ggml-base.en
  Wake     "goose"
  Hear     <microphone in use>
  Play     <speaker in use>
  Speak    af_heart @ 1.00x
  Log      <data_dir>/logs/pond.log
```

When no Whisper model resolves, `Listen` says why and the session falls back to typed input (the
default `active_whisper_model` is empty until one is chosen on the Models page). `Speak` reports a missing
Kokoro install instead of downloading it: `chat` never fetches the 92 MB engine, `serve` does.
Silero (2 MB) is fetched here if missing. With a `local`/`gguf` provider the loop then says
"Warming up." and prewarms the prompt prefix (`POND_DISABLE_PREWARM=1` skips it). It always speaks
a greeting, "Hi <name>, ready to take your first request.", before it starts listening.

Say the wake word and keep talking: "Goose, what's the weather in Nairobi?" is one utterance, not a
wake word, a pause and a command.

## One session per device

A voice session owns the microphone and the speaker. A second one does not degrade; it fights,
and the symptoms never name the cause: two voices answering, or a "stream configuration is not
supported" error from whichever lost the race for the input. So every `chat` run takes a lock
first, and a second one refuses to start and names the holder:

```
another voice session (pid 25718) is already running on this device.
...
Stop the other session first — `kill 25718`.
```

`crates/pond-server/src/voice_lock.rs`:

- **Per user, in the temp directory** (`giap-voice-<uid>.lock`), not in the data directory. A
  session on a scratch `POND_DATA_DIR` uses the same microphone, so it must take the same lock.
- **`flock`, not a pidfile.** The kernel releases it when the holder dies, however it dies, so
  there is no stale lock. The PID inside only names the holder.

Under `--json-events` the refusal is an `error` line and `exit: already_running`.

## The turn

| Moment | What the user gets |
|---|---|
| Wake word heard | a two-note ping (C6 then E6, about 220 ms) |
| Working | a soft chime (G5 + E6) every 2.6 s, mostly silence; off with `voice_thinking_tone_enabled = false`. An octave lower, the wake word model took it for "hey goose" |
| Answer ready | the tone stops and the reply is spoken as it streams: a short first clause, then whole sentences |
| Tool calls | shown in the UI (`tool_call`/`tool_result`), never spoken; the tone keeps playing |

There is no spoken filler and no tool narration: both would have to finish playing before the
answer could start.

After a reply that ends with a question, the loop listens again without the wake word (a follow-up
turn). If nothing is heard within 10 s it goes back to waiting for the wake word. After any other
reply it waits for the wake word straight away: a room keeps talking after a reply, and that talk is
not for the pond. Each decision is a `voice_follow_up` trace line.

Control phrases are matched against the whole utterance (`pond_voice::control::classify`):
"bye", "goodbye", "dismissed", "go to sleep", "that's all", "never mind", "stop", "stop listening"
end the conversation with a farewell and return to the wake word; "exit" and "quit" end the
session (`exit: dismissed`). "Stop the kitchen timer" is a request, not a dismissal.

Transcripts pass through `strip_whisper_artifacts`: bracketed and parenthesised tags
(`[BLANK_AUDIO]`, `(music)`), known silence hallucinations ("thank you", "thanks for watching"),
one- and two-character results, and a single word repeated are dropped.

How a turn ends early (wake word, talking over the reply, the desktop's Interrupt, stop) is in
[Voice pipeline interrupt](voice_pipeline_interrupt.md).

## Tuning

### Wake-word detector

`KeywordDetectorConfig` in `crates/pond-adapters-whisper/src/lib.rs`. `run_chat` replaces three of
its fields with settings, so for those the code default never runs; what runs is the setting
(defaults in `crates/pond-core/src/user_data/domain/settings.rs`, editable under
Settings > Voice > Listening).

| Knob | Live value | Comes from | What moving it does |
|---|---|---|---|
| `window_ms` | 1400 | code | Audio transcribed per cycle. Wider gives Whisper more to invent context from and costs more every cycle. |
| `slide_ms` | 200 | code | Reaction floor: the wake word is noticed no sooner than the next slide plus one transcription. |
| `lookback_ms` | 900 | code | How far the capture reaches back past the trigger. Too low clips the first words of every request. |
| `post_trigger_ms` | 12000 | code | Ceiling on the capture after the trigger. Silence normally ends it far sooner. |
| `energy_threshold` | 0.003 | `voice_kws_energy_threshold` | Windows quieter than this never reach Whisper. Raise it if a noisy room keeps waking it. |
| `silence_threshold` | 0.0025 | code | Below this counts as silence after the trigger. Raise it and quiet sentence endings get cut. |
| `post_trigger_silence_ms` | 400 | `voice_kws_post_trigger_silence_ms` | Silence that ends the post-trigger capture. |
| `cooldown_ms` | 2000 | `voice_kws_cooldown_ms` | Settling time before detection arms. The detector never reads audio from before it opened, so the request just heard cannot re-trigger it. The wake word does nothing during it, which also delays a wake-word interrupt at the start of every turn. |

The code defaults for the three settings-driven knobs are 0.010, 800 and 600; the settings defaults
were not retuned with them, and only the settings reach a running pond.

The relationships between knobs are asserted in tests (`the_lookback_covers_the_worst_case_detection_lag`,
`the_ring_holds_everything_both_readers_can_ask_for`,
`ending_a_sentence_is_judged_more_leniently_than_waking_whisper`) against the code defaults. The
silence-under-gate relation also holds for the live values, narrowly (0.0025 under 0.003). The
bound in `the_code_default_cooldown_re_arms_within_a_second` (at most 1000 ms) does not: the live
2000 ms is outside it. Retune freely; a failing test means a relationship broke, not a number.

### Request capture

After the trigger, `WhisperRsInput` keeps recording until end of speech and joins that to the
captured clip. Follow-up turns wait for onset first.

| Parameter | Value | Where |
|---|---|---|
| Onset wait (follow-up turns) | 10 s, onset is RMS >= 0.010 | `DEFAULT_ONSET_WAIT_SECS`, `record_mono_f32_vad` |
| End of speech | 800 ms of non-speech | `DEFAULT_SILENCE_MS` |
| Speech detector | Silero v5, threshold 0.5, 512-sample windows; RMS 0.005 when `vad_backend = "rms"` or Silero fails to load | `build_speech_detector` in `main.rs`, `pond-adapters-silero` |
| Hard cap | 30 s | `DEFAULT_DURATION_SECS` |

Onset stays on the energy gate on purpose: a freshly reset Silero under-reports its first window
and would clip the first word.

## How detection works

- **Whole-word matching.** A window's transcript fires when it contains the wake word or a
  calibrated variant as whole words (`pond_voice::text::contains_trigger`), so "goose" never fires
  on "mongoose". For "goose" and "hey goose" a few built-in mishearings are added
  (`builtin_fuzzy_variants`). Calibration is in [Wake-word calibration](../wake-word-calibration.md).
- **One pass.** A match fires at once; there is no second-window re-check.
- **Lookback.** Detection lags the wake word by a slide plus a transcription, so the capture
  reaches back `lookback_ms` and the wake word is removed from the transcript
  (`strip_leading_wake_word`), not from the audio.

## Two decode profiles

`TranscribeOpts` in `crates/pond-adapters-whisper/src/in_process.rs`:

- **`wake_word()`**: greedy, 2 threads, audio context fitted to the clip. Runs several times a
  second and only has to recognise one known phrase.
- **`accurate()`**: beam search (width 5), the default thread count (2 to 6), full context,
  non-speech tokens suppressed, temperature fallback on low-confidence segments. Runs once per
  turn, and its output becomes the model's prompt.

Measured on `tests/blobs/jfk.wav` with `ggml-base.en.bin`: greedy produced "asked not" and a
spurious music annotation where beam search produced "ask not", and beam search was also faster on
that clip. Reproduce:

```bash
WHISPER_TEST_MODEL="$HOME/Library/Application Support/goose-in-a-pond/models/ggml-base.en.bin" \
  cargo test -p pond-adapters-whisper --features metal decode_profiles -- --ignored --nocapture
```

What each profile keeps resident on the GPU is in [Voice pipeline efficiency](../voice-pipeline-efficiency.md).

## Logs

The console shows warnings and errors only, plus the banner and turn lines (printed directly, not
through tracing). Everything else goes to the daily file `<data_dir>/logs/pond.log.<YYYY-MM-DD>`
(`crates/pond-server/src/tracing_setup.rs`).

The file keeps `pond_adapters_whisper`, `pond_core::shared::services::chat` and `pond_server` at
`debug`: which windows were transcribed, whether they matched, what the transcript was before and
after the wake word came off. A misbehaving session is then already on record.

Third-party native libraries are silenced by target (llama.cpp, ggml, whisper.cpp, ONNX Runtime,
goose, rmcp): they warn about things a user cannot act on. whisper.cpp reaches tracing only
because `install_logging_hooks` is called once when the engine loads. The quiet console filter
repeats those carve-outs, because a bare `warn` directive replaces them rather than adding to
them. `RUST_LOG` overrides both layers.

## Known remaining noise

`clip_model_loader: tensor[N]: ...`, about 1400 lines, when a model with a vision or audio
projector (Gemma 4 E2B, for one) loads. llama.cpp's `clip.cpp` writes these straight to stderr
through `common_log`, not the callback `send_logs_to_tracing` installs, so no filter sees them.
`mtmd_context_params` has a `verbosity` field, but `MtmdContextParams` in llama-cpp-2 0.1.156 (the
fork pinned in the root `Cargo.toml`) does not expose it. Either a fork patch that sets it, or not
loading the projector for voice sessions, would remove them; the second changes image input, so it
is a product decision.
