# Installation Guide

## Quick start

```bash
git clone --recursive https://github.com/jarida-io/goose-in-a-pond.git
cd goose-in-a-pond
bash scripts/giap.sh          # menu: diagnose, install and build, service, desktop, logs
```

`scripts/giap.sh` is the front door. It detects the host (Jetson, other Linux, macOS), whether
CUDA is usable, the service unit's scope and the known bad states, then offers only the actions
that apply. Every verb also runs without the menu:

| Command | Does |
|---|---|
| `giap.sh doctor` | health report; exits 1 on any FAIL. Run it after every install |
| `giap.sh console [HOST]` | live stats, charts and logs: this machine, the Jetson the deploy scripts use (`nano`), and HOST over ssh. Keys: `tab` host, `l` logs, `d` doctor, `r` restart, `?` all |
| `giap.sh status` | the detection banner only |
| `giap.sh install` | first-time install through `scripts/install.sh`, with guardrails (below) |
| `giap.sh build` | web UI, then a release `pond-server` with this host's features, then `pondnet` if Go is present |
| `giap.sh build-ui` | `npm ci` when the lock is newer, `npm run build`, then touch `dist` so cargo re-embeds it |
| `giap.sh update` | pull, Node, build, restart, Wi-Fi grant, doctor |
| `giap.sh build-desktop` | macOS: `npm ci && npm run bundle:app` (stages a release sidecar first) |
| `giap.sh gui` | macOS: launch the built app against the running server (`GIAP_SERVER_PORT`) |
| `giap.sh deploy` | from a dev machine: `scripts/jetson.sh deploy` |
| `giap.sh logs` | today's `<data_dir>/logs/pond.log.YYYY-MM-DD` (UTC; not journald) |
| `giap.sh node [--check] [--method M] [--major N] [--install-deps]` | find or download a Node in range |
| `giap.sh litert status\|build\|import` | the LiteRT-LM library that runs `.litertlm` models |
| `giap.sh models pairings [--check]` | regenerate the vision pairing table |
| `giap.sh wifi-grant [remove\|status]` | Linux: the polkit grant for the BLE setup channel |
| `giap.sh service-account [undo\|status]` | Linux: run the pond as its own locked account, `agamemnon`, or move it back |
| `giap.sh provision --host H --key F` | operator: fetch a pond's setup card, give it a device certificate |

Global flags: `--dry-run` prints each command instead of running it; `-y` answers yes to
confirmations. Without a terminal and without `-y`, an action that needs confirmation refuses.
The `Makefile` targets forward to these commands.

## Prerequisites

