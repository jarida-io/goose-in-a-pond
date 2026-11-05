# Wake-word calibration

The live wake word is Whisper transcription matching: `WhisperKeywordDetector`
(`crates/pond-adapters-whisper/src/lib.rs`) transcribes a 1.4 s window every 200 ms and fires
when the transcript contains a trigger as whole words. Calibration teaches it how Whisper hears
the phrase in this household's voices: the user reads a few sentences containing the phrase, and
the server stores the phrase *as heard* (a mishearing such as "gus" for "goose") as an extra
trigger.

Where the ear runs ([pond-ear](developer/pond-ear.md)), Whisper keeps the wake word but reads
only the speech segments the ear endpoints on echo-cancelled audio, never a window on a timer,
and matches them against the same triggers (`pond_voice::text::wake_triggers`). The ear's "hey
goose" CNN is used only when `voice_wake_detector` is `hey_goose`, or `auto` once a calibration
shows the CNN hears the household; nothing records that result yet, so `auto` means Whisper.
Calibration has no effect on the CNN.

## What the detector matches

```
mic ring (16 kHz) -> 1.4 s window every 200 ms -> energy gate (voice_kws_energy_threshold)
  -> Whisper, wake_word() profile -> strip_whisper_artifacts -> normalize_transcript
  -> contains_trigger(transcript, t) for any trigger t -> ping, capture, transcribe the request
```

Triggers, built by `WhisperKeywordDetector::new` and `with_transcriptions`:

1. `voice_wake_word`, normalized (lowercase letter-only words). Always a trigger.
2. Each stored variant in `voice_wake_word_transcriptions`, reduced to the phrase as heard by
   `extract_wake_variant` (a variant stored as a whole sentence is cut down when the detector
   loads).
3. For "goose" and "hey goose" only, built-in mishearings (`builtin_fuzzy_variants`): "goos",
   "gooes", "gus", "gooch", "hey goose", "a goose", "the goose" for "goose"; "hey goos",
   "hey gus", "a goose", "hey gooch" for "hey goose".

Matching is whole words (`pond_voice::text::contains_trigger`), so "goose" never fires on
"mongoose". There is no second-window re-check. The same triggers are stripped from the start of
the request transcript (`strip_leading_wake_word`).

## How a take becomes a variant

1. The client shows a sentence from `calibration_prompts(phrase)` (`crates/pond-voice/src/text.rs`;
   the desktop mirror is `buildCalibrationPrompts` in `pond-desktop/src/modes/voice/wakePhrase.ts`):
   the phrase alone, "Hey <phrase>, good morning", "OK <phrase>, what time is it",
   "<phrase>, tell me a joke", "Thank you <phrase>".
2. The take is transcribed with the `accurate()` profile (beam search).
3. `extract_wake_variant` aligns the transcript with the sentence word by word (edit-distance word
   cost, gaps cost 1) and keeps the transcript words standing where the phrase stands. "Hey gus,
   good morning" stores "gus", not "good", which spells closer to "goose".
4. The result is dropped if it is further than 0.67 from the phrase (word edit distance over the
   longer length): "gus" for "goose" is 0.6 and kept, "bruce" is 0.8 and dropped. Only the phrase
   is stored, never the sentence or the audio.

The detector itself transcribes with the cheaper `wake_word()` profile (greedy, 2 threads), so a
stored variant comes from a different decode than the one it is matched against.

## API

### `POST /api/v1/voice/calibrate`

One take. Call it once per sentence, about five times. Exposure `UntilOnboarded`
(`crates/pond-api/src/middleware/mod.rs`): tokenless from loopback while the pond is neither
claimed nor onboarded, otherwise it needs a token.

Request: `multipart/form-data`.

| Field | Required | Description |
|---|---|---|
| `audio` | yes | WAV (16-bit mono 16 kHz recommended; other WAV layouts are decoded and resampled) |
| `prompt` | no | The sentence read out; locates the phrase in the take. Without it every calibration sentence is tried and the best alignment wins |

Response `200 OK`:

```json
{
  "transcript":   "Hey gus, good morning.",
  "normalized":   "hey gus good morning",
  "heard":        "gus",
  "new_variant":  true,
  "all_variants": ["gus"],
  "sample_count": 1,
  "target_count": 5,
  "complete":     false
}
```

| Field | Description |
|---|---|
| `transcript` | Raw Whisper transcript of the take |
| `normalized` | The whole transcript normalized (not what is stored) |
| `heard` | The phrase as heard; `null` when the take did not contain it |
| `new_variant` | Whether `heard` was new and was stored |
| `all_variants` | Every stored variant |
| `sample_count` | Stored variants, not takes |
| `target_count` | Takes to ask for (5) |
| `complete` | `sample_count >= target_count`; kept for older clients |

