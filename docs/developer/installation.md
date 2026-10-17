# Installation Guide

## Quick Start

```bash
git clone --recursive https://github.com/jarida-io/goose-in-a-pond.git
cd goose-in-a-pond
bash scripts/giap.sh          # menu: install, build, service, logs, doctor
```

`scripts/giap.sh` is the primary interface. It detects the host (Jetson /
generic Linux / macOS), whether CUDA is usable, and the known bad states, then
offers only the actions that apply. Non-interactive equivalents:

```bash
bash scripts/giap.sh install     # first-time install (-y to skip confirmations)
bash scripts/giap.sh build       # web UI + pond-server with this host's features
bash scripts/giap.sh doctor      # health report; exits 1 on any FAIL
```

Always run `doctor` after installing.

### What `giap.sh install` does

It delegates the work to `scripts/install.sh` — which remains the only place
that knows the full sequence — and adds the guardrails install.sh cannot have,
because they depend on host state it does not inspect:

- **Refuses to add a second service unit.** `install.sh` writes a root-owned
  unit to `/etc/systemd/system/`; a Jetson set up by hand runs a *user* unit of
  the same name. Both installed means two servers, each loading its own ~3 GB
  model into one memory pool. `giap.sh` passes `--no-service` when a unit exists
  at either scope.
- **Warns when Node is too old to build the web UI**, since the server would
  then embed the `build.rs` placeholder and nothing at runtime can tell you.
- **Stops a running service before a Jetson release build**, because the linker
  is OOM-killed while a model is resident.

## Install Script Options

`scripts/install.sh` is what `giap.sh install` calls underneath. Its real flags
(from `scripts/lib/install-common.sh`):

```
bash scripts/install.sh [OPTIONS]

Modes (auto-detected if omitted):
  --production      Linux: systemd service + mDNS + auto-start
  --jetson          Jetson Orin Nano (CUDA auto-detect, SQLX_OFFLINE)
  --minimal         Fastest: server + DB only, no model downloads

Build:
  --full            Build the entire workspace including Goose (10+ min)
  --desktop         Also build the Electron desktop app (macOS only)
  --no-verify       Skip the post-install health check

Models:
  --ollama          Use Ollama as the LLM provider
  --llamafile       Use llamafile as the LLM provider
  --no-models       Skip all model downloads
  --whisper-model MODEL   tiny|base|small (default: base)

Production:
  --port PORT       Override the port
  --dedicated       Set hostname to 'pond', serve on port 80
  --shared          Keep hostname, use a non-privileged port
  --no-service      Skip systemd service creation
  --data-dir DIR    Override the data directory
```

There is no `--fast` flag; it was documented here but never parsed.

## Prerequisites

