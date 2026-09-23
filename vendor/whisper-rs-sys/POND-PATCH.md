# Whisper GGML isolation

This directory vendors the published `whisper-rs-sys` 0.15.0 crate, including its
original licensing and native sources. It is selected by the root Cargo patch.

The Jetson's static CUDA build links both Whisper and llama.cpp. Both libraries
embed different GGML revisions and exported the same symbols. The old link failed
with 533 duplicate definitions; permitting duplicate definitions would also let
one engine call the other engine's incompatible implementation.

On Linux, `namespace.rs` scans the pinned native sources and generates a forced
include that gives Whisper's GGML/GGUF, C++ `ggml` namespace, CUDA sum-row helper,
quantization, block types and IQ helpers a
`pond_whisper_` prefix. C, C++ and CUDA receive the same header. Bindgen preserves
the Rust API and assigns explicit private link names to corresponding FFI items.
Generation must succeed; Linux cannot fall back to bindings that name public GGML
symbols. Dynamic backend loading is disabled because it looks up public symbol
names; CPU and CUDA backends remain statically registered. Other platforms retain
the upstream build behavior.

Changes from the published crate: this note, `namespace.rs`, build-script wiring,
a standalone Cargo workspace declaration, and the upstream Unlicense text
(restored from the whisper-rs repository because the published crate omitted it). Native source files are unchanged.
When updating Whisper, regenerate and inspect the linked symbol inventory and run
both GPU transcription and inference in the same production process before shipping.