| Requirement | macOS | Linux |
|---|---|---|
| Git | Xcode command line tools | `apt install git` |
| Rust (`rust-version` 1.91.1) | [rustup.rs](https://rustup.rs) | [rustup.rs](https://rustup.rs) |
| C compiler | `xcode-select --install` | `apt install build-essential` |
| cmake, pkg-config | `brew install cmake pkg-config` | `apt install cmake pkg-config` |
| ALSA, D-Bus, OpenSSL headers | not needed | `apt install libasound2-dev libdbus-1-dev libssl-dev` |
| Go (`native/pondnet/go.mod`: 1.27.3) | [go.dev](https://go.dev/dl/) | [go.dev](https://go.dev/dl/) |
| Node | `bash scripts/giap.sh node` | `bash scripts/giap.sh node` |
| Read access to `jarida-io/llama-cpp-rs-giap` | see [CONTRIBUTING](../CONTRIBUTING.md#prerequisites) | same |

`install.sh` installs cmake and pkg-config through Homebrew, and the Linux packages through apt,
dnf or pacman. On a production or Jetson install it also checks for the LLVM tools (`clang`,
`lld`, `llvm-ar` and friends).

Go builds `pondnet`, the remote-access helper, beside `pond-server`. `giap.sh build` skips it
with a warning when Go is missing (remote access then stays off); `install.sh` counts a failed
helper build as a failed build.

### Node

Three floors, each read off the packages that set it:

| For | Needs | Because |
|-----|-------|---------|
| The desktop app and the tests (the repo's range) | `^22.13.0 \|\| ^24.0.0 \|\| >=26.0.0` | ESLint 10 wants 22.13+ in the 22 line; vitest 5 rules out 23 and 25 |
| Building the web UI | `^20.19.0 \|\| >=22.12.0` | Vite 8. A server (the Jetson) only needs this |
| The Matter controller | `>=20.19 <22.0 \|\| >=22.13` | matter.js 0.17. Every Node in the repo's range qualifies |

`.nvmrc` says `22`, `pond-desktop/package.json` carries the range as `engines`, and CI's frontend
jobs run on 22. A Node outside the range is the usual reason `npm test` or `npm run dev:electron`
fails in a way that does not look like a Node problem.

```bash
bash scripts/giap.sh node                  # check; use one you have; else offer a download
bash scripts/giap.sh node --check          # report only (exit 1 if the shell's node is wrong)
bash scripts/giap.sh node -y               # no prompts
bash scripts/giap.sh node --method nvm     # install through nvm, fnm or volta; default: auto
bash scripts/giap.sh node --major 24       # which major to install when none is present
bash scripts/giap.sh node --install-deps   # then `npm ci` in pond-desktop and extensions/music
```

It stops at the first of these that works:

1. **The Node on your PATH**, if it is inside the range.
2. **One already installed** under nvm, fnm, volta, asdf or `~/.giap/node`. It asks each binary
   for its version and skips one that will not run. One in range is **recorded** in
   `~/.giap/node/.path`; nothing is downloaded.
3. **An install**, only after asking: through nvm, fnm or volta if present, else a download from
   `nodejs.org/dist` into `~/.giap/node`, checked against the release's `SHASUMS256.txt` before
   it is moved into place. https only (TLS 1.2+). macOS and Linux, arm64 and x64.

Nothing is installed globally or added to a shell profile. `GIAP_NODE_HOME` moves the download
folder; `GIAP_NODE_DIST_URL` points it at a mirror.

Every other `giap.sh` command puts the recorded Node first on PATH for its own run when the
shell's is outside the range, and says so. **The pond reads the record too**
(`crates/pond-server/src/node_path.rs`): at start, if the `node` on its PATH is outside the range
or missing, it puts the recorded one first, so the Matter controller and stdio extensions run on
it. That is the Jetson's case, where a systemd service's PATH finds apt's old Node first. The
record is read only at start, so restart the pond after `giap.sh node`; the log line is
`node_path_recorded`. Tests: `bash scripts/lib/node-setup.test.sh` (CI job `node-setup`).

## What `giap.sh install` does

It runs `scripts/install.sh` and adds three guardrails that depend on host state:

- **No second service unit.** `install.sh` writes a system unit; a Jetson set up by hand runs a
  user unit of the same name, and both means two servers each loading a model into one memory
  pool. `giap.sh` passes `--no-service` when a unit exists at either scope.
- **Node too old for the web UI.** It offers to fix Node first; otherwise the server embeds
  whatever is already in `pond-desktop/dist`.
- **Jetson with the service running.** It offers to stop the service for the build, because the
  release link is OOM-killed beside a resident model, and restarts it after.

It asks for Full (default) or Minimal, then confirms the exact `install.sh` command.

### `scripts/install.sh`

```
bash scripts/install.sh [OPTIONS]

Modes (auto-detected: Jetson -> jetson; Linux without a display -> production; else dev):
  --production      Linux: systemd service + mDNS
  --jetson          Jetson Orin Nano (CUDA features when CUDA is usable)
  --minimal         server + databases only, no model downloads

Build:
  --full            cargo build --workspace instead of the server crates only
  --desktop         also run `npm install` in pond-desktop
  --no-verify       skip the post-install health check

Models:
  (default)         offer GIAP's own conversation models, run in process; pick one or none
  --ollama          use Ollama instead: install it if needed and pull gemma4:e2b
  --llamafile       use a llamafile instead: gemma-2-2b-it into <data_dir>/bin
  --no-models       skip model downloads
  --whisper-model MODEL   tiny|base|small (default: base)

Production:
  --port PORT       override the port
  --dedicated       set the hostname to 'pond', serve on port 80
  --shared          keep the hostname, use a non-privileged port
  --no-service      skip the systemd unit
  --data-dir DIR    override the data directory
```

Its steps, in order:

1. Pre-flight: `git`, `cargo`, `cc`.
2. `git submodule update --init --recursive` if `goose/Cargo.toml` is missing.
3. System packages (above).
4. `cargo build -p pond-core -p pond-infra -p pond-api -p pond-server`: debug in dev mode,
   `--release` otherwise. A Jetson with CUDA adds
   `--features pond-adapters-local-inference/cuda,pond-adapters-whisper/cuda`. Then
   `scripts/build-network-helper.sh` builds `pondnet`.
5. `pond-server setup --model <size>` (next section).
6. The printed setup card (below).
7. Unless `--minimal`: the LLM step, and a Piper voice file under `models/tts/` that the Kokoro
   TTS engine does not use. The LLM step lists GIAP's picks (E4B 4.2 GB, E2B 2.6 GB) and downloads
   only the one chosen, with `pond-server models download` and `models activate`. Without a
   terminal it downloads nothing: the app's Models page shows the same picks and waits. On a
   Jetson whose L4T release caps large GPU allocations it says E4B will not load and defaults to
   none. `--ollama` and `--llamafile` replace the step.
8. Production and Jetson: the systemd unit and mDNS. A new pond is offered its own locked
   account, `agamemnon` (default yes); a pond that already has a unit keeps its account (below).
9. Linux: the Wi-Fi polkit grant on production and Jetson; a dev machine only prints the command.
10. `--desktop`: `npm install` in `pond-desktop`.
11. Unless `--no-verify` or systemd already runs it: start the server and check its health.

### `pond-server setup`

Safe to run again. It initialises `pond_system.db`, `pond_logs.db` and `pond_vectors.db` with all
migrations; moves flat model files into the `hf_cache` blob layout; fetches the model catalogue;
assigns Kokoro's default voice to the `tts` role if no voice is assigned; seeds the four
built-in prompt templates; downloads the Whisper GGML model (`tiny`, `base` or `small`); and
checks for the ONNX Runtime, `espeak-ng-data` and a microphone. Whisper runs in-process
(`whisper-rs`); there is no Whisper binary to download.

## Data directory

| Platform | Default |
|---|---|
| macOS | `~/Library/Application Support/goose-in-a-pond/` |
| Linux | `~/.local/share/goose-in-a-pond/` (`$XDG_DATA_HOME` honoured by the installer) |
| Any | `POND_DATA_DIR=/path`, or `install.sh --data-dir` |
| Linux, run as `agamemnon` | `/var/lib/goose-in-a-pond/` (the unit sets `POND_DATA_DIR`; a link stays at the old path) |

```
$DATA_DIR/
├── pond_system.db          authoritative: sessions, settings, memories, skills, devices, models
├── pond_logs.db            telemetry, event log, sensor readings
├── pond_vectors.db         the context vector index
├── schedules.json, schedule_runs.json
├── secrets.json            encrypted secret store (key under secrets/)
├── .runtime_api_port       the HTTP port the server bound (read this, never assume 4000)
├── .runtime_https_port     the HTTPS port it bound
├── models/
│   ├── ggml-<size>.en.bin  Whisper
│   ├── gguf/               GGUF chat models (llama.cpp)
│   ├── litertlm/           .litertlm models (LiteRT-LM)
│   ├── llm/                llamafiles
│   ├── kokoro/voices/      Kokoro voices (weights beside them in kokoro/)
│   ├── embedding/          embedding models
│   ├── silero/             VAD
│   └── face/               face detection and recognition ONNX models
├── hf_cache/               content-addressed Hugging Face downloads
├── engine/                 Goose's own state (GOOSE_PATH_ROOT)
├── lib/litert-lm/          recorded LiteRT-LM library packages
├── bin/                    downloaded binaries; espeak-ng-data link
├── logs/                   pond.log.YYYY-MM-DD (UTC)
├── tls/                    the HTTPS identity
├── matter-server/          the Matter controller and its storage
├── embedded-network/       remote-access (pondnet) state
├── prompts/                prompt template overrides
└── setup/                  0700: the printed setup card's code and claim
    ├── setup.json          0600; never regenerated, keep a backup
    └── card.svg            0600; the card install wrote, to print
```

### The setup card

Install makes the pond's setup code and writes a printable card to `$DATA_DIR/setup/card.svg`.
**Print it and keep it with the pond.** A pond with no screen is set up by scanning the card with
Goose On The Go, which claims the pond; the card is accepted only until the first device pairs.

```bash
pond-server setup-code                      # the code, the pond id, a terminal QR
pond-server setup-code --json               # {"code","display","id","claimed_at","url"}
pond-server setup-code --card card.svg      # the printable card again (`-` for stdout)
pond-server setup-code --status             # unclaimed, claimed or missing (the doctor uses this)
pond-server setup-code --rearm              # accept the same card again after a factory reset
```

For a pond imaged elsewhere, `bash scripts/giap.sh provision --host <pond> --key <file>` fetches
its card over SSH and saves it as `./pond-<id>-setup-card.svg` (`--card` to choose the path). A
pond that already had paired phones when it got its first setup code is created claimed, so
upgrading a pond in use opens nothing.

## Choosing a chat model

The live agent is Goose (`GooseAdapter` in `pond-adapters-goose`). The chat model is chosen in
the app's Models page and stored as the `chat_provider` and `chat_model` settings:

| `chat_provider` | Runs |
|---|---|
| `local` | in-process: a `.gguf` through Goose's llama.cpp backend, a `.litertlm` through its LiteRT-LM backend (build or import the library with `giap.sh litert`) |
| `ollama` | an Ollama server on `127.0.0.1:11434` |
| `llamafile` | a llamafile the pond starts from `models/llm/` |
| `mistralrs` | a mistral.rs server over its OpenAI-compatible API |
| `mesh` | another pond over libp2p (`--features mesh` builds only) |

## Running

```bash
cargo run -p pond-server --release -- serve --open     # server + dashboard in the browser
cargo run -p pond-server --release -- serve --native   # macOS: also launch the desktop app
cargo run -p pond-server --release -- chat --voice     # wake word, speech, spoken replies
cargo run -p pond-server --release -- dashboard        # a sign-in link for a browser
cd pond-desktop && npm run dev:electron                # the Electron shell in development
```

`serve` listens for HTTP on `127.0.0.1` (default 4000, falling back through 4009) and HTTPS on
all interfaces (default 4443); `--port` and `--https-port` override. `chat --voice` fetches any
missing voice model on first use. The web UI is embedded into the binary at compile time from
`pond-desktop/dist`; a binary built without it falls back to `--static-dir`
(default `pond-desktop/dist`).

## Production deployment (Linux)

```bash
bash scripts/giap.sh install          # auto-detects production mode on a headless Linux box
bash scripts/install.sh --production --dedicated --port 80
```

Service control afterwards is the `giap.sh` menu, items 20 to 25: install, start, stop, restart,
remove the unit, run in the foreground. The pond logs to files, not journald (`giap.sh logs`).

### The pond's own account (`agamemnon`)

On a new production or Jetson install, the service step asks whether the pond runs as its own
locked system account, `agamemnon`, and defaults to yes. A pond that already has a unit, at either
scope, keeps its account until someone runs `giap.sh service-account`.

| Piece | What it is |
|---|---|
| Account | `useradd --system`, locked, shell `/usr/sbin/nologin`, home `/var/lib/goose-in-a-pond`, in whichever of `video`, `render`, `audio`, `i2c` and `gpio` exist |
| Data | moved to `/var/lib/goose-in-a-pond` (`0750`, owned by `agamemnon`); a link stays at the old path. The login keeps read access through an ACL that leaves private files (`setup/`, `secrets/`, the host credential) private |
| Binary | stays in the login's checkout. An ACL gives `agamemnon` traverse-only access to the login's home (usually `0750`) |
| Sound | `loginctl enable-linger agamemnon` starts its user manager at boot, which runs its own PulseAudio (or PipeWire) like any login's; the unit sets `XDG_RUNTIME_DIR=/run/user/<uid>` so ALSA's `default` and `pactl` reach it |
| Unit | `/etc/systemd/system/goose-in-a-pond.service`, `User=agamemnon`, after `user@<uid>.service` |
| Power | the sudoers rule `giap.sh admin-card` writes names `agamemnon` |
| Login | `/etc/sudoers.d/goose-in-a-pond-login`: the login may start, stop and restart the unit and run commands as `agamemnon` (`sudo -n -u agamemnon ...`) without a password, so deploys and builds run unattended |

```bash
bash scripts/giap.sh service-account          # move this pond to agamemnon (sudo)
bash scripts/giap.sh service-account status   # the doctor line
bash scripts/giap.sh service-account undo     # back to the account, unit and data dir it came from
```

The move keeps what undo needs in `/var/lib/goose-in-a-pond-admin/before-agamemnon/` (root only):
the old scope, user, data directory and unit file. Undo leaves the account in place, locked and
owning nothing (`sudo userdel agamemnon` removes it), and removes the login's drop-in. After a
move, run the pond's own commands as it: `sudo -u agamemnon env POND_DATA_DIR=/var/lib/goose-in-a-pond
XDG_RUNTIME_DIR=/run/user/$(id -u agamemnon) <checkout>/target/release/pond-server chat --voice`. The
desktop app's voice loop does not do this yet; see
[what this does not cover yet](../architecture/pond-administrator.md#not-in-this-change).

### Wi-Fi from the BLE setup channel (the polkit grant)

A headless pond gets its Wi-Fi from a phone over Bluetooth
([the setup channel](../architecture/pond-setup-channel.md)) and joins it through NetworkManager
as its own user. NetworkManager's default policy refuses that user without a desktop session, so
a production or Jetson install writes a polkit grant for the pond's user (the system unit's
`User=`, else whoever runs the installer). It allows exactly four actions under
`org.freedesktop.NetworkManager.`: `settings.modify.system`, `network-control`, `wifi.scan` and
`enable-disable-wifi`.

| polkit | File |
|---|---|
| 0.105 (Ubuntu 22.04, JetPack 6) | `/etc/polkit-1/localauthority/50-local.d/50-giap-network.pkla` |
| 0.106 and later | `/etc/polkit-1/rules.d/50-giap-network.rules` |

```bash
bash scripts/giap.sh wifi-grant            # write it (sudo); again is a no-op
bash scripts/giap.sh wifi-grant status     # the doctor lines below
bash scripts/giap.sh wifi-grant remove     # take it out, either flavour
bash scripts/giap.sh --dry-run wifi-grant  # print the file, write nothing
```

polkit reads the file at once, with no restart. On Linux, `giap.sh doctor` checks rfkill for
Bluetooth and Wi-Fi (`rfkill unblock bluetooth` lifts a soft block), that `bluetooth.service`
runs, that the adapter offers LE advertising (`LEAdvertisingManager1.SupportedInstances`, else
`btmgmt info`), and the grant as NetworkManager answers it for this user
(`nmcli general permissions`). The pond's Bluetooth needs no grant: it never pairs, registers no
agent and leaves the adapter's discoverable and pairable switches alone.

## Jetson Orin Nano

The CUDA build has to happen on the device (nvcc targets its architecture). Deploy from the dev
machine: it builds the web UI locally, syncs it, hard-resets the device checkout to
`origin/<branch>` and builds there with the CUDA features. Push the branch first; `giap.sh`
warns when HEAD is not on the remote.

```bash
bash scripts/giap.sh deploy           # or: bash scripts/jetson.sh deploy
bash scripts/jetson.sh docker-build   # a CPU-only aarch64 binary in a linux/arm64 container
```

See [the Jetson dev routine](jetson-dev-routine.md) for the workflow and its traps, and
[scripts/jetson/README.md](../../scripts/jetson/README.md) for the scripts.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `goose/Cargo.toml` not found | `git submodule update --init --recursive`, or `giap.sh` menu 3 |
| `revision ... not found`, `failed to authenticate` fetching `llama-cpp-rs-giap` | a git credential that can read `jarida-io/llama-cpp-rs-giap` ([CONTRIBUTING](../CONTRIBUTING.md#prerequisites)) |
| `Unable to find libclang` (Jetson) | `sudo apt install libclang-dev clang` |
| Whisper or llama.cpp build fails (macOS) | `brew install cmake`, then rebuild |
| `ld: write() failed, errno=28` or another strange link error | the disk is full: `du -sh target`, then `rm -rf target/debug/incremental` or `giap.sh` menu 4 |
| Port 4000 already in use | the server falls back through 4009 on its own; read `<data_dir>/.runtime_api_port`, or pass `--port` |
| Dashboard is a placeholder page | the server was built before the UI: `giap.sh build-ui`, then rebuild the server |
| `npm test` or Electron fails oddly | Node outside the range: `bash scripts/giap.sh node` |
