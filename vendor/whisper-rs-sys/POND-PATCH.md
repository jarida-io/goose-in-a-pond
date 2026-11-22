# Whisper GGML isolation

This directory vendors the published `whisper-rs-sys` 0.15.0 crate, including its
original licensing and native sources. It is selected by the root Cargo patch.

The Jetson's static CUDA build links both Whisper and llama.cpp. Both libraries
embed different GGML revisions and exported the same symbols. The old link failed
with 533 duplicate definitions; permitting duplicate definitions would also let
one engine call the other engine's incompatible implementation.

On Linux and macOS, `namespace.rs` scans the pinned native sources and generates a forced
include that gives Whisper's GGML/GGUF, C++ `ggml` namespace, CUDA sum-row helper,
quantization, block types, IQ helpers and the Metal backend's Objective-C class a
`pond_whisper_` prefix. C, C++, Objective-C and CUDA receive the same header. Bindgen preserves
the Rust API and assigns explicit private link names to corresponding FFI items.
Generation must succeed; an isolated build cannot fall back to bindings that name public GGML
symbols. Dynamic backend loading is disabled because it looks up public symbol
names; CPU, CUDA and Metal backends remain statically registered. Other platforms retain
the upstream build behavior.

macOS was added on 2026-11-13. Without it, the Mac build linked some GGML functions from
each copy: `ggml_flash_attn_ext` from Whisper's, which numbers `GGML_OP_FLASH_ATTN_EXT` 73,
and the fork-only `ggml_flash_attn_ext_set_n_kv_max` from llama.cpp's, which expects 74. Every
Gemma turn then aborted on `GGML_ASSERT(a->op == GGML_OP_FLASH_ATTN_EXT)`. Which copy won
followed link order, so a rebuild could break a build that had worked.

Changes from the published crate: this note, `namespace.rs`, build-script wiring,
a standalone Cargo workspace declaration, the upstream Unlicense text
(restored from the whisper-rs repository because the published crate omitted it),
and six native source changes, below.
When updating Whisper, regenerate and inspect the linked symbol inventory and run
both GPU transcription and inference in the same production process before shipping.

## Native source change: graph buffer reservation failure

