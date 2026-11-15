# Contributing to Goose In A Pond

Goose In A Pond (GIAP) is a privacy-first, on-device home assistant built by
[Jarida](https://jarida.io) on [Goose](https://github.com/aaif-goose/goose). This file is the
contract for build, test, comment and commit conventions; [AGENTS.md](../AGENTS.md) points coding
agents here.

## Before you start

- Check [open issues](https://github.com/jarida-io/goose-in-a-pond/issues) and
  [pull requests](https://github.com/jarida-io/goose-in-a-pond/pulls) for duplicate work.
- Open an issue first for a large change.
- Read [Architecture rules](#architecture-rules) below. Work on any user-facing surface starts at
  [DESIGN.md](../DESIGN.md).

## Setup

### Prerequisites

| Tool | Version | Why |
|---|---|---|
| Rust | `rust-version = "1.91.1"` (root `Cargo.toml`); CI uses current stable | the workspace |
| Node | `.nvmrc` = 22; `pond-desktop/package.json` `engines`: `^22.13.0 \|\| ^24.0.0 \|\| >=26.0.0` | desktop app, web UI, tests. `bash scripts/giap.sh node` finds or downloads one in range |
| Go | `go 1.27.4` (`native/pondnet/go.mod`) | the `pondnet` remote-access helper (`scripts/build-network-helper.sh`) |
| C toolchain, cmake, pkg-config | any recent | whisper-rs and llama.cpp build from source |
| Linux only | `libasound2-dev libdbus-1-dev libssl-dev` | ALSA (cpal) and BlueZ (`pond-connect`) |
| `protoc` | any | only for `pond-adapters-lightning` (its Breez SDK compiles `.proto` files) |
| Git credentials for `jarida-io` | read access to `jarida-io/llama-cpp-rs-giap` | see below |

`llama-cpp-2` and `llama-cpp-sys-2` resolve to the private `jarida-io/llama-cpp-rs-giap` fork,
and Cargo resolves the whole workspace graph even for a one-crate build. Any git credential that
can read that repository works (`gh auth login`, a keychain entry, or an SSH key with a
`url."git@github.com:".insteadOf` rewrite). `.cargo/config.toml` sets
`net.git-fetch-with-cli = true` so Cargo uses git's credentials; without it Cargo's bundled
libgit2 reports the pinned rev as `revision ... not found`.

### Clone and build

```bash
git clone --recursive https://github.com/jarida-io/goose-in-a-pond.git
cd goose-in-a-pond
bash scripts/giap.sh doctor        # what this host has and lacks; exits 1 on any FAIL
bash scripts/giap.sh build         # web UI, then a release pond-server with this host's features
```

`scripts/giap.sh` is the front door for install, build, update, deploy and service control; see
[the installation guide](developer/installation.md). For day-to-day work:

```bash
cd pond-desktop && npm ci
npm run dev             # Vite + `cargo run -p pond-server -- serve --native`
npm run dev:electron    # Vite + tsup watch + the Electron shell
```

The first Rust build compiles Goose (`goose/` submodule) and llama.cpp and takes a while. After a
submodule bump: `git submodule update --init --recursive`.

No crate uses sqlx's compile-time query macros, so no database is needed to build. The build
scripts set `SQLX_OFFLINE=true` anyway.

### npm lockfiles: check them with the npm CI uses

CI installs with `npm ci`, which refuses a lockfile that disagrees with `package.json` or leaves
a peer dependency unsatisfied, and npm 10 and npm 11 disagree on what counts. The Matter job runs
Node 20.19 (npm 10), so check a lock with that npm:

```bash
npx npm@10 ci --ignore-scripts --no-audit --no-fund
```

When `npm ci` reports `ERESOLVE` over a peer range that a newer version of a transitive
dependency would satisfy, a plain relock often cannot fix it: npm keeps a locked transitive
version while it still fits its own range. Remove only the offending entries from
`package-lock.json` and let npm re-resolve them within their ranges:

```bash
node -e 'const f="package-lock.json",l=require("./"+f);for(const k of Object.keys(l.packages))if(/node_modules\/(react-aria|@adobe\/react-spectrum)$/.test(k))delete l.packages[k];require("fs").writeFileSync(f,JSON.stringify(l,null,2)+"\n")'
npx npm@10 install --package-lock-only --ignore-scripts
```

Compare the lock before and after; only the family you removed should move. Do not use
`--legacy-peer-deps` or `--force`: both hide the conflict.

## Development workflow

1. Branch from `main` (`feat/…`, `fix/…`, `docs/…`).
2. Write the test first ([testing guide](testing/tdd_guide.md)).
3. Implement, then run the checks for what you touched (next section).
4. Run `scripts/live-test.sh` for any change to a migration, a route, a handler or startup
   wiring. CI runs it only when the diff touches those paths and the private-fork secret exists.
5. Push and open a pull request against `main`.

Test on the Jetson first. Push the branch to the device's `origin`, deploy it with
`bash scripts/jetson.sh deploy --branch <branch>`, and run the checks and measurements there. Run
them on a Mac afterwards only when the maintainer asks. Stop the pond before building on the
device, and do not measure while anything else builds there. Without a Jetson, test on a Linux
box or a Mac.

## Checks

These mirror `.github/workflows/ci.yml`, which is the source of truth. `bash scripts/giap.sh check`
runs them for the areas your branch changes (`--all` for every area, `--dry-run` to see the
commands); the sections below are what it runs. New contributors start at the
[quick start](QUICKSTART.md).

### Rust

```bash
cargo fmt --check

# "Fast crates": nothing here pulls the Goose submodule or a heavy native library.
FAST="-p pond-voice -p pond-audio -p pond-audio-out -p pond-adapters-audio-devices -p pond-ear \
  -p pond-adapters-ear -p pond-core -p pond-infra -p pond-infra-scheduler -p pond-api \
  -p pond-adapters-llamafile -p pond-adapters-ollama -p pond-adapters-whisper -p pond-adapters-matter \
  -p pond-adapters-vision \
  -p pond-adapters-weather -p pond-adapters-uber -p pond-adapters-caldav -p pond-adapters-imap \
  -p pond-adapters-mcp-memory -p pond-mesh-protocol -p pond-mcp-server -p pond-hf-cache \
  -p pond-net -p pond-connect"
cargo clippy --locked $FAST -p pond-adapters-mistralrs
cargo test   --locked $FAST

# The production binary and every test target of it (compiles Goose; slow when cold).
cargo check --locked -p pond-server -p pond-adapters-goose --all-targets
cargo check --locked -p pond-server --features mesh --all-targets
```

CI also checks the quarantined `pond-agent`/`pond-inference` pair, `--features mistralrs-agent`,
and runs clippy and tests for `pond-adapters-vision-onnx`, `pond-adapters-mesh-libp2p`,
`pond-adapters-mesh-inference` and `pond-adapters-lightning` separately. Run those when you touch
them. CI runs clippy without `-D warnings`; fix new warnings anyway.

`cargo check` does not compile `#[cfg(test)]` code, so a removal it accepts can still break
`cargo test`. Run the tests before calling a deletion done.

`.cargo/config.toml` sets `-C target-cpu=native` for the Jetson. CI sets `RUSTFLAGS=""` because
native-CPU artifacts cached on one runner crash (SIGILL) on another; do the same for any cross or
containerised build.

### Desktop (`pond-desktop/`)

```bash
npm run typecheck      # tsc over src/ and electron/
npm run lint           # ESLint; every enabled rule is an error
npm test               # Vitest: renderer (happy-dom) and main (node) projects
npx playwright test    # E2E against Vite with page.route() mocks; no pond-server needed
```

`src/no-emoji.test.ts` fails the build on any emoji in `src/` or `electron/`, comments included.
Use `lucide-react` icons for functional icons.

### Other jobs

| Job | Run locally |
|---|---|
| `embedded-network` (Go) | `cd native/pondnet && test -z "$(gofmt -l .)" && go test -race ./... && go vet ./...` |
| `shellcheck` | `git ls-files -z '*.sh' ':!:goose/' \| xargs -0 shellcheck --severity=warning` (CI pins 0.11.0) |
| `node-setup`, `jetson-remote`, `setup-code`, `connect-setup`, `litert-setup`, `onnxruntime-setup` | `bash scripts/lib/<name>.test.sh` |
| `model-pairings` | `python3 -m unittest discover -s scripts/models -p 'test_*.py'` and `bash scripts/lib/model-pairings.test.sh` |
| `matter-server` | `cd matter-server && npm ci && npx tsc --noEmit && npx vitest run` |
| `music-extension` | `cd extensions/music && npm ci && npm run typecheck && npm test` |
| `desktop-smoke` | `cd pond-desktop && npm run build && npm run build:main && node scripts/smoke.mjs` |

`security.yml` separately runs `cargo audit`, `npm audit`, OSV over every lockfile, govulncheck
and gitleaks.

### Lint rules

- **shellcheck.** Covers every tracked `*.sh` outside `goose/`. The root `.shellcheckrc` makes it
  follow `source`d files. Warnings and errors fail the job; notes do not. For a real false
  positive put `# shellcheck disable=SCxxxx  # reason` on the line above; no file-wide disables.
- **ESLint** (`pond-desktop/eslint.config.js`) covers `src/`, `electron/`, `tests/` and the
  package-root tooling. Enabled: the recommended JavaScript and typescript-eslint sets,
  `react-hooks/rules-of-hooks`, `react-hooks/exhaustive-deps` and
  `react-refresh/only-export-components`. Unused code is an error: remove it rather than prefix
  it with `_`. The exception is a parameter an interface requires and one implementation does
  not read, named `_x`.
  - `react-refresh/only-export-components` keeps Fast Refresh working: a module that exports a
    component exports nothing else. Put helpers, hooks and contexts in a sibling `.ts` module
    (`useConfirm.ts` beside `ConfirmDialog.tsx`).
  - Off: the React Compiler rules in `eslint-plugin-react-hooks`' recommended set, and
    `require-yield` in `src/**/*.test.*` (mock streams that throw before their first frame).

## Architecture rules

GIAP is hexagonal; dependencies point inward to `pond-core`.

- **`pond-core` stays pure.** No `goose`, `sqlx`, `axum`, `reqwest` or other HTTP, database or
  filesystem framework. It is organised by bounded context (`models`, `mcp`, `user_data`,
  `security`, `shared`, `mesh`, `network`, `context`, `rides`), each with `domain`, `ports`,
  `services` and `mocks`. A new external capability is a trait in `<context>/ports/` and an
  implementation in an adapter crate. See
  [Creating ports and adapters](developer/creating-ports-and-adapters.md).
- **Goose-dependent crates** (`pond-adapters-goose`, `pond-adapters-local-inference`,
  `pond-mcp-server`) are ordinary workspace members. They must stay off the fast-crate lists in
  `ci.yml`, which exist to prove the rest builds without Goose.
- **`ChatService`** (`crates/pond-core/src/shared/services/chat.rs`) owns turn persistence.
  Handlers do not persist turns themselves. Memory extraction is not part of a turn: it runs in
  batches on the inference lane (`BatchExtractionService` in
  `crates/pond-core/src/user_data/services/memory_extraction.rs`).
- **Settings** are one key-value row per field. A new field needs both an `upsert!` in
  `update_fields` and an arm in `apply_key`; `roundtrip_persists_every_field` and
  `every_settings_field_is_dispositioned` fail the build otherwise.
- **The model does the tool selection.** No keyword or intent pre-classification in front of the
  main model; it calls MCP tools natively.

## Comments

Most code needs no comment. Write one only for what a reader cannot get from the code: a
constraint, an invariant, a non-obvious reason, or a quirk of a dependency (Goose, llama.cpp,
SQLite, Matter).

- **Keep it short.** One line where possible. A doc comment is a one-sentence summary plus any
  constraint the caller must know.
- **No history.** No "used to", dates, commit hashes, PR or issue numbers, phase or workstream
  tags (`PAI-7 P4`, `Phase F2`), or incident write-ups. Those go in the commit message and the
  pull request.
- **Don't narrate.** Don't restate the code (`// increment the retry count` above
  `retries += 1`), don't narrate your reasoning, and don't leave notes for the reviewer
  ("fixed X", "as requested", "new:").
- **Keep it true.** When you change code, fix or delete the comments it makes wrong.

```rust
// Bad: history and narrative
// Phase F2: attachments used to be inlined as base64 until a routine history
// read reached megabytes on 2026-09-12 (#nnn), so now they're referenced.

// Good: the constraint
// Referenced by URL, never inlined: base64 would make a history read megabytes.
```

Some comments are read by programs as well as people. Change them only when you mean to change
what the program reads:

- clap doc comments on CLI arguments are the `--help` text;
- `JsonSchema` field docs in `pond-mcp-server` are the tool descriptions the model sees;
- directives such as `// SAFETY:`, `@ts-expect-error`, `eslint-disable` and `/// <reference>`;
- code blocks in Rust doc comments, which run as doctests.

Several tests read source files as text: the `*_is_wired.rs` and `*_wiring.rs` tests,
`crates/pond-core/tests/egress_guard.rs` and `crates/pond-api/tests/stream_handler_parity.rs`.
Run the tests of the crates you touched after editing comments.

## Commit Messages

Use the conventional commit format:

```
<type>(<scope>): <short summary>

<body: optional, wrapped at 72 columns, saying why>
```

Types: `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `build`, `ci`, `chore`, `style`.
The scope is the crate or area without its `pond-` prefix (`api`, `whisper`, `desktop`,
`scripts`).

```
feat(api): add DELETE /api/v1/memories/:id endpoint
fix(whisper): handle an empty transcript from the in-process model
test(ollama): add HTTP 500 error path integration test
docs(contributing): list the CI jobs and how to run each locally
```

No AI attribution: no `Co-Authored-By:` trailer for an AI tool or model, no "Generated with"
line and no session link, in commits, pull request descriptions or review comments. Human
co-authors are fine.

## Pull request checklist

- [ ] `cargo fmt --check`, and clippy and tests for the crates you touched pass
- [ ] `cargo check -p pond-server -p pond-adapters-goose --all-targets` passes if you touched a
      crate the production binary uses
- [ ] `npm run typecheck`, `npm run lint` and `npm test` pass in `pond-desktop` if you touched it
- [ ] `shellcheck` is clean on any script you touched
- [ ] `scripts/live-test.sh` passes if you touched a migration, route, handler or startup wiring
- [ ] New ports have a mock and tests in `pond-core`; `pond-core` has no new framework dependency
- [ ] Tests that need hardware, a model or the network are `#[ignore = "<what it needs>"]`
- [ ] Comments follow [the comment rules](#comments); commits follow
      [the commit rules](#commit-messages)

## Licensing of contributions

By submitting a contribution you agree that it is licensed under the
[Apache License 2.0](../LICENSE) (source code) and
[CC BY 4.0](http://creativecommons.org/licenses/by/4.0/) (documentation and media), and you
confirm you have the right to submit it.

## Code of conduct

Be respectful, inclusive and professional. Harassment, discrimination or abuse is not tolerated.

## Questions

Open an issue or write to [info@jarida.io](mailto:info@jarida.io).
