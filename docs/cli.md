# GIAP Command-Line Reference

Three front doors, from most to least common:

| Tool | For | Defined in |
|---|---|---|
| `bash scripts/giap.sh` | Installing, building, updating, deploying and diagnosing a pond | `scripts/giap.sh` |
| `make <target>` | Short aliases that forward to `giap.sh` or `scripts/jetson.sh` | `Makefile` |
| `pond-server <command>` | Running the pond and managing its data | `enum Commands` and the `*Action` enums in `crates/pond-server/src/main.rs` |

- [giap.sh](#giapsh)
- [Makefile targets](#makefile-targets)
- [pond-server](#pond-server)
- [Environment variables](#environment-variables)

---

## giap.sh

```bash
bash scripts/giap.sh [OPTIONS] [COMMAND [SUBCOMMAND [ARG]]]
```

It detects the host (Jetson, other Linux, macOS), whether CUDA is usable, the service unit's scope
and the known bad states, then delegates to the other scripts (`scripts/install.sh`,
`scripts/jetson.sh`, `scripts/lib/*.sh`). Every command except `node` first puts a Node in the
repo's range on `PATH` for the run, if the shell's own is outside it. With no command it opens an
interactive menu.

### Commands

| Command | What it does |
|---|---|
| *(none)* | Interactive menu: diagnose, install and build, service install/start/stop/restart/remove, run in the foreground, desktop app, logs, Jetson power |
| `status` | Print the detection banner only |
| `doctor` | Banner plus a read-only health report; exits 1 if any check FAILs |
| `install` | Guardrailed `bash scripts/install.sh`: passes `--no-service` when a service unit already exists, offers to stop a running Jetson service for the build, and offers Full or Minimal (`--minimal`) when interactive |
| `build` | `build-ui`, then a release `pond-server` with this host's features (CUDA features on a Jetson; stops the service during the build there). Also builds the `pondnet` remote-access helper when Go is installed, and writes a build stamp |
| `build-ui` | `npm ci` when the lockfile is newer than `node_modules`, then `npm run build` in `pond-desktop/`, then touches `dist/` so cargo re-embeds it |
| `build-desktop` | macOS only: `npm ci && npm run bundle:app` in `pond-desktop/` (stages a release sidecar first) |
| `update` | Fast-forward to the upstream branch (refuses a dirty checkout or local commits), update the goose submodule, ensure Node, rebuild when the build stamp's commit differs, restart the service and wait for health, offer the Wi-Fi grant on Linux, then run `doctor` |
| `deploy` | From a dev machine: `bash scripts/jetson.sh deploy`, after warning if `HEAD` is not on the remotes. Refuses to run on the Jetson |
| `logs` | Shows today's rolling log file (`<data_dir>/logs/pond.log.YYYY-MM-DD`) and offers tail, errors, turn telemetry or engine lines |
| `gui` | macOS only: launch the installed or locally packaged app against the running server (`GIAP_SERVER_PORT`) |
| `node` | Report the Node the repo needs (range `^22.13.0 \|\| ^24.0.0 \|\| >=26.0.0`, preferred major from `.nvmrc`), or find, download and verify one |
| `litert status` | The LiteRT-LM C API library goose's `litert` backend loads for `.litertlm` models: recorded packages, verified |
| `litert build [macos-arm64\|linux-arm64]` | Build, package, verify and record the library; default platform is this host's |
| `litert import DIR` | Take in a package built on another host |
| `onnxruntime status` | Jetson: the GPU ONNX Runtime the vision and face models run on (TensorRT, then CUDA), verified against its manifest, and any build of it with its phase and progress |
| `onnxruntime build` | Jetson: build ONNX Runtime from source as onnxruntime.ai documents it, in the background (hours, logged to `~/.giap/onnxruntime/build.log`, resumable), install it under `<data_dir>/lib/onnxruntime-gpu/`, remove the 2.5 GB build tree (the wheel stays in `~/.giap/onnxruntime/dist/`), then build the TensorRT engines. Stops the pond while the build needs its memory and starts it again afterwards. With the install current for the device's CUDA and TensorRT, it only checks the engines |
| `models pairings` | Regenerate the vision pairing table from Hugging Face |
| `wifi-grant [remove\|status]` | Linux: install, remove or report the polkit grant that lets the pond's user join Wi-Fi from the BLE setup channel (uses sudo) |
| `admin-card [show\|rotate]` | My Pond's admin card: make it as the pond's user, seal a recovery copy (the TPM on a Jetson, a root-only file without one, the Keychain on macOS) and, on a Jetson, let the pond's user switch power modes and restart (uses sudo). `show` prints the sealed card again; `rotate` replaces it and locks every phone. See [pond administrator](architecture/pond-administrator.md) |
| `service-account [undo\|status]` | Linux: run the pond as its own locked system account, `agamemnon` (no password, shell `/usr/sbin/nologin`). Stops the pond, makes the account, moves the data directory to `/var/lib/goose-in-a-pond` (a link stays at the old path), writes a system unit with `User=agamemnon`, points the power rule and the Wi-Fi grant at it, and starts it (uses sudo). `undo` puts back the account, unit and data directory it came from; `status` is the doctor line. See [pond administrator](architecture/pond-administrator.md#the-service-account) |
| `provision --host HOST --key FILE [...]` | Operator machine: save a pond's printed setup card and give it a device certificate over SSH. Must be the first argument; runs `scripts/provision-device.sh` |
| `check [AREA...] [--all\|--e2e\|--full\|--base REF\|--dry-run]` | The checks CI runs, for the areas this branch changes against `main` (`rust`, `desktop`, `go`, `shell`, `matter`, `music`), or the named ones. Reads the Rust crate lists from `ci.yml`; refuses Rust early without access to the private llama.cpp fork. Must be the first argument; `scripts/lib/local-checks.sh` |

### Options

| Option | Applies to | Effect |
|---|---|---|
| `--dry-run` | all | Print every command instead of running it |
| `-y`, `--yes` | all | Answer yes to confirmation prompts |
| `--check` | `node`, `models pairings` | Report only, change nothing; exit 1 if the shell's Node is wrong or the committed table is stale |
| `--method nvm\|fnm\|volta\|download` | `node` | How to install Node (default `auto`) |
| `--major N` | `node` | Node major to install |
| `--install-deps` | `node` | Also run `npm ci` in `pond-desktop/` and `extensions/music/` |
| `--distdir DIR` | `litert build` | A Bazel distdir of pre-fetched dependency archives for the build |
| `-h`, `--help`, `help` | all | Print the usage header |

`provision` takes its own options (`--host`, `--key`, `--data-dir`, `--pondnet`, `--pond-server`,
`--card`); see the header of `scripts/provision-device.sh`.

The bash tests for the Node setup are `bash scripts/lib/node-setup.test.sh`.

## Makefile targets

| Target | Runs |
|---|---|
| `make install` | `bash scripts/giap.sh install` |
| `make build` | `bash scripts/giap.sh build` |
| `make ui` | `bash scripts/giap.sh build-ui` |
| `make server` | `bash scripts/giap.sh build` (UI and server; there is no server-only verb) |
| `make desktop` | `cd pond-desktop && npm ci && npm run bundle:app` |
| `make doctor` | `bash scripts/giap.sh doctor` |
| `make status` | `bash scripts/giap.sh status` |
| `make deploy` | `bash scripts/jetson.sh deploy` |
| `make docker-build` | `bash scripts/jetson.sh docker-build` (aarch64 CPU-only binary in a linux/arm64 container) |
| `make test` | `SQLX_OFFLINE=true cargo test -p pond-core -p pond-api -p pond-infra` |
| `make check` | `bash scripts/giap.sh check` |
| `make fmt` | `cargo fmt` |
| `make help` | List the targets |

`make test` covers three crates; the full fast-crate list CI tests is in `.github/workflows/ci.yml`.

---

## pond-server

```bash
pond-server [COMMAND]
```

`-h`/`--help` prints help for the binary or any command; `-V`/`--version` prints the version. With
no command it starts a keyboard chat with the provider from Settings, no wake word and no speech
(the same as `chat --no-wake-word --tts none`).

Every command works in the data directory: `$POND_DATA_DIR` if set, else the platform data
directory plus `goose-in-a-pond` (`~/Library/Application Support/goose-in-a-pond` on macOS,
`~/.local/share/goose-in-a-pond` on Linux). Run it from the repo with
`cargo run -p pond-server -- <command>`.

| Command | Summary |
|---|---|
| [`setup`](#setup) | First-time setup: databases, catalog, ASR model, checks |
| [`serve`](#serve) | Start the HTTP and HTTPS servers |
| [`chat`](#chat) | Interactive chat in the terminal, by keyboard or voice |
| [`status`](#status) | Show system status |
| [`onboard`](#onboard) | Interactive onboarding wizard |
| [`models`](#models) | Browse and manage models |
| [`agent`](#agent) | One-shot or REPL chat through the agent loop, no voice |
| [`prompts`](#prompts) | Manage system prompt templates |
| [`skills`](#skills) | Manage skills |
| [`recipes`](#recipes) | Manage recipes |
| [`memories`](#memories) | Manage memory fragments |
| [`calibrate`](#calibrate) | Calibrate the wake-word detector |
| [`pairing`](#pairing) | Show or refresh the device pairing code |
| [`setup-code`](#setup-code) | Show the printed setup code and its claim state |
| [`admin-card`](#admin-card) | Make or replace the admin card that unlocks My Pond |
| [`dashboard`](#dashboard) | Print a sign-in link for the web dashboard |

`ble-helper` is hidden: the server runs it as its own child for the BLE setup channel on macOS.

### setup

```bash
pond-server setup [--model <SIZE>]
```

| Flag | Default | Description |
|---|---|---|
| `--model` | `base` | Whisper model size: `tiny` (~39 MB), `base` (~74 MB), `small` (~244 MB) |

Steps, as it prints them:

1. Check system dependencies; warns and continues on failure.
2. Create `pond_system.db` and `pond_logs.db` and run migrations; apply the stored network mode;
   move flat model files into the content-addressed layout; fetch the model catalog; seed the
   built-in prompt templates (`balanced`, `concise`, `technical`, `warm`) where absent.
3. Download the Whisper model for in-process recognition (skipped if present).
4. Nothing to download for Whisper itself: it runs in-process.
5. Look for a TTS voice model (`.onnx`) in the TTS models directory and say where to get one. The
   step's output still calls it a Piper voice.
6. Check for ONNX Runtime.
7. Check for `espeak-ng-data`, symlinking a Homebrew or system copy when found.
8. Check microphone access.

### serve

```bash
pond-server serve [--static-dir <PATH>] [--open] [--debug] [--agent <BACKEND>]
                  [--port <PORT>] [--https-port <PORT>] [--native]
```

| Flag | Default | Description |
|---|---|---|
| `--static-dir` | `pond-desktop/dist` | Built web dashboard assets. Used only when the binary was built without an embedded UI |
| `--open` | off | Open the dashboard in the browser |
| `--debug` | off | Debug logging (noisy third-party crates capped) |
| `--agent` | `goose` | Agent backend: `goose`, `mistralrs` (needs the `mistralrs-agent` feature), `pond` (quarantined: overridden at startup), `mock` |
| `--port` | 4000 | Dashboard HTTP port on `127.0.0.1`; the next of 4001..4009 if taken |
| `--https-port` | 4443 | Companion HTTPS port on all interfaces, with sequential fallback |
| `--native` | off | Also launch the desktop app (macOS): `/Applications`, then `pond-desktop/release/`, then the dev Electron runtime. On Linux it says so and keeps serving |

The bound ports are written to `<data_dir>/.runtime_api_port` and `.runtime_https_port`, and the
host credential to `.runtime_host_credential`. The server stops on Ctrl-C or SIGTERM. The routes
it serves are in [api.md](api.md).

### chat

```bash
pond-server chat [-P <PROVIDER>] [-M <MODEL>] [--voice] [--wake-word <PHRASE>] [--no-wake-word]
                 [--tts <ENGINE>] [--session-id <ID>] [--json-events]
```

| Flag | Default | Description |
|---|---|---|
| `-P`, `--provider` | from Settings | `mock`, `llamafile`, `ollama`, or `local` (in-process GGUF; needs the `local-inference` feature) |
| `-M`, `--model` | from Settings | Model name for `--provider ollama` |
| `--voice` | off | Listen on the microphone: wake word, speech detection, recognition and spoken reply, fetching whatever is missing |
| `--wake-word` | from Settings | Wake phrase (with `--voice`) |
| `--no-wake-word` | off | Skip wake-word detection; listen at once each turn |
| `--tts` | `kokoro` | `kokoro`, or `none` to print only |
| `--session-id` | `default-session` | Session id for history; readable through `GET /api/v1/sessions/{id}/messages` |
| `--json-events` | off | Write the workflow as NDJSON on stdout, one event per line, and nothing else; diagnostics go to stderr. The desktop shell's voice mode runs `chat --voice --json-events` as a child process |

The loop cycles Wait, Listen, Think, Speak. Ctrl-C or end of input exits.

```bash
pond-server chat --provider mock --no-wake-word --tts none
printf 'What is the weather today?\n' | pond-server chat --provider mock --no-wake-word --tts none
pond-server chat --voice
```

### status

```bash
pond-server status
```

Prints the version, hostname, platform, database path, assistant name, provider, model, wake word
and onboarding step. Says to run `setup` when there is no database.

### onboard

```bash
pond-server onboard [--reset]
```

| Flag | Description |
|---|---|
| `--reset` | Clear the onboarding record and start again |

Walks the steps in order: Welcome, Basics (your name), Language and Location (timezone),
Accessibility, Personality (`balanced`, `concise`, `technical` or `warm`), Goose's Identity
(assistant name), Wake Word (presets or custom), Model, Extensions, then Completed. Blank answers
can be set later in Settings.

### models

```bash
pond-server models list [--category <CATEGORY>]
pond-server models download <CATEGORY> <NAME>
pond-server models delete <CATEGORY> <NAME>
pond-server models activate <CATEGORY> <NAME> --role <ROLE>
```

| Command | Description |
|---|---|
| `list` | The catalog with download state and role. `--category` takes `gguf`, `llamafile`, `whisper`, `tts`, `ollama` or `embedding` |
| `download` | Download a catalog model to disk |
| `delete` | Delete the file; the catalog row stays |
| `activate` | Assign a model to a role. `--role` is required: `chat`, `think`, `task`, `asr` or `tts` |

### agent

```bash
pond-server agent chat <MESSAGE> [--session <ID>] [--role <ROLE>]
pond-server agent repl [--session <ID>] [--role <ROLE>]
pond-server agent tools
pond-server agent extras
```

| Command | Flags | Description |
|---|---|---|
| `chat` | `--session` (default `cli-agent`), `--role` (default `auto`) | Send one message through the Goose agent loop and stream the answer; text on stdout, status and tool calls on stderr |
| `repl` | `--session` (default `cli-repl`), `--role` (default `auto`) | Multi-turn conversation |
| `tools` | | List the MCP extensions and tools the agent has loaded |
| `extras` | | List the system prompt extras |

`--role` takes `auto`, `chat`, `think` or `task`. `auto` is treated as `chat`; nothing classifies
the message.

### prompts

```bash
pond-server prompts list
pond-server prompts show <NAME>
pond-server prompts reset <NAME>
```

| Command | Description |
|---|---|
| `list` | Every template, built-in and user-defined |
| `show` | A template's full content |
| `reset` | Restore a built-in template (`balanced`, `concise`, `technical`, `warm`) to its factory text |

User templates are created and deleted through the API (`PUT`/`DELETE /api/v1/prompts/{name}`).

### skills

```bash
pond-server skills list [--all]
pond-server skills add <NAME> [--description <TEXT>] [--icon <KEY>] [--content <MARKDOWN>]
pond-server skills toggle <ID>
pond-server skills remove <ID>
```

| Command | Flags | Description |
|---|---|---|
| `list` | `--all` includes inactive skills | List skills |
| `add` | `--description` (default empty; always visible to the model, so keep it brief), `--icon` (default `sparkles`; cosmetic), `--content` (omit to read stdin) | Add a skill |
| `toggle` | | Flip a skill's active state by UUID |
| `remove` | | Delete a skill by UUID |

### recipes

```bash
pond-server recipes list
pond-server recipes show <NAME>
pond-server recipes import <NAME> <FILE> [--description <TEXT>]
pond-server recipes remove <NAME>
```

| Command | Description |
|---|---|
| `list` | All recipes |
| `show` | A recipe's YAML |
| `import` | Store a Goose recipe YAML file under a slug; `--description` defaults to empty |
| `remove` | Delete a recipe by slug |

### memories

```bash
pond-server memories list [--limit <N>]
pond-server memories add <CONTENT>
pond-server memories remove <ID>
```

| Command | Description |
|---|---|
| `list` | Recent fragments, newest last; `--limit` defaults to 20 |
| `add` | Save a fragment |
| `remove` | Delete a fragment by UUID |

### calibrate

```bash
pond-server calibrate [--phrase <PHRASE>] [--samples <N>] [--whisper-url <URL>] [--reset]
```

Records short clips of the wake phrase and stores what recognition heard as trigger variants, so
the detector matches the phrase as Whisper tends to mishear it.

| Flag | Default | Description |
|---|---|---|
| `--phrase` | `voice_wake_word` from Settings | Phrase to calibrate |
| `--samples` | 5 | Clips to record |
| `--whisper-url` | | Accepted and ignored: recognition is in-process, with the model named by `active_whisper_model` |
| `--reset` | off | Clear existing calibration first |

See [wake-word-calibration.md](wake-word-calibration.md).

### pairing

```bash
pond-server pairing [--refresh]
```

Asks the running server (`127.0.0.1:<.runtime_api_port>`, with the host credential) for the current
pairing code, or for a new one when none is live or `--refresh` is given. Prints the six-digit code,
its expiry, where the phone connects, the key it pins, and a QR code for Goose On The Go. Fails if
the server is not running under this account.

### setup-code

```bash
pond-server setup-code [--json] [--card <PATH|->] [--rearm]
pond-server setup-code --status
```

The printed setup code a pond with no screen is set up with. It is made once, the first time this
runs or the server starts, never changes, and is accepted only until the first device pairs. Works
with or without the server running.

| Flag | Description |
|---|---|
| `--json` | Print `{"code","display","id","claimed_at","url"}` instead of the card text |
| `--card PATH\|-` | Also write a printable SVG card to `PATH` (mode 0600), or to stdout with `-` |
| `--status` | Print only `unclaimed`, `claimed` or `missing`; never makes a code. Conflicts with the other flags |
| `--rearm` | Accept the same card again; devices already paired keep working |

### admin-card

```bash
pond-server admin-card [--json] [--rotate]
pond-server admin-card --status
```

The password a member types into My Pond on their phone. It prints the password once and keeps
only a PBKDF2 verifier in `<data_dir>/setup/admin.json`; a running server reads the new card on
the next unlock. Use `bash scripts/giap.sh admin-card`, which also seals a recovery copy. Without
`--rotate` it refuses to replace a card in force.

| Flag | Description |
|---|---|
| `--json` | Print `{"password","card_id"}` instead of the card text |
| `--rotate` | Replace the card in force; every phone unlocked with the old one locks, and every phone's own key is revoked |
| `--status` | Print only `present` or `missing`; never makes a card. Conflicts with the other flags |

### dashboard

```bash
pond-server dashboard
```

Prints `http://127.0.0.1:<port>/#host=<credential>`, built from `.runtime_api_port` and
`.runtime_host_credential`. A browser needs the host credential to pair phones or manage remote
access; the part after `#` is never sent over the network. The link stops working when the server
restarts. Through an SSH tunnel (`ssh -L 9000:127.0.0.1:<port> <pond>`), change only the port.
Anyone holding the link can pair devices until the restart, so do not share it.

---

## Environment variables

Development and test knobs; a normal install sets none of them.

| Variable | Effect |
|---|---|
| `POND_DATA_DIR` | Use this data directory instead of the platform default |
| `RUST_LOG` | Tracing filter; when set it wins over `--debug` for both the console and the log files |
| `POND_DEV_ALLOW_LOOPBACK=1` | Loopback requests skip bearer-token checks and the host credential. Development only |
| `POND_DEV_INSECURE_LAN=1` | Debug builds only: also serve the companion API over plaintext HTTP on all interfaces, port `POND_DEV_INSECURE_LAN_PORT` (default 4080). See [auth-network-posture.md](auth-network-posture.md) |
| `POND_CORS_ALLOWED_ORIGINS` | Extra CORS origins, comma-separated |
| `GIAP_DEV_SERVER` | The Vite dev server origin to trust (`npm run dev:electron` sets it) |
| `POND_DEVICE_PROFILE` | Make the server take a board's decisions on another machine; see `scripts/jetson-emu.sh doctor` |
| `POND_DISABLE_PREWARM=1` | Skip the prompt-prefix warm-up |
| `GIAP_LLAMAFILE_PORT` | Port of the llamafile or llama-server subprocess (default 8080) |
| `POND_CREDENTIALS_URL` | The credentials service for Uber and Apple Music sign-in; `off` turns it off |
| `GIAP_RIDE_POLL_SECS` | How often a ride under way is read (default 15, at least 5) |