`whisper.cpp/ggml/src/ggml-backend.cpp`, in `ggml_backend_sched_alloc_splits`, is
upstream llama.cpp commit `911f6cdc8a` ("ggml : handle graph buffer reservation
failure", #26070, 2026-09-18), applied unchanged. Drop it when the vendored GGML
already contains that commit.

Without it, a compute buffer that cannot be allocated crashes the process instead of
returning an error. `ggml_gallocr_reserve_n` records the new graph layout, fails to
allocate the buffer and leaves it `NULL`; the scheduler ignored that result, and
`ggml_gallocr_alloc_graph` then found a matching layout, skipped reallocation and
dereferenced the `NULL` buffer. On a Jetson this is ordinary: the LLM and Whisper share
the GPU, and on 2026-10-04 a 90 MiB encoder buffer failed and the Pond took `SIGSEGV`.
The last log line before such a crash is `ggml_gallocr_reserve_n_impl: failed to
allocate CUDA0 buffer`; with the change it is followed by `failed to reserve graph
buffers` and Whisper's own `failed to init ... allocator`, and the transcription
returns an error.

## Native source change: forget a reservation that could not allocate

`whisper.cpp/ggml/src/ggml-alloc.c`, in `ggml_gallocr_reserve_n_impl`, resets the recorded
node and leaf counts when a compute buffer cannot be allocated. It is the same change, line
for line, as the third native change in jarida-io/llama-cpp-rs-giap (`2dc017bc`, its
`llama-cpp-sys-2/POND-PATCH.md`). Upstream llama.cpp did not have it as of 2026-10-05; drop it
when the vendored GGML resets a failed layout itself.

The reservation fix above makes the first failure an error. The next allocation of the same
graph shape still crashed: the failed reservation had recorded a layout that places tensors in
the buffer it could not allocate, so the allocator found a matching layout, skipped
reallocation and wrote through NULL. While every transcription made its own state, a failed
state was dropped and the next call started with a fresh allocator, so this was unreachable.
Since states are kept and reused (`Engine` in `crates/pond-adapters-whisper/src/in_process.rs`),
the next transcription after an out-of-memory one reuses that allocator.

There is no Whisper-side test. The bindings here expose `ggml.h` only, not the allocator and
scheduler APIs, and widening them changes the symbol inventory this crate renames on Linux. The
fork's `tests/graph_reservation_failure.rs` exercises the identical code on the CPU backend: it
died with `SIGSEGV` on the second attempt without this change, and refuses all three with it.

## Native source change: decoder KV cache that cannot grow

`whisper.cpp/src/whisper.cpp`, in `whisper_full_with_state`, no longer frees the state
when the self-attention cache cannot be recreated for more decoders, and in
`whisper_kv_cache_init` a failed buffer allocation frees the ggml context it made.
This is Pond's own change, not an upstream one. Drop it when upstream whisper.cpp
stops calling `whisper_free_state` before `return -7`.

A state is created with a self-attention cache for one decoder. Beam search grows it
on the first decode to `beams + 2` decoders' worth, 42 MiB for `base` with five beams.
When that allocation failed, whisper.cpp freed the state and returned `-7`. But the
state belongs to the caller, and whisper-rs's `Drop` frees it again: a double free. On
2026-10-05 the Pond took `SIGBUS` on the Jetson one second after
`whisper_kv_cache_init() failed for self-attention cache`, while a build held most of
the memory. The `ggml_backend_sched_alloc_splits` change above does not reach this
path: it happens after the state, with its compute buffers, already exists. Now the
state is left freeable and reusable. The cache is marked absent, with no buffer and a
decoder count of zero, so another `whisper_full` on the state recreates it.
`whisper_decode_with_state` recreates a one-decoder cache when it finds none, because
language auto-detect decodes through it before `whisper_full` reaches its own check, and
asserted on the missing buffer.
`running_out_of_gpu_memory_while_decoding_is_an_error_not_a_crash` in
`crates/pond-adapters-whisper` reproduces the failure on a CUDA build. Its doc comment
says how to read a run that aborts in GGML's CUDA scratch pool instead
(`cuMemAddressReserve` in `ggml-cuda.cu`). That is a separate failure this change does
not touch: GGML aborts when a state's first decode cannot reserve its pool.

`build.rs` declares the native sources with `rerun-if-changed` and refreshes the copy
in `OUT_DIR` file by file. Before, the copy was made once per `OUT_DIR` and only
`wrapper.h` and `namespace.rs` were watched, so an edit to a native source compiled
nothing and the earlier library shipped with no error. `namespace.rs` leaves the
forced-include header alone when its content is unchanged: every native file includes
it, so rewriting it recompiled all of them (44 minutes on the Jetson) for a one-file edit.

## Native source change: a state's batch starts empty

`whisper.cpp/src/whisper.cpp` gives `whisper_state::batch` an initializer, so a new
state's batch is all null. This is Pond's own change; upstream has the same bug. Drop
it when upstream initialises the member.

`whisper_init_state` creates the state with `new whisper_state`, which leaves `batch`
indeterminate until the batch is allocated near the end. Every earlier failure, a
backend, `kv_self`, `kv_cross` or `kv_pad` that cannot be allocated, goes through
`whisper_free_state`, which hands that batch to `whisper_batch_free`. So the
out-of-memory path the graph-reservation change above turns into an error freed
whatever the pointers happened to hold, and when the allocator returned the block a
previous, already freed state had used, that was a double free. `whisper_batch_free`
skips null pointers, so an empty batch makes the early free do nothing.

## Native source change: a failed allocation resets the scheduler

`whisper.cpp/src/whisper.cpp` calls `ggml_backend_sched_reset` before returning `false` when
`ggml_backend_sched_alloc_graph` fails while encoding (conv, encoder, cross) or decoding. This is
Pond's own change. `ggml_graph_compute_helper` already resets after every compute, failed or not;
these four returns did not, so the scheduler kept the failed attempt's split assignments and tensor
copies. With states now kept between transcriptions, the next allocation on that scheduler could
reuse them against a different graph: a copy-layout assert or misrouted inputs when the shape
differs (prompt versus beam step, or another wake-word clip length). Worst-case buffers are
reserved when a state is created, so this path is rare. Drop it when upstream resets there.

## Native source change: a prefix for the embedded Metal library's symbols

`whisper.cpp/ggml/src/ggml-metal/CMakeLists.txt` names the start and end of the embedded Metal
library `_${GGML_METAL_EMBED_SYMBOL_PREFIX}ggml_metallib_start` and `..._end`. This is Pond's own
change. Upstream writes them as fixed names into generated assembly, which the forced include
never reaches, so with the Objective-C renamed they would be undefined; and with llama.cpp's
Metal backend defining the same names, Whisper could load llama.cpp's shaders. `build.rs` sets
the prefix to `pond_whisper_` when it isolates GGML; unset, the names are upstream's.
