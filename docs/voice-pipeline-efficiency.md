# Voice pipeline efficiency

What keeps the voice loop quick to answer and bounded in memory, and where each lever lives. The
loop itself, and the detector's knobs and live values, are in [Voice mode](developer/voice-mode.md).

## From wake word to request

| Lever | What it saves | Where |
|---|---|---|
| Energy gate | Windows quieter than `voice_kws_energy_threshold` (0.003) never reach Whisper, so a quiet room costs almost nothing | `detection_loop`, `crates/pond-adapters-whisper/src/lib.rs` |
| Cheap wake decode | `TranscribeOpts::wake_word()`: greedy, 2 threads (so detection cannot starve the request decode of cores), audio context fitted to the 1.4 s clip instead of Whisper's full 30 s | `crates/pond-adapters-whisper/src/in_process.rs` |
| One breath | The capture reaches back `lookback_ms` (900) past the trigger, so "Goose, what's the weather" is one utterance and needs no second recording | `WhisperKeywordDetector` |
| Silence ends captures | 400 ms under RMS 0.0025 ends the post-trigger capture (`voice_kws_post_trigger_silence_ms`); recording then continues until 800 ms of non-speech as judged by Silero, so a pause after the wake word does not cut the request | `detection_loop`, `record_mono_f32_until_silence` |
| Speculative reply | On follow-up turns a transcription starts at the first silent frame and a reply starts from it before the 800 ms confirm; resumed speech or a different final transcript discards it | `record_mono_f32_vad`, `ChatService::listen_with_speculative_chat` |
| Prefix prewarm | With a `local`/`gguf` provider, model load and system-prompt prefill happen before `ready`, not on the first request | `run_chat` (`warm_agent.prewarm`), `crates/pond-server/src/main.rs` |
| Working tone, no filler | The wait is covered by a tone that stops at the first speech unit, not by speech that would have to finish first | `WorkingTone` in `chat.rs`, `start_thinking_tone_thread` in `crates/pond-audio-out` |

## From first token to first sound

