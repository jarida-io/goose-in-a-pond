# pond-ear

`crates/pond-ear` is the ear: what the voice child needs to hear, on one audio front end.
It handles the "hey goose" wake word, Silero voice activity and endpointing, Voice Match (who is
speaking), and an echo canceller fed the exact audio the pond plays. The code is hand-written Rust
with std only and no crates. It is a port of the voice-match-lab engine, minus training, the lab
website and Moonshine. Speech-to-text stays Whisper.

`crates/pond-adapters-ear` runs it behind the voice loop's ports, and `chat --voice` uses it
wherever Silero is available and `vad_backend` is not `rms` (`build_listening` in
`crates/pond-server/src/voice_rig.rs`). Room voice inside serve, and Voice Match enrolment, are
not built yet. [The wiring](#wiring-it-into-the-voice-loop) is at the end.

```
mic 16 kHz -> echo canceller -> 32 s ring + 80-dim fbank every 10 ms -> wake CNN (every 80 ms)
     ^ play(at, pcm)                       |                         -> Silero (every 32 ms) -> endpoint rule
     the PCM being played                  +-------------------------> ResNet34 on a span   -> Voice Match
```

## Map

| module | what |
|---|---|
| `engine` | Kaldi fbank (f64 512-point FFT), libm-free `ln`/`exp`/`sqrt`/`erf`, conv kernels on three rungs (scalar f32, NEON f32, int8 `SDOT` in inline asm), the op-program executor over one caller-owned arena, Silero v5 by hand with the official 64-sample context, and a general f32 real FFT |
| `convert` | our own ONNX protobuf reader (subgraphs included), converters for WeSpeaker ResNet, the wake CNN and Silero, the `.vmw` writer, SHA-256, `ModelFile` (validated weights in aligned memory) |
| `wake` | window constants, the embedded "hey goose" model, `Detector` (the self-contained reference) |
| `ear` | `Ear` (state machine), `Front`, the parts, `Endpointer`, `Aec`, events, OS memory readings |
| `score` | Voice Match's decision, term for term the face matcher's: centroid + top-3, S-norm, margins, calibration, FAR/FRR/EER |
| `dsp` | the pond's capture resampler, a streaming band-limited `Resampler` for the playback tap, the energy quality gate |
| `wav`, `locations` | WAV in and out; where the pond keeps the ear's models |

The `pond-ear` binary has three commands. `measure` reports RAM in a fresh process, `replay`
runs a WAV through the ear (`--far` plays a second WAV as the far end), and `convert` turns ONNX
into `.vmw`.

## What it costs

RAM per part, in a fresh process on the Mac (`pond-ear measure`, 2026-10-16), against the OS's
own reading. Each part is counted as weights plus arena plus state. "Rung file" is what a weights
file holding only that rung's tensors would cost.

| part | weights | arena | state | scalar / NEON f32 (rung file) | int8 SDOT (rung file) |
|---|---|---|---|---|---|
| echo canceller (160 ms tail, 500 ms delay search) | - | - | 0.54 MB | 0.54 MB | 0.54 MB |
| front end (32 s ring + fbank stream) | - | - | 3.07 MB | 3.07 MB | 3.07 MB |
| wake CNN (hey goose) | 0.37 MB | 1.29 MB | 0.12 MB | 1.71 MB | 1.51 MB |
| Silero v5 | 1.24 MB | 0.02 MB | 0.00 MB | 1.26 MB | 1.26 MB |
| Voice Match (ResNet34-LM, 3 s span) | 31.87 MB | 8.79 MB | 0.10 MB | 35.41 MB | 19.50 MB |
| **ear** (as loaded: 45.77 MB) | | | | **40.35 MB** | **24.25 MB** |
| the OS: footprint grew by | | | | 45.50 MB (99%) | 46.43 MB (101%) |

The front end row is computed, not measured: the ring grew from 15 s to 32 s, so that the
endpointer's 30 s longest utterance still has its start in the ring when it ends; the rest of the
table is the 2026-10-16 measurement, when the ring was 15 s and the ear held 1.63 MB less. The ear
without Voice Match holds 6.6 MB. CPU on a 30 s synthetic replay with 16 s of far end
was measured on a heavily loaded M4 (load average 26), so these figures are high. On the int8
rung the wake CNN took 2.3% of one core, Silero 0.6%, the echo canceller 0.9% (about 1.7% while
audio plays) and the front end 0.1%. On the scalar rung, the only one on x86, the wake CNN took
11.8%. Voice Match runs only on a wake or a barge-in: 3.2 GMAC for a 1.4 s wake segment.

Load `.vmw` files in a long-lived process, not ONNX. A Silero ONNX parsed in-process grew the footprint
16.5 MB for 5.0 MB of model, because the parse widens tensors to f64 and fragments what it frees.
`convert::ensure_vmw` converts once and caches the `.vmw` beside the ONNX. The embedded wake model
is the exception: it is small, so converting it at load costs about 10 ms and no measurable RAM.

## The embedded wake model

`models/hey-goose.onnx` has SHA-256 `32ae1703...f699`. It was trained in the lab on Kokoro voices
only (Apache-2.0): the pond's 12 English voices, 5 accented ones and 12 blends, with synthetic rooms
and noise. No household audio and no macOS voice went into it. Its threshold, 0.97, was chosen by
streaming the detector as the ear does on held-out validation voices: the highest recall with at most
one false accept per hour of keyword-free speech. At that threshold it catches 72% of validation and
54% of test phrases in context, against 43% and 21% for the earlier macOS-trained model at 0.98.
Recipe, evaluation and numbers are in
[`crates/pond-ear/models/hey-goose.md`](../../crates/pond-ear/models/hey-goose.md).

ResNet34 and Silero are not embedded. The crate takes paths or bytes, and `locations` names where
the pond keeps them: `models/silero/silero_vad.onnx`, which the voice child already downloads
(`ensure_silero_model`), and `models/voice-match/resnet34_LM.onnx`, which serve fetches by default
for Voice Match (see [Voice Match](../voice-match.md)).

### Credit: the Voice Match model

Voice Match runs WeSpeaker's ResNet34-LM speaker model (wenet-e2e/wespeaker, Apache-2.0 code),
trained on VoxCeleb. WeSpeaker publishes its VoxCeleb models under the dataset's
licence, Creative Commons Attribution 4.0 International (CC BY 4.0); the copyright of the VoxCeleb
audio stays with the owners of the original videos. Whatever ships it must carry the credit:

> Speaker embeddings by WeSpeaker ResNet34-LM (H. Wang et al., "WeSpeaker: A Research and
> Production oriented Speaker Embedding Learning Toolkit", ICASSP 2023), trained on VoxCeleb
> (A. Nagrani, J. S. Chung, A. Zisserman, "VoxCeleb: a large-scale speaker identification
> dataset", Interspeech 2017; J. S. Chung, A. Nagrani, A. Zisserman, "VoxCeleb2: Deep Speaker
> Recognition", Interspeech 2018). Licensed under CC BY 4.0,
> https://creativecommons.org/licenses/by/4.0/.

## Segments for a host-side wake detector

With `Config.segments` on, the wake CNN never runs. Outside Listen the ear instead reports each
stretch of speech on the echo-cancelled signal as `Event::Segment { start, end, cut }`, for the host
to transcribe and match against a custom phrase. A segment uses the endpointer's rule (Silero's
hysteresis, 800 ms of quiet), reaches 200 ms back before Silero's onset and is dropped under 250 ms
of voiced time. It is cut at `Endpoint::segment_max_ms` (8 s) with `cut` set while the speech goes
on: the host reads only a segment's opening, and a television or a room of talk otherwise ran a
segment to 30 s and hid a wake past its opening. A wake in a cut segment listens from the wake's
start, so the request is endpointed whole. Listening cancels the segment in progress, since its
audio belongs to the turn. `tests/replay.rs` checks segments come out bit for bit the same for any
chunking and never while listening.

## Echo canceller

`ear::Aec` takes the 16 kHz PCM being played, stamped with the microphone's sample clock
(`play(at, pcm)`). Stamps within 10 ms of where the last push ended are treated as continuous
playback. It works in 8 ms blocks:

- **Bulk delay.** GCC-PHAT runs on 4 kHz copies of both streams, with a 256 ms microphone window,
  a 0..500 ms search and an averaged phase-only cross-spectrum. A peak must stand 6x above the mean
  correlation on two consecutive updates. The filter then starts 16 ms before the direct path. A
  delay change shifts the learned filter by whole blocks rather than discarding it.
- **Adaptive filter.** This is a partitioned-block frequency-domain adaptive filter (MDF), 20
  partitions covering 160 ms of room tail. The gradient constraint is alternated over the
  partitions, and the step is proportionate per partition.
- **Step control.** The learning rate follows Valin, "On adjusting the learning rate in frequency
  domain echo cancellation with double-talk" (IEEE TASLP, 2007), the scheme in Speex's MDF. Per
  bin, the step is the share of the error that is residual echo, from a leak regression of
  residual power on echo power. Near-end speech therefore stops adaptation by itself.
- **Two-path.** A background filter adapts. The foreground, which produces the output, is replaced
  only when the background is significantly better, and the background is reset when it is
  significantly worse. If the foreground makes the microphone louder than no filter at all for
  100 ms, the room has changed: both filters are dropped and relearned at the fast initial rate.
- **Residual suppression.** A sqrt-Hann STFT applies a Wiener-like gain from leak x |echo
  estimate|^2, floored at -30 dB, falling at once and recovering gently.
- **Pass-through.** While no far-end audio is in the filter's span, the microphone passes through
  bit for bit, 16 ms late. The ear's wake scores with the canceller in the path are therefore
  identical to the reference detector's.

Synthetic room, from `tests/aec.rs`: echo at -17 dBFS with 40 ms delay and T60 0.25 s, a talker
at -23 dBFS over it from 6 to 10 s, then a new room at 13 s (110 ms delay, T60 0.30 s).

| | linear filter | with suppression |
|---|---|---|
| ERLE over 3-6 s (far end alone) | 32.9 dB | 51.6 dB |
| time to a sustained 20 dB | 2.4 s (delay locked after 1.0 s) | |
| double talk 6-10 s, residual echo | 35.5 dB | near talker kept at 18.2 dB SNR |
| filter misalignment before / after double talk | -36.7 / -35.3 dB | |
| far end alone again, 10-13 s | 36.9 dB (worst 100 ms: 26.0 dB) | 49.9 dB |
| after the path change: delay relocked / 20 dB again | 0.5 s / 2.9 s | |
| ERLE over 17-20 s | 28.0 dB | 38.9 dB |

In the ear (`tests/replay.rs`, real Silero), the reply's own echo never barged in once the room
was learned. A voice talking over the reply was reported 0.67 s after it began. Without the
canceller, Silero read the echo as speech (p of about 1.0) the whole time. The weak spot is the
first second of the first reply in a session: two false barge-ins while the room was being
learned. They are harmless under the interrupt rule, which needs an enrolled voice or the wake
word.

## Tests

`cargo test -p pond-ear`: 42 unit tests, 2 echo-canceller tests, 5 parity tests and 6 replay
tests. The audio in all of them is synthetic, from formulas in `tests/common/mod.rs`. No
recording of a person is in the repository.

- **Parity.** The goldens in `tests/golden/` come from kaldi-native-fbank and ONNX Runtime on the
  same formula inputs. Regenerate them with
  `python3 crates/pond-ear/oracle/golden.py --silero <silero_vad.onnx> --resnet <resnet34_LM.onnx>`
  (numpy, onnxruntime, kaldi-native-fbank). Measured worst errors:

  | check | result |
  |---|---|
  | fbank vs Kaldi | 1.8e-4 |
  | hey goose logits, f32 rungs | 3.3e-6 / 1.2e-6 |
  | hey goose logits, int8 | 0.07, no decision flips |
  | Silero, every rung | 8.3e-7, 0 flips in 125 chunks |
  | ResNet34 (1 - cos), f32 rungs | 2.8e-12 / 3.6e-12 |
  | ResNet34 (1 - cos), int8 | 3.6e-3 |

  The converter writes the lab's `.vmw` bytes exactly for all three models.
- **Skipping.** Silero and ResNet34 tests use the pond's copies (or `POND_EAR_SILERO`,
  `POND_EAR_SPEAKER`). Without them they print why and pass. CI has neither, so the replay tests
  there run on seeded Silero- and ResNet-shaped models, which exercise the same arithmetic and
  plumbing.
- **Replay.** Thirty seconds of voices, noise and a reply with its echo, plus host actions, give
  the same 35 events bit for bit for chunk sizes 1, 7, 511, 1600 and 50000. They also give the
  same events after a reset, and the same hops and triggers as `Detector`.
- **Endpoint and RAM.** The endpoint rule is tested on probability streams, and with Silero on
  synthetic speech, where it fired 796 ms after the speech ended. The RAM accounting is checked
  against the OS in a fresh process.

## `.vmw` layout

```
0    "VMW1", u32 version, u32 n_sections, u32 n_ops
16   u32 arch, u32 frontend, u32 emb_dim, u32 n_slots, u32 feat_dim, u32 converter
40   [u8; 32] SHA-256 of the source ONNX
72   n_sections x { [u8; 8] name, u32 offset (64-aligned), u32 length }
...  "ops" (32-byte records), "w32" (f32 weights, biases, int8 scales and sums), optional "i8w"
```

## Wiring it into the voice loop

| Step | Status | Where |
|---|---|---|
| **pond-adapters-ear** runs one `Ear` on its own thread with a permanent mic claim, read through `MicReader`. It serves `ChatService` as its `StreamingWakeWordDetector` (the activation is the endpointed request from `Ear::audio`, so nothing is recorded twice), its `VoiceInput` (`listen(start)`, then `Utterance` or `Silence`) and its `InterruptWatch`. The interrupt rule lives there: the wake word, or a `BargeIn` with an enrolled `who` | built | `crates/pond-adapters-ear/src/worker.rs` |
| **Whisper as the wake detector**: with `Config.segments`, each `Segment`'s first 6 s is transcribed (`Transcriber::scan`: the accurate profile without temperature retries) and matched against `wake_triggers`; a match is the wake and its transcript the request. Room Whisper reads the wake phrase and the members' names as its prompt (`speech_prompt::room_prompt`), and a transcript that is only that prompt said back counts as nothing | built | `match_segments` and `wake_in` in `crates/pond-adapters-ear`, `WhisperTranscriber` in `crates/pond-server/src/voice_rig.rs` |
| **pond-audio-out playback tap**: every played source copies what the mixer pulls to a process-wide tap; the ear resamples it to 16 kHz, stamps it with the mic's clock plus 10 ms, and passes it to `Ear::play` before the mic samples it overlaps | built | `crates/pond-audio-out/src/tap.rs`, `crates/pond-adapters-ear/src/reference.rs` |
| **The host** moves the ear through `Think` and `Speak` and back to `Wait` from `ChatService`'s state observer, and calls `listen(start)` after an interrupt | built | `EarRig::state_hook`, `ChatService::with_state_observer` |
| **Room voice in serve**, sharing serve's model, Whisper and Kokoro; the desktop attaches to it | built | `crates/pond-server/src/room_voice.rs`, `pond-desktop/electron/main/voice/roomClient.ts` |
| **Models**: Silero as `.vmw` through `ensure_vmw` (`load_vad`); ResNet34 fetched by default to `models/voice-match/` (pinned revision and SHA-256) and converted once (`load_speaker`) | built | `crates/pond-server/src/voice_match.rs`, `pond_core::models::domain::voice_match_model` |
| **Enrolment**: one flow of five takes teaches the wake word and Voice Match (`POST /voice/takes`): Whisper records the phrase as heard, an ear of its own (`TakeEar`) scores the take with the CNN and embeds up to 12 s with `Ear::embed`; the audio is never kept | built | `crates/pond-adapters-ear/src/enrol.rs`, `crates/pond-api/src/voice_match.rs` |
| **Recognition in the room**: the room's ear loads ResNet34 once someone has five prints, gets `set_household` at start and on every change, and names the waker on CNN wakes and Whisper segment wakes alike; after a recognised wake, a follow-up in another enrolled member's voice is dropped, and one Voice Match does not recognise continues | built | `crates/pond-server/src/room_voice.rs`, `EarRig::listen`, `Worker::speaker_of` |
| **Echo**: a Whisper segment wake whose words are mostly, in order, what the pond said this turn or in the last 10 s is dropped (`ear: own echo ignored`) | built | `crates/pond-adapters-ear/src/echo.rs` |
