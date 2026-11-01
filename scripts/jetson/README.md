# Jetson (Orin Nano) scripts

`bash scripts/giap.sh` is the front door on every host, the Jetson included: it detects the board
and whether CUDA is usable, builds with the right features, stops the service before a release
link, and `giap.sh doctor` (exit 1 on any FAIL) catches the bad states. From the Mac,
`bash scripts/giap.sh deploy` checks that HEAD is on the deploy remotes and then runs
`scripts/jetson.sh deploy`. The day-to-day procedure, service layout and checks are in
[docs/developer/jetson-dev-routine.md](../../docs/developer/jetson-dev-routine.md).

This directory holds what `scripts/jetson.sh` dispatches to:

```bash
bash scripts/jetson.sh <command>
```

| Command | Runs on | What it does |
|---|---|---|
| `deploy [--branch BR] [--no-restart]` | the Mac | `deploy.sh`: copies the recorded LiteRT-LM library, hard-resets the board's checkout to `origin/<branch>` (default `main`) with its submodule, builds and rsyncs the web UI, runs the CUDA release build on the board detached (following it across dropped connections), restarts the user service and health-checks it. `JETSON_HOST` picks another ssh alias (default `nano`), `JETSON_DATA_DIR` another data directory |
| `build [--cuda]` | the board | `build-native.sh`: apt packages, the web UI with the `npm` on `PATH`, and `cargo build --release`, with `pond-adapters-local-inference/cuda,pond-adapters-whisper/cuda` under `--cuda`. `giap.sh build` does the same job and also provides Node, stops the service and writes the build stamp |
| `docker-build [--full]` | an arm64 host with Docker | `build-docker.sh`: a CPU-only aarch64 binary in a `linux/arm64` container, written to `target-jetson/release/pond-server`. `--full` adds in-process GGUF inference. No CUDA is possible here |
| `optimize` | the board | `llama-optimization/run-all.sh`: a standalone CUDA llama.cpp build and benchmark suite; see [its README](llama-optimization/README.md) |
| `probe-mlc [--keep]` | the board | `mlc-toolcall-probe.sh`: MLC-LLM serve plus a streaming tool-call quality probe (an experiment) |
| `bakeoff <prep\|capture\|c1\|c2\|c3\|c4\|report>` | the board | The inference-engine bake-off harness; see [bakeoff/README.md](bakeoff/README.md) |

Other files here:

| File | What it is |
|---|---|
| `embed-p0/` | The P0 EmbeddingGemma 2 harness: memory beside the serving pond, latency, throughput and recall against MiniLM; see [its README](embed-p0/README.md) |
| `kiosk.sh` | Puts the dashboard on an attached panel with one Weston compositor and no desktop (`probe`, `enable-drm`, `start`, ...). Needs `nvidia-drm` loaded first |
| `sudoers-giap` | A sudoers drop-in listing the commands a deploy needs without a password; its header has the install and removal commands |
| `ggml-toolchain.cmake` | Pins `GGML_NATIVE=OFF` for builds that are not on the board |

The board's LiteRT-LM library is built with `bash scripts/giap.sh litert build linux-arm64` (Docker
on the Mac, or natively on the board followed by `giap.sh litert import` on the Mac); `deploy`
ships whatever package the Mac records. See
[docs/developer/litert-lm.md](../../docs/developer/litert-lm.md).

## First-time device setup

```bash
ssh nano
git clone --recurse-submodules <the repository's origin URL> goose-in-a-pond
cd goose-in-a-pond
bash scripts/giap.sh build        # Node, web UI, CUDA release build, build stamp
bash scripts/giap.sh              # menu: "Install the service" writes the user unit on port 8080
loginctl enable-linger "$USER"    # start at boot, survive logout
```

`deploy` restarts a **user** unit. `giap.sh install` runs `scripts/install.sh`, which writes a
**system** unit of the same name when none exists; on a board meant for `deploy`, install the user
unit first so `install` skips its own.

## Gotchas

- **`-C target-cpu=native`** in `.cargo/config.toml` is right on the board and wrong anywhere
  else: a cross or container build must override it (`target-cpu=cortex-a78` for rustc) or the
  binary SIGILLs. C and C++ need an ISA string (`-march=armv8.2-a+fp16+dotprod`), not a CPU name;
  `ggml-toolchain.cmake` covers ggml.
- **The board pulls `origin`.** Push the branch there (and to `jarida-io`) before deploying, or the
  deploy builds stale code.
- **NvMap error 12** on a model load: page cache NvMap cannot reclaim. Stop the pond,
  `sync; echo 3 > /proc/sys/vm/drop_caches`, start it.
- **Link OOM**: no chat or model load while a release build links. `deploy` stops the pond below
  3,000 MB free; `giap.sh build` stops it always.
- **The registry's tuning block is code.** `apply_jetson_settings`
  (`crates/pond-adapters-local-inference/src/lib.rs`) re-stamps every row of the chat model at each
  provider build, so hand edits to goose's registry do not survive.
