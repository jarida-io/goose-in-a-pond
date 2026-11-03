# wake-references.bin

The non-wake references every personal wake match is held against (`wake::personal::references`).
A member's phrase must come clearly closer to their own templates than to any of these.

- **What:** 36 short phrases, the ear's fbank frames of each, every frame less its mean over the bins
  and scaled to unit length, stored as `i8` (value times 127). Format `PWR1`, a `u32` count, then per
  phrase a `u32` frame count and 80 bytes a frame. 267,432 bytes, SHA-256
  `061e2260751099fdf10923d73c4d8c1c1a88875b65b374a911c44f28b63aee47`.
- **Source:** Kokoro (Apache-2.0) through voice-match-lab's `tools/kokoro-synth`, `model_quantized.onnx`,
  speed 1.0, voices `am_adam`, `af_bella` and `bf_isabella`. No household audio and no recording of a
  person. The texts, each in all three voices:
  He goes to school. / I saw a goose. / Hey guys. / Hey, you there. / The goose is loose. /
  He knows. / Okay, good. / Who goes there? / Hey, how are you? / Go home now. / Excuse me. /
  Thank you.
- **Remake:** render those 36 clips into `negatives/` of a bench directory, then
  `wake_bench --model ggml-base.bin --silero silero_vad.onnx --bench DIR --write-references
  crates/pond-ear/models/wake-references.bin` (`crates/pond-adapters-whisper/examples/wake_bench.rs`),
  which trims each clip to its loud frames before taking them.
