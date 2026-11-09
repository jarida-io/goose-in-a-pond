# Changing the process environment

**Rule:** once the server runs threads, change an environment variable only with
`pond_core::shared::services::process_env::set` or `remove`, and call into a C library that reads
the environment on its own (ALSA, through cpal and rodio) inside `process_env::reading`. The test
`crates/pond-core/tests/environment_changes.rs` fails on a raw `std::env::set_var` or `remove_var`
in production code; tests may still set variables of their own.

## Why

glibc's `setenv` grows the environment by reallocating the array it made the time before, and
that frees the old array. Rust's `std::env::var` takes std's lock, but C code calls `getenv` and
walks `environ` with no lock at all. If it does so while another thread adds a variable, it reads
freed memory.

On the Jetson this crashed the pond with SIGSEGV on about one start in five (2026-11-09). A core
dump showed the crashing thread in `getenv("ALSA_CONFIG_PATH")`, under
`snd_device_name_hint`: the speaker route was listing ALSA devices while the Goose adapter set its
context and compaction knobs (`GOOSE_CONTEXT_LIMIT` and the rest) during start-up. No thread was
still in `setenv` when it died; the writer had finished and freed the array the reader was still
walking.

Measured on the Jetson the same day: before the fix, 3 of 15 starts crashed (2 of 9 ordinary
starts, then the 6th of a restart loop, whose core dump is the one above). With the fix, 25 starts
in a row came up with no crash.

## How it works

`process_env` holds one `RwLock`. `set` and `remove` take it for writing, `reading` for reading,
so a guarded C call and a change never overlap. Do not nest `reading`, and never `set` inside it:
std's lock lets a queued writer block a second read, so a thread would wait for itself.

The guarded calls are where the pond reaches ALSA: the speaker route (`pond-audio-out`, default and
named outputs, opening a stream), the microphone (`pond-audio`, finding and opening the device),
the device prober and probes and the sound test (`pond-adapters-audio-devices`), and the spoken
setup code's speaker check (`pond-server`).

## What it does not cover

Other C libraries read the environment too: ONNX Runtime, CUDA, the DNS resolver. They are not
guarded, so a change at run time can still race them. The runtime changes left are few: the Goose
knobs (only when they differ from the last set), a provider rebuild, and Kokoro's espeak data path
when the voice is first built. Moving those out of the environment into Goose's own configuration
is the complete fix, if one of them is ever seen in a crash.
