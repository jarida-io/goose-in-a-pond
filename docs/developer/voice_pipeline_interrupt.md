# Voice pipeline interrupt

How a voice turn ends before its reply finishes. All of it is in `ChatService::run_loop` and
`stream_response_inner` (`crates/pond-core/src/shared/services/chat.rs`); the loop itself is in
[Voice mode](voice-mode.md).

## The race

Each turn runs as a spawned task (`stream_response_inner`) and `run_loop` races it:

```rust
tokio::select! {
    biased;
    chat_result = &mut chat_handle => Raced::Turn(chat_result),   // finished, or barged in
    reason = self.stop_signal() => Raced::Stop(reason),            // desktop `stop` / stdin EOF
    () = &mut interrupted => Raced::Interrupted,                   // desktop `interrupt`
    wake_result = &mut wake_fut => Raced::Wake(wake_result),       // wake word said again
}
```

`biased` puts the finished turn first, so a turn that completed in the same tick as a stop or an
interrupt is persisted rather than abandoned. Nothing is persisted until the turn task returns
normally (`finalize_confirmed_turn`), so every early end leaves no half-turn in history and sends
no `turn_complete`.

| Way it ends | Trigger | What follows |
|---|---|---|
| Wake word | `WhisperKeywordDetector` fires during the turn | Turn abandoned; the new speech is captured (wake-word clip plus the rest of the sentence) and becomes the next request |
| Talking over the reply (barge-in) | Mic level above 0.15 RMS for three 100 ms windows while audio is playing | Turn ends; capture starts from 200 ms before the user began talking, and that becomes the next request |
| `{"cmd":"interrupt"}` (desktop Interrupt button) | `VoiceControl::interrupt` | Turn abandoned; back to waiting for the wake word, nothing recorded |
| `{"cmd":"stop"}` or stdin EOF | `VoiceControl::stop` | Turn abandoned; session ends with `exit: stopped` or `stdin_eof` |

The commands exist only under `--json-events` ([desktop contract](voice-desktop-child-process.md)).

## Wake word

The detector races only if `supports_interruption()` is true. `WhisperKeywordDetector` says yes;
`InstantActivation` (`--no-wake-word`, or no speech model) says no, because it resolves at once
and would abort every turn. The detector sleeps `voice_kws_cooldown_ms` (2000 ms by default)
before it arms, so the wake word cannot interrupt the first two seconds of a turn. It is dropped,
and lets go of the mic, before any follow-up capture starts.

## Barge-in

`watch_for_barge_in` starts when the first speech unit of the reply is queued. It takes a mic hold
(`SpeechEnergy::hold_mic`), so the mic stays open while the reply plays even with no wake-word
detector running, which is why barge-in also works under `--no-wake-word`. It polls
`MicEnergy::recent_rms` every 100 ms (`pond_voice::barge`):

| Constant | Value | Why |
|---|---|---|
| `THRESHOLD_WHILE_SPEAKING` | 0.15 | Above the reply's own echo at ordinary volume |
| `CONSECUTIVE_WINDOWS` | 3 (about 300 ms) | A cough is not a barge-in |
| `ONSET_PRE_ROLL_MS` | 200 | A softly spoken first syllable sits under the threshold |

It listens only while audio is coming out (`VoiceOutput::is_playing`), not between sentences
while a tool runs. Barge-in is wired only with speech input (`with_speech_energy` in `run_chat`);
typed input has none. There is no echo cancellation in this path: loud speakers can still trigger
it, and the follow-up capture can begin with the reply's last words. Headphones avoid both.

## Stopping the audio

`VoiceOutput` (`crates/pond-core/src/models/ports/voice_output.rs`) is the port; `KokoroOutput`
implements it on the audio bus ([audio-bus.md](audio-bus.md)). `begin_utterance` queues the turn
for the bus (`TurnFloor`), so it holds it through its thinking as well as its words, and
`end_utterance`, `stop_speaking` and the next `begin_utterance` give it back; the wake ping takes it
from a turn. `speak()` outside a turn queues as words said whole, and `play_audio()` outside one as
a sample that a turn takes the bus from.

- `begin_utterance()` once per turn bumps a generation counter and clears the interrupt flag, so a
  barge-in stops the whole reply, not one sentence.
- `stop_speaking()` sets `speech_interrupted`. Playback polls it every 50 ms and calls
  `sink.stop()`.
- Audio whose generation is older than the current one is dropped at the next poll, which covers a
  cancel raised just before a new turn cleared the flag.
- `stop_thinking_tone()` ends the working tone; `abandon_turn` also aborts the turn task, which
  drops Goose's stream within a token.

## Speculative replies

On follow-up turns the capture starts a transcription at the first silent frame, before 800 ms of
silence confirms the end of speech, and `listen_with_speculative_chat` starts a reply from it. If
speech resumes, the job is aborted and silenced (`SpeculativeSignal::Invalidated`). If the final
transcript differs from the speculative one, the speculative reply is aborted and a fresh turn
runs, so there is still one persisted turn per request.

## Response time

| Path | Latency to silence |
|---|---|
| Barge-in | about 300 ms of loud audio, plus up to 100 ms poll, plus up to 50 ms playback poll |
| Wake word | next 200 ms slide plus one `wake_word()` transcription of a 1.4 s window (unmeasured), plus up to 50 ms; never within the first `voice_kws_cooldown_ms` of the turn |
| Interrupt / stop command | one stdin line plus up to 50 ms |

## Related

- [Voice mode](voice-mode.md): the loop, tuning, detection
- [Voice pipeline efficiency](../voice-pipeline-efficiency.md): streaming speech and memory
- [Wake-word calibration](../wake-word-calibration.md): what the detector matches
