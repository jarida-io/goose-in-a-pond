# Spoken pairing code clips

A pond reads a pairing code aloud when a phone asks (`POST /api/v1/handshake/spoken-code`). With a
voice installed it uses that voice; a fresh pond may not have one yet, so these clips are compiled
into `pond-server` (`src/spoken_code.rs`) and played instead: `intro`, the six digits, `again`, the
six digits.

| File | Says |
|---|---|
| `intro.wav` | "Your Goose pairing code is" |
| `again.wav` | "Once more" |
| `0.wav` to `9.wav` | "zero" to "nine" |

- **Model:** Kokoro-82M v1.0, ONNX export `onnx-community/Kokoro-82M-v1.0-ONNX`, full-precision
  `model.onnx`. Licence: Apache-2.0 (model and voices).
- **Voice:** `af_heart`, the voice a pond uses when none is configured
  (`pond_adapters_kokoro::DEFAULT_VOICE`), at speed 1.0.
- **Format:** mono, 16-bit PCM WAV, 24 kHz (the generator drops to 16 kHz only if the set would
  pass 500 KB), trimmed to 25 ms either side of the speech and peak-normalised to 0.9.
  `spoken_code::tests` checks each clip decodes, is mono and lasts 0.2 s to 3 s.

## Regenerating

The generator reads the model, the voice, the espeak-ng data and the ONNX Runtime from a pond's
data directory, and writes only here:

```bash
cargo run -p pond-server --example make-spoken-code-clips
# or name the inputs:
cargo run -p pond-server --example make-spoken-code-clips -- \
  --data-dir "$HOME/Library/Application Support/goose-in-a-pond" \
  --voice af_heart --quality fp32 --out crates/pond-server/assets/spoken-code
```

It needs `<data>/models/kokoro/{model.onnx,tokenizer.json,voices/af_heart.bin}`,
`<data>/bin/espeak-ng-data` and `<data>/lib/libonnxruntime*` (or `ORT_DYLIB_PATH`), which a pond
that has spoken once already has. Listen to the result before committing it.
