# Jetson dev routine

How GIAP is built, deployed, run and checked on the Jetson Orin Nano 8 GB. Tests and measurements
run on the board first, and on a Mac afterwards only when the maintainer asks. Numbers that are
true only on the hardware (KV cost, free memory, prefill and decode rates, NvMap behaviour) come
from here in any case. The script reference is
[scripts/jetson/README.md](../../scripts/jetson/README.md); recovery after a power cut is
[jetson-power-loss-recovery.md](jetson-power-loss-recovery.md); OS-level tuning is recorded in
[jetson-device-tuning.md](jetson-device-tuning.md).

## Three traps

1. **Never `cargo run` on the board, and never build without `--release` and CUDA.** A plain
   `cargo build -p pond-server` has no `cuda` feature: llama.cpp runs on the six CPU cores, many
   times slower, and nothing errors. Build with `bash scripts/giap.sh build` or deploy from the Mac.
2. **One server at a time.** The service holds 8080. A second `pond-server serve` started by hand
   without `--port` binds 4000 (falling back through 4009) and loads a second copy of the model
   into the same 8 GB; the board swap-thrashes. The port a server actually bound is in
   `<data_dir>/.runtime_api_port`.
3. **Never load a model while a release build links.** The linker is OOM-killed. The deploy and
   `giap.sh build` both stop the pond first when memory is short.

## Access

```bash
ssh nano     # alias in ~/.ssh/config: HostName nano.local, key ~/.ssh/nano_jetson
```

`scripts/lib/jetson-remote.sh` makes the deploy survive mDNS dropping out: it asks the board for
its own IPv4 address while the name resolves, records it in `~/.giap/jetson/<alias>.address`, and
falls back to that address later, always checking the name's host key. `JETSON_HOST=<alias>`
deploys to another alias.

Sudo on the board asks for a password unless `scripts/jetson/sudoers-giap` is installed (its header
says how); it lists the commands a deploy needs rather than granting `ALL`.

## The service

| | |
|---|---|
| Unit | `goose-in-a-pond.service`. The `giap.sh` menu's "Install the service" writes a **user** unit, `~/.config/systemd/user/goose-in-a-pond.service`, running the installed `~/.local/lib/goose-in-a-pond/bin/pond-server serve --port 8080`; `loginctl enable-linger <user>` makes it start at boot. `scripts/install.sh` writes a **system** unit of the same name (`/etc/systemd/system/`) instead. `giap.sh status` shows which scope a board has, and the menu refuses to add a second unit |
| Binary | Built at `~/goose-in-a-pond/target/release/pond-server` (with `.giap-build-stamp` naming its features and commits), then installed with `pondnet` to `~/.local/lib/goose-in-a-pond/bin/` by `giap.sh build` and the deploy. The service runs the installed copy, so `cargo clean` or a deleted `target/` cannot stop the pond from starting; the copy it replaced is kept as `pond-server.previous`. A unit that still runs from `target/` is repointed once by `bash scripts/giap.sh install-binaries`, and `giap.sh doctor` warns until it is |
| HTTP | `127.0.0.1:8080`, loopback only. Tunnel to it: `ssh -N -L 8080:127.0.0.1:8080 nano`, then `http://localhost:8080`. Keep the same port both ends or OAuth redirects miss |
| HTTPS | all interfaces, normally 4443 (`<data_dir>/.runtime_https_port`), with the pinned certificate the phone app trusts; see [remote-access.md](../remote-access.md) |
| Data | `~/.local/share/goose-in-a-pond/` |
| Logs | daily files `<data_dir>/logs/pond.log.YYYY-MM-DD`, in UTC. journald holds only what reached stdout and stderr, so it is nearly empty for a healthy pond and useful for a crash before logging started |

```bash
ssh nano systemctl --user status goose-in-a-pond
ssh nano systemctl --user restart goose-in-a-pond
ssh nano 'cd goose-in-a-pond && bash scripts/giap.sh doctor'   # first move when something is wrong
ssh nano 'cd goose-in-a-pond && bash scripts/giap.sh logs'     # tail, errors, turn telemetry
```

