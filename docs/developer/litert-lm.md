# The LiteRT-LM C API library

goose's `litert` backend runs `.litertlm` models through Google's
[LiteRT-LM](https://github.com/google-ai-edge/LiteRT-LM) C API, which it opens at runtime by
absolute path (`RTLD_LOCAL`). GIAP builds that library itself, per platform, and ships it with the
desktop app and to the Jetson. Nothing is downloaded at runtime.

Everything here is behind `scripts/giap.sh`; the logic is `scripts/lib/litert-setup.sh` and its
tests are `scripts/lib/litert-setup.test.sh`.

## Running a LiteRT-LM model

A `.litertlm` file is one of the in-process engines under `chat_provider = "local"`
([inference_optimization.md](inference_optimization.md#choosing-the-chat-provider-and-engine)).
Choosing one as the chat model is all it takes: `litert_model::register`
(`crates/pond-adapters-goose/src/litert_model.rs`) gives the file a goose registry row with
`backend_id = "litert"`, and the provider build runs it on the `litert` backend. Files live in
`<data_dir>/models/litertlm/`; the curated picks are the Gemma 4 E2B and E4B builds from
`litert-community` ([models.md](models.md#the-curated-list)).

The engine options come from `pond_core::models::domain::litert::engine_options` and are stamped
by `apply_litert_settings` (log line `Applied LiteRT-LM settings`):

| Option | Budgeted device (the Orin) | Elsewhere | Override |
|---|---|---|---|
| Context (`max_num_tokens`) | 8,192 | 16,384 | none; fixed for the engine's life |
| Execution | GPU | GPU | `GIAP_LITERT_BACKEND=gpu\|cpu` |
| Multi-token prediction | off | on, except E2B on the CPU | `GIAP_LITERT_SPECULATIVE=on\|off` |
| Prefill chunk sizes compiled | 128 only | every size the model ships | `LITERT_PREFILL_SIGNATURES` |

The Orin's 8,192 is measured: 4,096 cannot hold the pond's first prompt, and 16,384 ran out of
memory with E4B on the GPU. Compiling only the 128-token prefill chunk there kept the GPU's share
of RAM from growing across a compaction. A LiteRT-LM model reads no pictures in the pond: the
backend sends text only, so the library's image fixes below are not exercised by GIAP.

## What is shipped

| File | What it is |
|---|---|
| `liblitert-lm.{dylib,so}` | The C API (`//c:litert-lm`), built from the pinned commit |
| `libLiteRt`, `libGemmaModelConstraintProvider` | Linked by the C API; the constraint provider is needed even on CPU |
| `libLiteRtWebGpuAccelerator`, `libwebgpu_dawn`, `libLiteRtTopKWebGpuSampler` | The GPU path: WebGPU/Dawn over Vulkan on the Orin, over Metal on a Mac. Not in the desktop app |
| `libLiteRtMetalAccelerator`, `libLiteRtTopKMetalSampler` | macOS only: the desktop app's GPU path |
| `include/*.h` | The public C headers (not shipped in the app) |
| `LICENSE` | LiteRT-LM's Apache-2.0 licence |
| `MANIFEST.sha256` | Line 1 describes the build; then `sha256  ./path` for every other file |

Everything except `liblitert-lm` is LiteRT's own prebuilt binary from `prebuilt/<platform>/` at the
same commit.

### Metal or WebGPU on a Mac

LiteRT opens its GPU accelerator by file name from the directory `libLiteRt` is in, and on macOS it
tries WebGPU before Metal and keeps the first it finds. A macOS package carries both, so it runs on
WebGPU. On an M4 with E2B, Metal measured warm turns at 129-142 ms to the first token against
155-192 and decode about 13% faster; the live tool-call test passes on both.

| Runs | Accelerator | Why |
|---|---|---|
| The desktop app | Metal | Staging leaves out the WebGPU accelerator, its sampler and Dawn (`LITERT_DESKTOP_LEAVES_OUT`); the app is 41 MB smaller for it |
| A pond run from the source checkout | WebGPU | The Orin's accelerator, so tests on a Mac reflect the Orin |
| The same, with `GIAP_LITERT_ACCELERATOR=metal` | Metal | pond-server points the backend at `<package>.metal/`: hard links to the package's files except WebGPU's, as the app carries them |

The library statically contains its own Rust std, allocator, abseil and protobuf, which is why it
stays behind `RTLD_LOCAL`. Their third-party notices are not collected into the package yet.

## The pin

`LITERT_COMMIT` and `LITERT_CAPI_VERSION` in `scripts/lib/litert-setup.sh`:
`a5dd27b5bd363be38feb62af637facbdb043b11c` on `giap-main` in
[jarida-io/LiteRT-LM](https://github.com/jarida-io/LiteRT-LM/tree/giap-main), GIAP's public fork of
LiteRT-LM. That is
LiteRT-LM `main` at `b320801b` (`version.bzl`: `VERSION = "0.18.0"`, `C_API_VERSION = "1.0.0"`; C
API 1.0.0 is on `main`, in no release yet) plus GIAP's fixes, among them:

- **fp32 activations on the GPU.** With fp16, a conversation carrying tools decoded corrupt text on
  the Mac's GPU.
- **KV snapshots and whole-prompt prefill.** `litert_lm_conversation_save_kv_snapshot` and
  `litert_lm_conversation_load_kv_snapshot` write a conversation's KV cache to a file and read it
  back; `litert_lm_conversation_send_prefill_text_stream` rewinds the cache and prefills a whole
  prompt, skipping the leading tokens the cache already holds. The backend sets the chat aside
  through them before a side call runs and restores it afterwards (the goose patch ledger has the
  details).
- **KV reuse through an unchanged image**: a key per picture, prefill chunks aligned to images, and
  a cache of encoded images.

A build fetches the pin from the fork (`LITERT_REPO_URL`), so it needs nothing set. A managed
clone made from upstream before the fork existed fetches the pin from the same URL. The backend
still runs on upstream LiteRT-LM. Without the GIAP calls it never sets a conversation aside or
rematches one, and its GPU path keeps the fp16 fault.

To move it:

1. Rebase `giap-main` onto the upstream commit you want, run the fork's tests, push it to the fork,
   and read `version.bzl` at the result. A different `C_API_VERSION` means the goose binding has to
   be checked against `c/*.h` before anything else. A pin that is not pushed cannot be fetched.
2. Set `LITERT_COMMIT` (and `LITERT_CAPI_VERSION`) to the rebased head, then
   `bash scripts/lib/litert-setup.test.sh`.
3. Build both platforms. Packages live under `<capi>-<commit8>/`, so the old one stays until you
   delete it.
4. If the prebuilt set changed, `LITERT_REQUIRED_LIBS`, `LITERT_DESKTOP_REQUIRED_LIBS`,
   `LITERT_DESKTOP_LEAVES_OUT` and `mac.binaries` in `pond-desktop/electron-builder.yml` follow it;
   `scripts/verify-sidecar.sh` names a dylib that is staged but not listed.

## Building

```bash
bash scripts/giap.sh litert build macos-arm64      # on an Apple Silicon Mac
bash scripts/giap.sh litert build linux-arm64      # in Docker anywhere it runs linux/arm64; natively on the Jetson
bash scripts/giap.sh litert import DIR             # take in a package built on the Jetson, for deploy
bash scripts/giap.sh litert status                 # what is recorded, each verified; exit 1 on a failure
bash scripts/giap.sh --dry-run litert build linux-arm64   # every command, nothing run
```

The source is a clone of the fork this tool keeps at `~/.giap/litert-lm/src`, moved to the pin and
given the Git LFS objects of that one platform. `LITERT_LM_SRC=<dir>` uses a clone of your own
instead, which must already be at the pin; it is never moved.

- **macos-arm64** needs bazelisk, git-lfs and the Xcode command line tools, and runs
  `bazelisk build -c opt --config=macos_arm64 --define=litert_runtime_link_mode=dynamic //c:litert-lm`
  natively: about 22 minutes cold on an M4.
- **linux-arm64** runs the same target with `--config=linux_arm64` in `ubuntu:22.04` (glibc 2.35,
  the same as JetPack 6; not `rust:bookworm`, whose glibc is newer than the device's), with clang 15
  and a bazelisk checked against its published SHA-256. The source is mounted read-only, and Bazel's
  output root persists in the Docker volume `giap-litert-lm-linux-arm64`
  (`docker volume rm giap-litert-lm-linux-arm64` reclaims it). The container packages and verifies
  the result with the same script, including a `dlopen` of the library on that glibc.
- **linux-arm64 on the Jetson itself** builds natively, with the board's own Bazel (bazelisk or
  bazel), clang and patchelf, and the flags its own builds have always used: `-c opt
  --define=litert_runtime_link_mode=dynamic --features=-parse_headers`, without
  `--config=linux_arm64` (`-march=armv8-a -O3`, YNNPACK). Every Orin measurement was made with that
  build, and switching flags would recompile every target, hours on the board. The manifest records
  the flags either way. Bazel folds `PATH` into its C++ actions, so run it with the `PATH` the
  board's cache was built under or it recompiles everything. Stop the pond service first. Then copy
  the package to the dev machine and `giap.sh litert import` it there, because `scripts/jetson.sh
  deploy` ships the package the dev machine records.
- `LITERT_BAZEL_JOBS=N` caps Bazel's jobs, for a Docker VM that runs out of memory.

A build is staged beside its destination, verified, and only then moved into place and recorded, so
a failed build leaves the previous package as it was.

## Package layout and verification

```
~/.giap/litert-lm/
  .path-macos-arm64    .path-linux-arm64      the package each platform last built and verified
  1.0.0-a5dd27b5/macos-arm64/                 liblitert-lm.dylib, lib*.dylib, include/, LICENSE, MANIFEST.sha256
  1.0.0-a5dd27b5/linux-arm64/                 liblitert-lm.so, lib*.so, include/, LICENSE, MANIFEST.sha256
```

Packaging rewrites how the libraries find each other. On macOS: `liblitert-lm.dylib` is renamed
`@rpath/liblitert-lm.dylib`, Bazel's sandbox rpaths are removed, every dylib searches
`@loader_path`, and every dylib is signed again ad hoc, because Apple Silicon refuses a modified,
unsigned Mach-O. On Linux: every `.so` gets a RUNPATH of exactly `$ORIGIN`, replacing the
Google-internal paths the prebuilts carry. Neither needs `DYLD_LIBRARY_PATH` or `LD_LIBRARY_PATH`.

Verification, which `litert status`, `giap.sh doctor` and the stage, verify and deploy scripts all
run, checks every file against the manifest and fails on any file the manifest does not list or any
symlink. Only then does it inspect the libraries (Mach-O: arm64, dependencies inside the directory or
part of macOS, rpaths, signature; ELF: aarch64, RUNPATH, dependencies) and load one where the host
can. A package that fails its hashes is never loaded.

## Where it goes

| Target | Location | How |
|---|---|---|
| Desktop app | `Contents/Resources/litert-lm/`, beside `pond-server` | `npm run stage:server` copies the recorded macos-arm64 dylibs, except WebGPU's, and LICENSE to `pond-desktop/resources/litert-lm/`; `scripts/verify-sidecar.sh` checks and loads them and refuses a WebGPU accelerator; electron-builder ships them |
| Jetson | `~/.local/share/goose-in-a-pond/lib/litert-lm/1.0.0-a5dd27b5/` | `bash scripts/jetson.sh deploy` copies the recorded linux-arm64 package and runs `sha256sum -c` and `ldd` there; `JETSON_DATA_DIR` if the pond's data dir is elsewhere |
| A pond run from `target/` | the recorded package directory | found through `~/.giap/litert-lm/.path-<platform>` |

At startup pond-server decides which library the backend loads and logs the decision
(`crates/pond-server/src/litert_runtime.rs`). A `GOOSE_LITERT_LIB_DIR` that is already set wins. A
library in `litert-lm/` beside the executable, the app bundle's, is left for the backend to find
there, so an app always loads the copy it was built and verified with. Otherwise pond-server sets
`GOOSE_LITERT_LIB_DIR` to the newest package under `<data dir>/lib/litert-lm/` (the Jetson), then to
the recorded one (`~/.giap/litert-lm/.path-<platform>`, under `GIAP_LITERT_HOME` when that is set),
or to that package's Metal view when `GIAP_LITERT_ACCELERATOR=metal` on a Mac. With none of them the
backend falls back to its own search.

With nothing recorded, staging and deploy warn and carry on: the app or the device then runs without
the litert backend. A recorded package that fails verification stops them.

### With no library at all

After deciding, pond-server checks the places goose's backend searches (`GOOSE_LITERT_LIB_DIR`,
`litert-lm/` beside the executable, the executable's directory, `<data dir>/engine/data/litert-lm/lib`)
and, when none holds the library, logs which it searched (a `WARN` when LiteRT-LM models are on
disk). The catalogue then lists every LiteRT-LM model as unavailable (`litert_library_missing`), the
dashboard disables it with "Needs the LiteRT-LM library: run giap.sh litert build", and the pond
refuses to download or switch to one with a 409
([models.md](models.md#an-engine-this-pond-cannot-run)). Install the package, then restart the
pond: the check runs once, at startup.

## When Bazel stalls on downloads

From some networks `storage.googleapis.com` resolves to front ends that serve Bazel at 5-30 KB/s and
never time out, and the TensorFlow-family dependencies list a Google Storage mirror first, so a build
can sit for hours on one archive. Fetch the archive yourself from a front end that is fast from where
you are (`curl -w '%{speed_download}'` to compare; `--resolve storage.googleapis.com:443:<ip>` to
pick one), check its SHA-256 against the `.bzl` that names it, put it in a directory, and pass that:

```bash
bash scripts/giap.sh litert build linux-arm64 --distdir ~/litert-distdir    # or LITERT_DISTDIR=...
```

Bazel takes an archive from there when its file name and checksum match. In Docker the directory is
mounted read-only at `/distdir`.

## Google's own prebuilts

LiteRT-LM's Python wheel already packages `prebuilt/<platform>/liblitert-lm.*` when the repository
carries it. Once a release ships C API 1.0.0 that way, the Bazel step can become a copy of that file:
the backend binds to the C API, not to this build. The packaging, manifest and verification stay as
they are, and Google's library needs the same rpath fixes as the other prebuilts.