| Requirement | macOS | Linux |
|------------|-------|-------|
| Git | Xcode CLI tools | `apt install git` |
| Rust | [rustup.rs](https://rustup.rs) | [rustup.rs](https://rustup.rs) |
| C compiler | `xcode-select --install` | `apt install build-essential` |
| cmake | `brew install cmake` | `apt install cmake` |
| pkg-config | `brew install pkg-config` | `apt install pkg-config` |
| Node.js (desktop and tests; the UI build needs less) | `bash scripts/giap.sh node` | `bash scripts/giap.sh node` |

The install script auto-installs cmake and pkg-config via Homebrew (macOS) or apt (Linux).

### Node

Three different floors, each read off the packages that set it, not chosen:

| For | Needs | Because |
|-----|-------|---------|
| The desktop app and the tests (the repo's range) | `^22.13.0 \|\| ^24.0.0 \|\| >=26.0.0` | ESLint 10 wants 22.13+ in the 22 line; vitest 5 rules out the odd-numbered 23 and 25 |
| Building the web UI | `^20.19.0 \|\| >=22.12.0` | Vite 8. A server (the Jetson) only needs this |
| The Matter controller | `>=20.19 <22.0 \|\| >=22.13` | matter.js 0.17. Every Node in the repo's range qualifies; a server with only the UI floor may not, and the doctor says so |

`.nvmrc` says `22`, `pond-desktop/package.json` carries the range as `engines`, and CI's frontend job
runs on 22. A Node outside the range is the usual reason `npm test` or `npm run dev:electron` fails in a way
that does not look like a Node problem.

```bash
bash scripts/giap.sh node                  # check; use one you have; else offer a download
bash scripts/giap.sh node --check          # report only, change nothing (exit 1 if the shell's node is wrong)
bash scripts/giap.sh node -y               # no prompts (scripts, CI)
bash scripts/giap.sh node --dry-run        # say what it would do
bash scripts/giap.sh node --method nvm     # install through nvm (or fnm, volta); default: auto
bash scripts/giap.sh node --major 24       # which major to install when none is present
bash scripts/giap.sh node --install-deps   # then `npm ci` in pond-desktop and extensions/music
```

What it does, in this order, stopping at the first that works:

1. **The Node on your PATH**, if it is inside the range. Nothing else happens.
2. **One already installed**: it looks in nvm, fnm, volta, asdf and in `~/.giap/node` (an earlier run of
   this). It asks each binary for its version and skips one that will not run. If one is inside the range
   it is **recorded** in `~/.giap/node/.path`, and nothing is downloaded.
3. **An install**, only after asking (`-y` answers for you; with no terminal and no `-y` it refuses):
   through nvm, fnm or volta if you have one, else a download from `nodejs.org/dist` into `~/.giap/node`.
   The download is **checked against the SHA-256 nodejs.org publishes** in the release's `SHASUMS256.txt`,
   unpacked beside its destination and moved into place only after the check passes; on a mismatch nothing
   is installed and the run fails, naming both hashes. It is https only (TLS 1.2+). Builds exist for macOS and Linux on
   arm64 and x64, so the Jetson is covered; anything else is reported, not guessed at.

Nothing is installed globally and nothing is added to a shell profile. `GIAP_NODE_HOME` moves the download
folder; `GIAP_NODE_DIST_URL` points it at a mirror.

The recorded Node is what makes the rest of `giap.sh` work whatever your shell's default is: every command
(`build`, `doctor`, the menu) puts the right Node first on PATH for its own run if the shell's is outside the
range, and says so in the banner and the doctor. Your own shell is left alone; `giap.sh node` prints the line to type
(`nvm use 26.10.0`, `export PATH=...`). `giap.sh build-ui` and `giap.sh install` do the same when they find
a Node that cannot build the UI, and offer the download before falling back to "build it elsewhere and rsync".

**The pond reads the record too** (`crates/pond-server/src/node_path.rs`). When it starts, if the `node` on
its own PATH is outside the range, or there is none, it puts the recorded one first, so the Matter controller
and the stdio extensions it starts run on it. That is the Jetson's case: apt's Node 12 in `/usr/bin` comes
first on a systemd service's PATH. A `node` inside the range is left alone, which is the order `giap.sh`
itself takes, and without a record nothing is asked. It asks each binary for its version, as the script does,
and gives up on one that has not answered in 3 seconds. The record is read only at start, so **restart the
pond after `giap.sh node`**; its log says what it did (`node_path_recorded`). The doctor says whether the
pond will use the Node it is reporting on: an installed one that `giap.sh` found but never recorded, it will
not.

The script is tested in `scripts/lib/node-setup.test.sh` (against a fake nodejs.org, so no network; run by
CI's `node-setup` job): `bash scripts/lib/node-setup.test.sh`.

## What Gets Installed

### Data Directory

| Platform | Default Path |
|----------|-------------|
| macOS | `~/Library/Application Support/goose-in-a-pond/` |
| Linux | `~/.local/share/goose-in-a-pond/` |
| Custom | Set `POND_DATA_DIR=/path` or `--data-dir` |

### Directory Structure

```
$DATA_DIR/
├── pond_system.db              SQLite (sessions, settings, memory, skills)
├── pond_logs.db                SQLite (telemetry, sensor readings)
├── schedules.json              Scheduled task definitions
├── schedule_runs.json          Execution history
├── bin/
│   ├── whisper-server          Whisper ASR binary
│   ├── piper                   Piper TTS binary
│   └── espeak-ng-data/         TTS voice data
├── models/
│   ├── ggml-base.en.bin        Whisper model (~74 MB)
│   ├── tts/
│   │   ├── en_US-lessac-medium.onnx       TTS voice (~63 MB)
│   │   └── en_US-lessac-medium.onnx.json  TTS config
│   ├── gguf/                   GGUF models (local inference)
│   └── face/                   Face recognition ONNX models
├── prompts/                    User prompt overrides
└── setup/                      0700: the printed setup card's code and claim
    ├── setup.json              0600; never regenerated, keep a backup
    └── card.svg                0600; the card install wrote, to print
```

### The setup card

Install makes the pond's setup code (`pond-server setup-code`) and writes a printable card to
`$DATA_DIR/setup/card.svg`. **Print it and keep it with the pond.** A pond with no screen is set
up by scanning the card with Goose On The Go, which claims the pond; the card is accepted only
until the first device pairs. To print it again, or to see the code:

```bash
pond-server setup-code                      # the code, the pond id, a terminal QR
pond-server setup-code --card card.svg      # the printable card again
pond-server setup-code --status             # unclaimed, claimed or missing (doctor uses this)
pond-server setup-code --rearm              # accept the same card again after a factory reset
```

For a pond imaged elsewhere, `bash scripts/giap.sh provision --host <pond> --key <file>` fetches
its card over SSH and saves it on your machine as `./pond-<id>-setup-card.svg` (`--card` to choose
the path). `giap.sh doctor` reports the claim state. A pond that already had paired phones when it
got its first setup code is created claimed, so upgrading a pond in use opens nothing.

## LLM Provider Options

GIAP supports three LLM backends. You need at least one:

### Ollama (recommended for development)

```bash
# Install: https://ollama.com
ollama pull gemma3:4b    # 2.6 GB — works great with GIAP
# Or for Gemma 4:
ollama pull gemma4:latest
```

Configure in GIAP: Settings → Chat Provider = "ollama", Model = "gemma3:4b"

### Llamafile (self-contained, no dependencies)

```bash
# The install script downloads this with --llamafile:
bash scripts/install.sh --llamafile
```

### Local GGUF (in-process, no subprocess)

```bash
# Requires --features local-inference (default)
# Place .gguf file in $DATA_DIR/models/gguf/
# Configure: Settings → Chat Provider = "local", Model = "filename.gguf"
```

## Running

```bash
# Start server + web dashboard
cargo run -p pond-server --release -- serve --open

# Voice mode (requires whisper-server running on port 9000)
cargo run -p pond-server --release -- chat --voice

# Desktop app
cd pond-desktop && npm run dev:electron
```

## Production Deployment (Linux)

For headless deployment on a Linux server:

```bash
bash scripts/giap.sh install          # auto-detects production mode
# or directly:
bash scripts/install.sh --production --dedicated --port 80
```

`scripts/setup.sh` no longer exists — `install.sh` replaced both it and the old
dev installer.

Service control afterwards is `bash scripts/giap.sh` menu 20-25 (install, start,
stop, restart, status, logs, uninstall).

## Jetson Orin Nano

Cross-compilation is **not** viable for the local-inference path: nvcc must
target the device architecture, so the CUDA build has to happen on the Jetson.
Deploy from your dev machine instead — it builds the web UI locally (the
device's Node is too old for Vite), syncs it, then builds on-device with the
correct feature flags:

```bash
bash scripts/giap.sh deploy           # or: bash scripts/jetson.sh deploy
```

For a CPU-only aarch64 binary without CUDA, `bash scripts/jetson.sh docker-build`
builds in a linux/arm64 container.

See [scripts/jetson/README.md](../../scripts/jetson/README.md) for the full
workflow and [docs/jetson-build-and-run.txt](../jetson-build-and-run.txt) for
the operational traps.

## Troubleshooting

### "goose/Cargo.toml not found"
```bash
git submodule update --init --recursive
```

### "Unable to find libclang" (Jetson)
```bash
sudo apt install libclang-dev clang
```

### "instruction requires: fullfp16" (Jetson ARM)
```bash
# Add to .cargo/config.toml:
[target.aarch64-unknown-linux-gnu]
rustflags = ["-C", "target-cpu=native"]
```

### Whisper build fails (macOS)
```bash
brew install cmake
# Re-run setup:
cargo run -p pond-server -- setup
```

### Port 4000 already in use
```bash
cargo run -p pond-server --release -- serve --port 8080
```