For a system unit drop `--user` and add `sudo`.

## Deploying

```bash
git push origin <branch> && git push jarida-io <branch>   # the board pulls `origin`
bash scripts/giap.sh deploy                               # checks, then runs scripts/jetson.sh deploy
bash scripts/jetson.sh deploy [--branch BR] [--no-restart]
```

`giap.sh deploy` refuses to run on the board and asks before deploying a HEAD that is missing from
`origin` or `jarida-io`, because the board resets to `origin/<branch>` and would otherwise build
stale code and report success. `scripts/jetson/deploy.sh` then:

1. Copies the LiteRT-LM library recorded for `linux-arm64` (`giap.sh litert build linux-arm64`, or
   `giap.sh litert import` of a build made on the board) to `<data_dir>/lib/litert-lm/<capi>-<commit8>/`,
   verifying it on both ends. With none recorded it warns and continues; `.litertlm` models will
   not load on the board. See [litert-lm.md](litert-lm.md).
2. Hard-resets the board's checkout to `origin/<branch>` (default `main`) and syncs the goose
   submodule to its pin. A parent commit that moves the pin does not move the submodule on its own.
3. Builds the web UI on the Mac and rsyncs `pond-desktop/dist`, which pond-server embeds at compile
   time.
4. Runs the release build on the board, detached, with
   `--features pond-adapters-local-inference/cuda,pond-adapters-whisper/cuda` and
   `CMAKE_CUDA_ARCHITECTURES=87` (a real sm_87 cubin rather than PTX compiled at first load).
   Under 3,000 MB free (`JETSON_BUILD_MIN_MB`) the pond is stopped for the build. A lost connection
   does not stop it: run the deploy again and it follows the build still running. The build writes
   the build stamp.
5. Restarts the **user** unit and polls `GET /api/v1/health` for up to ten minutes; startup applies
   migrations, sizes the context, loads the embedder and warms the prompt prefix before it binds.
   `--no-restart` leaves the service as it was.

A cold CUDA build takes 35 minutes or more; an incremental one after a submodule bump about ten.

### Building on the board

`bash scripts/giap.sh build` builds the UI and a release CUDA server on the board: it finds or
fetches a Node inside the supported range (`giap.sh node`), stops the pond while it builds, builds
the remote-access helper when Go is installed, and writes the build stamp. The older
`bash scripts/jetson.sh build --cuda` uses whatever `npm` is on `PATH` and stops nothing.

To ship a UI built elsewhere: `rsync -az --delete pond-desktop/dist/ nano:goose-in-a-pond/pond-desktop/dist/`,
then `find pond-desktop/dist -exec touch {} +` on the board so cargo re-embeds it.

## Checking it is on the GPU

```bash
grep -E "Applied Jetson Orin Nano CUDA|CPU-ONLY" ~/.local/share/goose-in-a-pond/logs/pond.log.$(date -u +%F) | tail -1
```

`Applied Jetson Orin Nano CUDA settings` is a CUDA build. `CPU-ONLY (built without the cuda
feature)` means rebuild. The line before it, `Jetson context sized to fit this model's KV cache in
the LLM budget`, gives the window the model got; see
[inference_optimization.md](inference_optimization.md#the-context-window) for how it is derived.
With a Gemma 4 E2B or E4B chat model, `MTP drafter loaded` confirms speculative decoding.

The vision and face models need their own GPU runtime: `bash scripts/giap.sh onnxruntime build` on
the board builds ONNX Runtime with TensorRT in the background (hours; `giap.sh onnxruntime status`
follows it) and their engines. Then:

```bash
grep -E "onnx: .* (moved to|stays on|loaded on)" ~/.local/share/goose-in-a-pond/logs/pond.log.$(date -u +%F) | tail
```

`onnx: face_detector moved to tensorrt` is on the GPU; a model that only says `loaded on the cpu`
is not, and `GET /api/v1/vision/status` reports the same per model.

Per turn, with the KV prompt cache working: the first turn after a model load plans
`CreateContext`, a new conversation's first turn `FullPrefillInPlace` (with `giap::kv` logging the
preamble snapshot it restored, if one matched), later turns `ReusePrefix`, side calls
`SacrificialContext`. `FullPrefillInPlace` on a later turn means something rewrote the start of the
prompt.