Hearing the phrase correctly every time stores nothing new, so a client counts its own successful
takes (non-null `heard`) rather than `sample_count`.

Transcription is in process (`AppState::transcribe_audio`, the same Whisper model as `/transcribe`).
Only when no Whisper model loaded at startup does the route post to `voice_whisper_url`
(`/inference` on a whisper.cpp server), gated by the network mode.

| Status | Reason |
|---|---|
| 400 | Missing `audio` field or a multipart error |
| 422 | No speech in the recording |
| 500 | In-process transcription failed, or settings read/write failed |
| 502 | External fallback only: egress denied, server unreachable, or a bad response |

### `DELETE /api/v1/voice/calibrate`

Clears every stored variant. The detector keeps `voice_wake_word` and the built-ins. Same
exposure. Response: `{"cleared": true, "message": "Wake-word calibration data cleared"}`.

## Clients

- **Desktop app.** `WakeWordCalibration` (`pond-desktop/src/components/WakeWordCalibration.tsx`),
  mounted from the settings catalogue. It records 3.5 s per take (`recordFixedDuration` in
  `micRecorder.ts`), sends each with its sentence through `api.calibrateWakeWord(wav, prompt)`,
  and resets with `api.resetWakeWordCalibration()` (`pond-desktop/src/api/PondApiClient.ts`).
  The hub's Voice settings view (`pond-desktop/src/hub/views/settings/Voice.tsx`) shows the
  stored variants and a reset.
- **`pond-server calibrate`** (`run_calibrate` in `crates/pond-server/src/main.rs`):

  ```bash
  pond-server calibrate [--phrase "hey pond"] [--samples 5] [--reset]
  ```

  It loads the in-process Whisper model named by `active_whisper_model` from
  `<data_dir>/models/`, opens the pinned microphone (`audio_input_device`), and for each sample
  waits for Enter, records until silence and stores the variant exactly as the API does, with the
  phrase itself as the prompt (say the phrase, not a sentence). `--phrase` also saves
  `voice_wake_word`. A repeat of a stored variant counts as a sample. It writes the same settings
  the live detector reads, so it targets the live wake path.

  `--whisper-url` is still accepted and does nothing: transcription is in process. The command
  does not take the voice lock ([Voice mode](developer/voice-mode.md#one-session-per-device)), so
  stop any running voice session first or the two will contend for the microphone.

Calibration is optional: the detector always fires on the wake word itself.

## Settings

Stored in `pond_system.db` (flat settings table), writable with `PUT /api/v1/settings`.

| Setting | Default | Validation | Use |
|---|---|---|---|
| `voice_wake_word` | `"goose"` | 1 to 4 words, at most 40 characters, letters, spaces, apostrophes and hyphens only (`Check::WakePhrase`) | The phrase; always a trigger |
| `voice_wake_word_transcriptions` | `[]` | | Calibrated variants |
| `voice_kws_energy_threshold` | `0.003` | UI: 0 to 1 | Windows below this RMS skip Whisper; `0` disables the gate |
| `voice_kws_post_trigger_silence_ms` | `400` | UI: integer >= 0 | Silence that ends the post-trigger capture; `0` always runs to the 12 s ceiling |
| `voice_kws_cooldown_ms` | `2000` | UI: integer >= 0 | Delay before detection arms each time it starts |
| `voice_recording_duration_secs` | `3` | 1 to 300 | Browser voice path only; the voice loop ends on silence |

The settings UI groups these under Settings > Voice > Wake word and Settings > Voice > Listening.
The full detector table, including the knobs that are code constants, is in
[Voice mode](developer/voice-mode.md#tuning).

### Setting the energy gate

Each window skipped by the gate is logged at `trace`:

```bash
RUST_LOG=pond_adapters_whisper=trace cargo run -p pond-server -- chat --voice
```

```
TRACE pond_adapters_whisper: KWS: silent window skipped (rms=0.0082)
```

Stay quiet for ten seconds, note the highest idle RMS, and set `voice_kws_energy_threshold` about
20% above it. Then speak and check that windows reach Whisper (`KWS window: "..."` at `debug`,
which the log file records by default).

### Cooldown

The detector sleeps `voice_kws_cooldown_ms` every time it starts, including at the start of each
turn, where it races the reply. The wake word cannot interrupt a reply during that time. The
startup banner shows the phrase and how many variants are stored:

```
  Wake     "goose" (2 calibrated variants)
```