| Lever | Detail | Where |
|---|---|---|
| Whole replies | `ReplySplitter` hands Kokoro a reply whole, so Kokoro shapes its pauses; only a reply past one pass (200 tokens, about 11 s of speech) is cut, at the last sentence end that fits ([TTS text normalization](tts-text-normalization.md)) | `crates/pond-voice/src/text.rs` |
| Pipelined synthesis | `SpeechPipeline`: one task synthesizes units, another plays them, so unit N+1 synthesizes while N plays. It measures each unit, and holds a sound that starts from silence until the units still owed can follow it without a gap | `crates/pond-core/src/models/services/voice/speech_pipeline.rs`, used by `stream_response_inner` and `crates/pond-audio-out/src/bus.rs` |
| Kokoro session | Loaded once, on first synthesis (in `chat`, the startup greeting), with ONNX Runtime graph optimization at level 3. The optimized graph is saved beside the model as `<stem>.cache.onnx` and reused while it is newer than the model, so later loads skip optimization | `Engine::load`, `crates/pond-adapters-kokoro/src/engine.rs` |
| Long speech on the GPU | On a Jetson with the GPU runtime, a unit of 100 tokens or more that more speech follows is made by a helper process on CUDA, about ten times faster; short replies stay on the CPU ([below](#long-speech-on-the-gpu)) | `crates/pond-adapters-kokoro/src/gpu.rs`, `kokoro_gpu_speech` in `crates/pond-server/src/main.rs` |
| ORT threads | Intra-op threads = cores minus 2, clamped to 2..6, so speech beats real time without starving the LLM | `default_intra_threads`, `crates/pond-adapters-kokoro/src/lib.rs` |
| Quality tier | The host default is `q8f16`, and `q4f16` on aarch64 Linux, where `q8f16` produces silence and `q8` runs slower than real time; `usable_quality` swaps an unusable tier before loading | `host_default_quality`, `usable_quality` |
| Unload and prewarm | `serve` can drop the Kokoro session to free memory and reload it ahead of need: `POST /api/v1/voice/tts/unload`, `/voice/tts/prewarm`, `GET /voice/tts/status` | `crates/pond-api/src/routes.rs`, `crates/pond-server/src/kokoro_control.rs` |
| One output stream | The speaker is held open across utterances (`Speaker`) and reopened only when the route changes; reopening per sentence degrades CoreAudio over turns | `crates/pond-audio-out/src/lib.rs` |
| DAC wake pre-roll | When nothing has played for 500 ms, 300 ms of silence is played first, so a USB DAC ramping up from idle does not clip the first syllable. Only the first audio after idle gets it: a unit of the same reply after a slow synthesis plays at once | `needs_wake_preroll`, `play` in `crates/pond-audio-out/src/lib.rs` |
| Kokoro's memory | No ORT arena (ort's CPU default; the session also says so) and no memory patterns. Resident memory still rises to the peak of the longest forward pass and stays there: on the Mac, 577 MB after load to 1,062 MB with units under the 200-token budget, 868 MB with units under 120 tokens | `Engine::load_session`, `crates/pond-adapters-kokoro/src/engine.rs`; the ignored `resident_memory_over_fifty_units` test |

## Long speech on the GPU

Kokoro on the CPU takes a little longer to make speech than the speech lasts, so a briefing of
eight units is two minutes of synthesis. On a Jetson with the GPU runtime installed
(`giap.sh onnxruntime build`), long speech goes to the GPU instead.

- **What goes there.** A unit of `GPU_FROM_TOKENS` (100) phoneme tokens or more that is not the
  last of its speech starts the helper, and every later unit of that speech follows it, the last
  one included. A reply of one unit, and a short line before a tool call, stay on the CPU: the
  helper takes longer to start than the CPU takes to say them.
- **Why a helper process.** ONNX Runtime's CUDA provider keeps its memory when a session is
  dropped and returns it only when its process exits. Measured on the Orin Nano on 2026-11-13:
  load and speak held 678 MB, releasing the session gave back 49 MB, a second load added about
  270 MB more. So `pond-server kokoro-gpu-helper --model <model.onnx>` is a child of the server
  (`GpuSpeech`): it loads the full-precision weights, writes a ready mark, then answers each pass
  it is sent on its standard input (token ids, the voice's style row, the speed) with the samples
  on its standard output. The server keeps the phonemizer, the cuts, the gaps and the check for a
  silent pass.
- **When it ends.** After the last unit of the speech, after 20 s with no pass, when a turn
  begins or a reply is interrupted (`begin_utterance`, `stop_speaking`), or when the server closes
  the pipe by exiting. Its memory is back as soon as it has exited.
- **Room for it.** Before it starts, the multimodal encoder is released if no generation holds
  the model (`release_idle_encoders`; the next spoken or picture turn loads it again in about
  2 s), and it starts only with `GPU_NEEDS_MB` (750) available. A released encoder's memory is
  waited for, up to 3 s: it is not yet available when the release returns. Otherwise the speech is made on
  the CPU as before.
- **Out of memory.** The helper sets its own `oom_score_adj` to 1000, so a kernel short of
  memory ends the helper, and the speech carries on from the CPU, before it ends the pond.
- **Failure.** A helper that does not start, exits or gives no audio within 30 s sends that pass
  to the CPU, and the helper is not tried again for ten minutes.
- **The weights.** `model.onnx` (326 MB) is fetched in the background at start, and again
  whenever a long speech finds it missing. The quantised files have no GPU kernels and
  `model_fp16.onnx` comes back as NaN on CUDA.
- **The switch.** `voice_tts_gpu_enabled` (Settings, Voice: "Faster long speech"), read as each
  long speech begins.

| Pass (Orin Nano, 2026-11-13) | Speech | CPU `q8`, 4 threads | CUDA |
|---|---|---|---|
| 53 tokens | 3.3 s | 3.6 s | 0.39 s |
| 107 tokens | 6.4 s | 7.0 s | 0.57 s |
| 178 tokens | 10.7 s | 12.1 s | 0.86 s |

The helper holds about 700 MB with the provider options in `load_on_gpu`, against 1,150 MB on
ONNX Runtime's defaults. A cold helper gave its first audio after 2.6 to 3.9 s alone and 3.9 to
6.8 s beside the running pond. While the language model decodes at the same instant as a pass, it
runs at about half speed (13 against 25 tokens a second); a helper that is loaded and quiet costs
it nothing.

Logs: `kind="kokoro_gpu_started"` (`load_ms`, `available_mb`), `kokoro_gpu_stopped` (`reason`:
`speech_ended`, `idle`, `cut_short`, `switched_off`, `failed`; `passes`, `held_ms`),
`kokoro_gpu_skipped` (`reason`: `memory`, `model_missing`), `kokoro_gpu_failed`,
`kokoro_gpu_weights_ready`, `kokoro_gpu_weights_failed`, and the encoder's own
`encoder_released` with `reason="room_wanted"`.

## When the GPU has no room for a transcription

In-process Whisper (`pond-adapters-whisper`, built with its `cuda` feature on the Jetson) keeps the
model weights on the GPU from startup, and keeps one `whisper_state` per decode profile (`Engine`
in `in_process.rs`). The accurate profile's state is made at load and decodes a second of silence
there, which sizes its beam-search cache and reserves GGML's CUDA scratch pool. The wake-word
profile's state is made on its first use. After that a transcription allocates nothing, so a GPU
the LLM has filled does not fail it: the ignored CUDA test `a_kept_state_transcribes_on_a_full_gpu`
fills the GPU with states and still transcribes.

A state made per call would allocate on every call, and two of those allocations failing took the
Pond down:

- **Growing the beam-search cache.** whisper.cpp freed the caller's state, a double free and a
  `SIGBUS`. Fixed in the vendored source; see `vendor/whisper-rs-sys/POND-PATCH.md`.
- **Reserving the scratch pool on a state's first decode** (`cuMemAddressReserve` in
  `ggml-cuda.cu`). GGML aborts there by design. A kept state reserves its pool once, at load.

### What a state costs (measured on the Jetson with `base`, 2026-10-05)

One state is 170.6 MB:

| Part | Size |
|---|---|
| KV caches: self, cross and pad | 6.29 + 18.87 + 3.15 MB |
| Compute buffers: conv, encode, cross and decode | 17.24 + 23.09 + 4.66 + 97.29 MB |

Five-beam decoding grows the self cache to seven decoders' worth, 44.03 MB, so the accurate state
holds about 208 MB plus its scratch pool. That much was needed while any transcription ran anyway;
keeping it means it is not handed back to the LLM between calls. A pond serving only the
transcribe route holds just the accurate state; local voice mode adds the wake-word state, another
170.6 MB. Preparing the state adds about 0.7 s to startup.

### Logs

| `kind` | Meaning |
|---|---|
| `whisper_state_prepared` | The accurate state was made at load (`elapsed_ms`) |
| `whisper_state_created` | A state was made, with a running count; a count that keeps rising means states are being lost to panics and the allocations are back on the call path |
| `whisper_state_not_prepared` | Preparing at load failed; the first transcription will make the state, and can then fail with no room |
| `whisper_transcription_failed` | A transcription failed, typically next to Whisper's `failed to init ... allocator` and `failed to reserve graph buffers` |

On a failure the transcribe routes answer 500 with the reason and the wake-word loop moves to its
next window. Calls with the same profile share one state, so they run one at a time.

Whisper does not fall back to the CPU. On the Orin the CPU and GPU share memory, so a CPU retry
competes for what just ran out, and a CPU-resident second context would hold its weights all the
time to cover a failure that clears when the LLM's turn ends. If failures recur, shrink the LLM's
context, not Whisper.