```bash
grep -E "prompt prefill plan|turn_end" ~/.local/share/goose-in-a-pond/logs/pond.log.$(date -u +%F) | tail
```

## Emulating the board on the Mac

`scripts/jetson-emu.sh` runs the board's decisions on a Mac by setting `POND_DEVICE_PROFILE`
(`crates/pond-core/src/models/domain/device_profile.rs`); unset, nothing is emulated.

```bash
bash scripts/jetson-emu.sh doctor                      # what each tier can and cannot emulate
bash scripts/jetson-emu.sh run [--profile P] [-- CMD]  # tier 1: pond-server on the Mac, in a scratch pond
bash scripts/jetson-emu.sh test [--profile P] [FILTER] # tier 1: cargo test with the device branches live
bash scripts/jetson-emu.sh box [--profile P] [-- CMD]  # tier 2: linux/arm64 container, memory ceiling enforced
bash scripts/jetson-emu.sh profiles                    # orin-nano-8gb (default), orin-nano-8gb-cpu, orin-nx-16gb
```

Under a profile the process is a budgeted device: the Jetson settings stamp, context sizing, the
memory status and the picture-support rules all take the board's path. Tier 1 cannot enforce the
7,620 MB ceiling; neither tier has CUDA; **no tier is a source of a throughput, TTFT, KV-cost or
free-memory number**.

## Running notes

- **Power mode**: 25 W (`sudo nvpmodel -m 1`) rather than MAXN_SUPER (`-m 2`). Both clock memory at
  3,199 MHz and decode is memory-bandwidth-bound; one measured turn under MAXN_SUPER drew 1,176
  over-current events against 44 at 25 W for 5% more decode. `giap.sh` (Jetson power) shows the mode
  and over-current counters. Mode ids on this SKU: 0 = 15 W, 1 = 25 W, 2 = MAXN_SUPER, 3 = 7 W.
- **Headless**: gdm3 stays off; GNOME held 2.6 GB of NvMap. `scripts/jetson/kiosk.sh` puts the
  dashboard on an attached panel with one compositor process instead of a desktop.
- **NvMap error 12** on a model load while `free` shows gigabytes: page cache NvMap cannot reclaim.
  Stop the pond, `sync; echo 3 > /proc/sys/vm/drop_caches`, start it.
- **Leave `context_window_override` at 0.** The derived window is what the engine can hold.
- **Decode rule of thumb**: `tok/s = 102 / model_GB` with the model fully on the GPU, before
  speculative decoding.
- **Whisper** keeps its decode states for the life of the process (about 208 MB plus a CUDA scratch
  pool with `base`, measured 2026-10-05; see [voice-pipeline-efficiency.md](../voice-pipeline-efficiency.md)),
  and voice mode adds the wake-word state on first use. Count both next to the model; they are not
  freed between utterances.
- **llamafile** defaults to port 8080, which the pond holds on the board: set `GIAP_LLAMAFILE_PORT`
  if you run one there.
- **`pkill -f pond-server` over ssh kills your own shell**: the remote command line contains the
  pattern. Use the PID or `pgrep -f '[p]ond-server'`.
- **Disk**: `target/debug` on the board is waste; `rm -rf target/debug` reclaims about 10 GB.
- **`.cargo/config.toml` sets `-C target-cpu=native`**, correct for a build on the board and wrong
  for any cross or container build (the binary SIGILLs); those override it.
